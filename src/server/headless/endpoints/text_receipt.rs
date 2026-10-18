//! While receipt is pending, only text and metadata may advance.
use super::*;

impl EndpointClient {
    pub(super) fn encode_surface(&self, surface: &wire::PaneSurfaceFrame) -> wire::ServerMessage {
        let mut message = wire::ServerMessage::PaneSurface(surface.clone());
        let delta = self
            .surface_delta
            .then_some(self.surface.as_ref())
            .flatten()
            .and_then(|last| {
                crate::protocol::surface_delta::message(last, &mut message)
                    .map_err(|error| warn!(%error, "failed to encode surface delta"))
                    .ok()
                    .flatten()
            });
        let reused = if let wire::ServerMessage::PaneSurface(next) = &mut message {
            (delta.is_none() && self.surface_reuse)
                .then_some(self.surface.as_ref())
                .flatten()
                .filter(|last| {
                    last.boot_id == next.boot_id
                        && last.frame == next.frame
                        && next.popup.is_none()
                        && next.graphics.assets.is_empty()
                })
                .and_then(|last| {
                    crate::protocol::surface_reuse::message(last.surface_revision, next)
                        .map_err(|error| warn!(%error, "failed to encode surface reuse"))
                        .ok()
                        .flatten()
                })
        } else {
            None
        };
        delta.or(reused).unwrap_or(message)
    }
}

pub(super) fn same_placement_geometry(
    a: &wire::SurfaceGraphicsPlacement,
    b: &wire::SurfaceGraphicsPlacement,
) -> bool {
    let mut a = a.clone();
    a.asset.data_fingerprint = b.asset.data_fingerprint;
    a.asset.data_len = b.asset.data_len;
    a == *b
}

fn same_pane_geometry(a: &wire::PaneSurfacePane, b: &wire::PaneSurfacePane) -> bool {
    a.pane_id == b.pane_id
        && a.rect == b.rect
        && a.inner_rect == b.inner_rect
        && a.scrollbar_rect == b.scrollbar_rect
        && a.pixel_width == b.pixel_width
        && a.pixel_height == b.pixel_height
        && a.scroll.map(|s| (s.offset_from_bottom, s.viewport_rows))
            == b.scroll.map(|s| (s.offset_from_bottom, s.viewport_rows))
}

pub(super) fn same_popup_geometry(
    a: Option<&wire::ClientShellPopupSurface>,
    b: Option<&wire::ClientShellPopupSurface>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.terminal_id == b.terminal_id
                && a.width == b.width
                && a.height == b.height
                && a.frame.width == b.frame.width
                && a.frame.height == b.frame.height
                && a.pixel_width == b.pixel_width
                && a.pixel_height == b.pixel_height
        }
        _ => false,
    }
}

fn compatible(previous: &wire::PaneSurfaceFrame, next: &wire::PaneSurfaceFrame) -> bool {
    previous.boot_id == next.boot_id
        && previous.frame.width == next.frame.width
        && previous.frame.height == next.frame.height
        && previous.panes.len() == next.panes.len()
        && previous
            .panes
            .iter()
            .zip(&next.panes)
            .all(|(a, b)| same_pane_geometry(a, b))
        && previous.splits == next.splits
        && same_popup_geometry(previous.popup.as_deref(), next.popup.as_deref())
        && previous.graphics.placements.len() == next.graphics.placements.len()
        && previous
            .graphics
            .placements
            .iter()
            .zip(&next.graphics.placements)
            .all(|(a, b)| same_placement_geometry(a, b))
}

fn popup_position(facts: &PopupReadFacts) -> (&str, u64, u64) {
    (
        &facts.terminal_id,
        facts.scroll.offset_from_bottom,
        facts.scroll.viewport_rows,
    )
}

impl HeadlessServer {
    pub(super) fn stream_endpoint_text_during_receipt(
        &mut self,
        client_id: u64,
        resources: &crate::api::schema::SessionSnapshot,
    ) {
        let client = self.endpoint_clients.get(&client_id).expect("live client");
        #[cfg(unix)]
        if client.direct_graphics {
            return;
        }
        if !client.active {
            return;
        }
        let (Some(previous), Some(previous_snapshot)) =
            (client.surface.as_ref(), client.snapshot.as_ref())
        else {
            return;
        };
        if previous.boot_id != self.endpoint_boot_id
            || previous.frame.width != client.size.cols
            || previous.frame.height != client.size.rows
        {
            return;
        }
        let mut snapshot = crate::server::endpoint_snapshot::snapshot(
            &self.app,
            &self.endpoint_boot_id,
            client.projection_revision,
            self.server_config_diagnostic_without_keybindings.as_deref(),
            &client.location,
            Vec::new(),
            resources.clone(),
        );
        if snapshot.focused_tab_id != previous_snapshot.focused_tab_id {
            return;
        }
        let mut jobs = crate::server::endpoint_jobs::projection(
            &self.app.state,
            &self.endpoint_boot_id,
            client.projection_revision,
        );
        let projection_revision =
            if client.snapshot.as_ref() != Some(&snapshot) || client.jobs.as_ref() != Some(&jobs) {
                client.projection_revision.saturating_add(1)
            } else {
                client.projection_revision
            };
        snapshot.revision = projection_revision;
        jobs.revision = projection_revision;
        let target = self.endpoint_target(client_id);
        let popup_before = self.popup_read_facts(client_id);
        if (previous.popup.is_some() && client.popup_read_facts.is_none())
            || client.popup_read_facts.as_ref().map(popup_position)
                != popup_before.as_ref().map(popup_position)
        {
            return;
        }
        // Do not resize shared PTYs while the previous scene is still in flight.
        let Ok(rendered) = crate::server::endpoint_surface::render_text_surface(
            &self.app,
            target,
            Rect::new(0, 0, client.size.cols, client.size.rows),
            false,
            client.cell_size,
        ) else {
            return;
        };
        if popup_before != self.popup_read_facts(client_id) {
            return;
        }
        let delivered = client.graphics_delivery.with_large_images(false, true);
        let (graphics, _) = crate::kitty_graphics::endpoint_scene::collect(
            &self.app,
            target,
            &rendered.panes,
            rendered.popup.as_deref(),
            client.cell_size,
            &delivered,
        );
        let mut surface = wire::PaneSurfaceFrame {
            boot_id: self.endpoint_boot_id.clone(),
            projection_revision,
            surface_revision: client.surface_revision,
            frame: rendered.frame,
            panes: rendered.panes,
            splits: rendered.splits,
            popup: rendered.popup,
            graphics,
        };
        if !compatible(previous, &surface) {
            return;
        }
        surface.graphics.assets.clear();
        surface
            .graphics
            .placements
            .clone_from(&previous.graphics.placements);
        surface
            .graphics
            .retained_assets
            .clone_from(&previous.graphics.retained_assets);
        let mut baseline = previous.clone();
        baseline.graphics.assets.clear();
        if surface == baseline
            && !client.write_pending
            && client.published_projection_revision == Some(snapshot.revision)
        {
            return;
        }
        surface.surface_revision = surface.surface_revision.saturating_add(1);
        let snapshot_message = match crate::protocol::endpoint::snapshot_message(&snapshot) {
            Ok(m) => m,
            Err(_) => return,
        };
        let Ok(job_data) = serde_json::to_string(&jobs) else {
            return;
        };
        let messages = [
            snapshot_message,
            wire::ServerMessage::EndpointControl {
                kind: crate::protocol::endpoint_jobs::JOBS_PROJECTION_KIND.into(),
                data: job_data,
            },
            client.encode_surface(&surface),
        ];
        let mut batch = Vec::new();
        for message in messages {
            let Ok(data) = framed(&message) else {
                return;
            };
            batch.extend(data);
        }
        let bytes = crate::logging::latency_trace_enabled().then_some(batch.len());
        let client = self
            .endpoint_clients
            .get_mut(&client_id)
            .expect("live client");
        if client.writer.render.send_ordered(batch).is_err() {
            return;
        }
        client.projection_revision = projection_revision;
        client.snapshot = Some(snapshot);
        client.jobs = Some(jobs);
        client.surface_revision = surface.surface_revision;
        client.surface = Some(surface);
        client.popup_read_facts = popup_before;
        client.published_projection_revision = Some(projection_revision);
        client.write_pending = false;
        if let Some(bytes) = bytes {
            tracing::info!(target: "herdr_latency", event = "surface.queued",
                time_us = crate::logging::latency_trace_time_us(), pid = std::process::id(),
                boot_id = %self.endpoint_boot_id, client_id, surface_revision = client.surface_revision,
                bytes, with_images = false, text_only = true,
                "text-only surface queued while graphics receipt remains pending");
        }
    }
}
