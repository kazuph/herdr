//! Stable endpoint connections share server resources but own their viewer location.

use super::*;
use crate::protocol::endpoint_wire::{self as wire, ClientMessage, ClientPaneInputEvent};
use crate::server::client_transport::ClientWriter;
use crate::server::client_view::{ClientShellLocation, ClientShellTopology};
use crate::server::endpoint_transport::EndpointTransportEvent;

mod lifecycle;
#[cfg(unix)]
pub(super) mod native_graphics;
mod pane_graphics;

pub(super) struct EndpointClient {
    writer: ClientWriter,
    pub(super) location: ClientShellLocation,
    size: wire::ClientSurfaceSize,
    cell_size: crate::kitty_graphics::HostCellSize,
    graphics_delivery: crate::kitty_graphics::endpoint_scene::DeliveryCache,
    active: bool,
    surface_reuse: bool,
    surface_delta: bool,
    #[cfg(unix)]
    direct_graphics: bool,
    pixel_mouse: bool,
    mouse_capture: bool,
    outer_focus: Option<bool>,
    last_activity: u64,
    presentation_committed: bool,
    sent_presentation: Option<(bool, bool, bool)>,
    held_inputs: crate::server::endpoint_input::HeldInputs,
    projection_revision: u64,
    surface_revision: u64,
    snapshot: Option<crate::protocol::endpoint_projection::SnapshotJson>,
    jobs: Option<crate::protocol::endpoint_jobs::EndpointJobsProjection>,
    pub(super) surface: Option<wire::PaneSurfaceFrame>,
    popup_read_facts: Option<PopupReadFacts>,
    write_pending: bool,
    published_projection_revision: Option<u64>,
    editors: Vec<EndpointEditor>,
    staged_clipboard_files: Vec<std::path::PathBuf>,
}

#[cfg(test)]
impl EndpointClient {
    pub(super) fn staged_clipboard_files(&self) -> &[std::path::PathBuf] {
        &self.staged_clipboard_files
    }
}

impl Drop for EndpointClient {
    fn drop(&mut self) {
        crate::server::clipboard_image::remove_files(std::mem::take(
            &mut self.staged_clipboard_files,
        ));
    }
}

struct EndpointEditor {
    pane_id: crate::layout::PaneId,
    public_pane_id: String,
    previous_pane: String,
    tab_id: String,
    workspace_index: usize,
    tab_index: usize,
    previous_zoomed: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct PopupReadFacts {
    terminal_id: String,
    content_revision: u64,
    scroll: api::schema::PaneScrollInfo,
}

pub(crate) fn supported_methods() -> &'static [&'static str] {
    &[
        "session.snapshot",
        "agent.restore",
        "server.stop",
        "server.restart",
        "server.reload_config",
        "server.pane_history.get",
        "server.pane_history.set",
        "client_shell.surface.set",
        "worktree.list",
        "worktree.create",
        "worktree.open",
        "worktree.remove",
        "workspace.focus",
        "tab.focus",
        "pane.focus",
        "workspace.create",
        "workspace.duplicate",
        "workspace.list",
        "workspace.get",
        "workspace.rename",
        "workspace.set_section",
        "workspace.move",
        "tab.create",
        "tab.list",
        "tab.get",
        "tab.rename",
        "tab.move",
        "pane.get",
        "pane.scrollback.edit",
        "pane.command.execute",
        "pane.agent.start",
        "pane.rename",
        "pane.scroll",
        "pane.selection.read",
        "pane.copy_motion",
        "pane.copy_search",
        "pane.clear",
        "pane.zoom",
        "pane.swap",
        "pane.arrange",
        "pane.resize",
        "layout.set_split_ratio",
        "pane.split",
        "pane.close",
        "popup.close",
        "popup.get",
        "popup.selection.read",
        "popup.scroll",
        "run.log.open",
        "tab.close",
        "workspace.close",
    ]
}

pub(super) fn new_boot_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn framed(message: &wire::ServerMessage) -> Result<Vec<u8>, protocol::FramingError> {
    let mut data = Vec::new();
    protocol::write_message(&mut data, message)?;
    Ok(data)
}

impl HeadlessServer {
    #[cfg(test)]
    pub(super) fn endpoint_measurement_barrier(&self, client_id: u64) {
        let message = wire::ServerMessage::EndpointControl {
            kind: "test.measurement.barrier".into(),
            data: String::new(),
        };
        self.endpoint_clients[&client_id]
            .writer
            .control
            .send_after_render(framed(&message).unwrap())
            .unwrap();
    }

    fn endpoint_topology(&self) -> ClientShellTopology {
        let resources = self.app.session_snapshot();
        ClientShellTopology {
            focused_pane_ids: resources
                .tabs
                .iter()
                .filter_map(|tab| {
                    let (workspace, index) = self.app.parse_tab_id(&tab.tab_id)?;
                    let pane = self
                        .app
                        .state
                        .workspaces
                        .get(workspace)?
                        .tabs
                        .get(index)?
                        .layout
                        .focused();
                    Some((
                        tab.tab_id.clone(),
                        self.app.public_pane_id(workspace, pane)?,
                    ))
                })
                .collect(),
            pane_tab_ids: resources
                .panes
                .iter()
                .map(|pane| (pane.pane_id.clone(), pane.tab_id.clone()))
                .collect(),
            focused_workspace_id: resources.focused_workspace_id,
            fallback_workspace_id: resources
                .workspaces
                .first()
                .map(|workspace| workspace.workspace_id.clone()),
            active_tab_ids: resources
                .workspaces
                .into_iter()
                .map(|workspace| (workspace.workspace_id, workspace.active_tab_id))
                .collect(),
            tab_workspace_ids: resources
                .tabs
                .into_iter()
                .map(|tab| (tab.tab_id, tab.workspace_id))
                .collect(),
        }
    }

    fn endpoint_target(&self, client_id: u64) -> Option<crate::ui::tab_surface::TabSurfaceTarget> {
        let tab_id = self
            .endpoint_clients
            .get(&client_id)?
            .location
            .focused_tab_id()?;
        let (workspace_index, tab_index) = self.app.parse_tab_id(tab_id)?;
        Some(crate::ui::tab_surface::TabSurfaceTarget {
            workspace_index,
            tab_index,
            pane_focus: self
                .endpoint_clients
                .get(&client_id)?
                .location
                .focused_pane_id()
                .and_then(|pane| self.app.parse_pane_id(pane))
                .filter(|(workspace, pane)| {
                    *workspace == workspace_index
                        && self.app.state.workspaces[workspace_index].find_tab_index_for_pane(*pane)
                            == Some(tab_index)
                })
                .map(|(_, pane)| pane),
        })
    }

    pub(super) fn handle_endpoint_event(&mut self, event: EndpointTransportEvent) -> bool {
        if self.shutting_down || self.handoff_in_progress {
            return false;
        }
        match event {
            EndpointTransportEvent::Connected {
                client_id,
                hello,
                writer,
            } => {
                let last_activity = self.allocate_activity_stamp();
                let mut location = ClientShellLocation::default();
                location.reconcile(&self.endpoint_topology());
                self.endpoint_clients.insert(
                    client_id,
                    EndpointClient {
                        writer,
                        location,
                        size: hello.surface_size,
                        cell_size: crate::kitty_graphics::HostCellSize {
                            width_px: hello.cell_width_px,
                            height_px: hello.cell_height_px,
                        },
                        active: hello.surface_active,
                        surface_reuse: hello.surface_reuse,
                        surface_delta: hello.surface_reuse && hello.surface_delta,
                        #[cfg(unix)]
                        direct_graphics: hello.direct_graphics,
                        graphics_delivery: Default::default(),
                        pixel_mouse: hello.pixel_mouse,
                        mouse_capture: hello.mouse_capture,
                        outer_focus: None,
                        last_activity,
                        presentation_committed: false,
                        sent_presentation: None,
                        held_inputs: Default::default(),
                        projection_revision: 0,
                        surface_revision: 0,
                        snapshot: None,
                        jobs: None,
                        surface: None,
                        write_pending: true,
                        published_projection_revision: None,
                        popup_read_facts: None,
                        editors: Vec::new(),
                        staged_clipboard_files: Vec::new(),
                    },
                );
                true
            }
            EndpointTransportEvent::Disconnected { client_id } => {
                self.set_endpoint_surface_active(client_id, false);
                self.endpoint_tab_geometry
                    .retain(|_, controller| *controller != client_id);
                #[cfg(unix)]
                self.disconnect_endpoint_native(client_id);
                self.endpoint_clients.remove(&client_id).is_some()
            }
            EndpointTransportEvent::WriterDrained { client_id } => self
                .endpoint_clients
                .get(&client_id)
                .is_some_and(|client| client.write_pending),
            EndpointTransportEvent::ApiResponse(completed) => {
                if !self.endpoint_clients.contains_key(&completed.client_id) {
                    return false;
                }
                let response = self.project_endpoint_api_response(
                    completed.client_id,
                    completed.response,
                    completed.create_focus,
                    false,
                );
                let id = serde_json::from_str::<serde_json::Value>(&response)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                self.send_endpoint_response(completed.client_id, &id, response);
                true
            }
            EndpointTransportEvent::Message { client_id, message } => {
                if !self.endpoint_clients.contains_key(&client_id) {
                    return false;
                }
                match message {
                    ClientMessage::ClipboardImage {
                        target,
                        extension,
                        data,
                    } => self.apply_endpoint_clipboard_image(client_id, target, &extension, &data),
                    #[cfg(unix)]
                    ClientMessage::GraphicsTransmissionStarted {
                        transfer_id,
                        image_id,
                    } => {
                        self.endpoint_native_started(client_id, transfer_id, image_id)
                            | self.endpoint_pane_direct_started(client_id, transfer_id, image_id)
                    }
                    #[cfg(unix)]
                    ClientMessage::GraphicsTransmissionResult {
                        transfer_id,
                        image_id,
                        success,
                    } => {
                        self.endpoint_native_result(client_id, transfer_id, image_id, success)
                            | self.endpoint_pane_direct_result(
                                client_id,
                                transfer_id,
                                image_id,
                                success,
                            )
                    }
                    ClientMessage::ClientShellResize {
                        surface_size,
                        cell_width_px,
                        cell_height_px,
                        pixel_mouse,
                    } => {
                        let client = self
                            .endpoint_clients
                            .get_mut(&client_id)
                            .expect("checked client");
                        if cell_width_px
                            .checked_mul(u32::from(surface_size.cols))
                            .is_none()
                            || cell_height_px
                                .checked_mul(u32::from(surface_size.rows))
                                .is_none()
                        {
                            self.endpoint_error(
                                client_id,
                                "invalid endpoint pixel geometry".into(),
                            );
                            return false;
                        }
                        client.size = surface_size;
                        client.pixel_mouse = pixel_mouse;
                        client.cell_size = crate::kitty_graphics::HostCellSize {
                            width_px: cell_width_px,
                            height_px: cell_height_px,
                        };
                        client.surface = None;
                        self.claim_endpoint_geometry(client_id);
                        true
                    }
                    ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                        self.handle_endpoint_request(client_id, &boot_id, &request);
                        true
                    }
                    ClientMessage::ClientShellPaneInput { pane_id, events } => {
                        self.apply_endpoint_pane_input(client_id, &pane_id, events)
                    }
                    ClientMessage::ClientShellPopupInput {
                        terminal_id,
                        events,
                    } => self.apply_endpoint_popup_input(client_id, &terminal_id, events),
                    ClientMessage::ClientShellFocus { focused } => {
                        self.set_endpoint_focus(client_id, focused)
                    }
                    ClientMessage::ClientShellMouseCapture { enabled } => {
                        let client = self
                            .endpoint_clients
                            .get_mut(&client_id)
                            .expect("checked client");
                        let changed = client.mouse_capture != enabled;
                        client.mouse_capture = enabled;
                        changed
                    }
                    ClientMessage::EndpointControl { kind, data }
                        if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND =>
                    {
                        self.sync_endpoint_presentation(client_id, Some(data))
                    }
                    ClientMessage::ClientShellHostTheme { .. }
                    | ClientMessage::EndpointControl { .. } => false,
                    _ => false,
                }
            }
        }
    }

    pub(super) fn endpoint_graphics_pending(&self) -> bool {
        self.endpoint_clients
            .values()
            .any(|client| client.active && client.graphics_delivery.has_pending())
    }

    /// A tab shown by a presenting endpoint viewer whose terminal is not known to be unfocused
    /// has been seen. Endpoint viewers navigate with their own location instead of the server's
    /// active workspace, so finished agents there would otherwise stay "done, unseen" forever.
    fn mark_endpoint_viewed_tabs_seen(&mut self) {
        let viewed = self
            .endpoint_clients
            .values()
            .filter(|client| client.active && client.outer_focus != Some(false))
            .filter_map(|client| client.location.focused_tab_id())
            .filter_map(|tab| self.app.parse_tab_id(tab))
            .collect::<Vec<_>>();
        for (ws_idx, tab_idx) in viewed {
            if let Some(tab) = self
                .app
                .state
                .workspaces
                .get_mut(ws_idx)
                .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
            {
                for pane in tab.panes.values_mut() {
                    pane.seen = true;
                }
            }
        }
    }

    pub(super) fn stream_endpoint_views(&mut self) {
        let topology = self.endpoint_topology();
        for client in self.endpoint_clients.values_mut() {
            client.location.reconcile(&topology);
        }
        self.mark_endpoint_viewed_tabs_seen();
        self.endpoint_tab_geometry.retain(|tab, owner| {
            topology.tab_workspace_ids.contains_key(tab)
                && self.endpoint_clients.get(owner).is_some_and(|client| {
                    client.active && client.location.focused_tab_id() == Some(tab)
                })
        });
        let mut clients = self.endpoint_clients.keys().copied().collect::<Vec<_>>();
        clients.sort_unstable();
        for client_id in clients {
            let client = self
                .endpoint_clients
                .get_mut(&client_id)
                .expect("live client");
            let mut snapshot = crate::server::endpoint_snapshot::snapshot(
                &self.app,
                &self.endpoint_boot_id,
                client.projection_revision,
                self.server_config_diagnostic_without_keybindings.as_deref(),
                &client.location,
                Vec::new(),
            );
            let mut jobs = crate::server::endpoint_jobs::projection(
                &self.app.state,
                &self.endpoint_boot_id,
                client.projection_revision,
            );
            if client.snapshot.as_ref() != Some(&snapshot) || client.jobs.as_ref() != Some(&jobs) {
                client.projection_revision = client.projection_revision.saturating_add(1);
                snapshot.revision = client.projection_revision;
                jobs.revision = client.projection_revision;
                client.snapshot = Some(snapshot);
                client.jobs = Some(jobs);
                client.surface = None;
                client.write_pending = true;
            }
            let snapshot = client.snapshot.as_ref().expect("snapshot produced").clone();
            let target = self.endpoint_target(client_id);
            let client = self.endpoint_clients.get(&client_id).expect("live client");
            let active = client.active;
            let size = client.size;
            let cell_size = client.cell_size;
            if !active
                && !client.write_pending
                && client.published_projection_revision == Some(snapshot.revision)
            {
                continue;
            }
            let mut batch = Vec::new();
            let message = match crate::protocol::endpoint::snapshot_message(&snapshot) {
                Ok(message) => message,
                Err(error) => {
                    warn!(client_id, %error, "endpoint snapshot serialization failed");
                    continue;
                }
            };
            if let Ok(data) = framed(&message) {
                batch.extend(data);
            } else {
                continue;
            }
            let client = self.endpoint_clients.get(&client_id).expect("live client");
            let message = wire::ServerMessage::EndpointControl {
                kind: crate::protocol::endpoint_jobs::JOBS_PROJECTION_KIND.into(),
                data: match serde_json::to_string(client.jobs.as_ref().expect("jobs produced")) {
                    Ok(data) => data,
                    Err(error) => {
                        warn!(client_id, %error, "endpoint job projection serialization failed");
                        continue;
                    }
                },
            };
            if let Ok(data) = framed(&message) {
                batch.extend(data);
            } else {
                continue;
            }
            if active {
                let resize = snapshot.focused_tab_id.as_ref().is_some_and(|tab| {
                    *self
                        .endpoint_tab_geometry
                        .entry(tab.clone())
                        .or_insert(client_id)
                        == client_id
                });
                let (cols, rows) =
                    crate::server::client_transport::clamp_terminal_size(size.cols, size.rows);
                let popup_before = self.popup_read_facts(client_id);
                let rendered = match crate::server::endpoint_surface::render_text_surface(
                    &self.app,
                    target,
                    Rect::new(0, 0, cols, rows),
                    resize,
                    cell_size,
                ) {
                    Ok(rendered) => rendered,
                    Err(_) => continue,
                };
                if popup_before != self.popup_read_facts(client_id) {
                    continue;
                }
                let client = self.endpoint_clients.get(&client_id).expect("live client");
                let (graphics, graphics_delivery) = crate::kitty_graphics::endpoint_scene::collect(
                    &self.app,
                    target,
                    &rendered.panes,
                    rendered.popup.as_deref(),
                    cell_size,
                    &client.graphics_delivery,
                );
                let mut graphics = graphics;
                #[cfg(unix)]
                let mut graphics_delivery = graphics_delivery;
                let pane_prepared = self.prepare_endpoint_pane_graphics(client_id, &mut graphics);
                #[cfg(unix)]
                let prepared = {
                    if !self.endpoint_native.can_hold(client_id, &graphics) {
                        self.retire_endpoint_native(client_id);
                    }
                    if let Some((held, delivery)) = self.endpoint_native.hold(client_id, &graphics)
                    {
                        graphics = held;
                        graphics_delivery = delivery;
                        None
                    } else if pane_prepared.is_none()
                        && self
                            .endpoint_clients
                            .get(&client_id)
                            .is_some_and(|c| c.direct_graphics)
                    {
                        self.endpoint_native.prepare(
                            client_id,
                            &self.endpoint_boot_id,
                            &mut graphics,
                            &graphics_delivery,
                        )
                    } else {
                        None
                    }
                };
                let client = self.endpoint_clients.get(&client_id).expect("live client");
                #[cfg(unix)]
                tracing::debug!(
                    client_id,
                    direct_graphics = client.direct_graphics,
                    prepared = prepared.is_some(),
                    inline_assets = graphics.assets.len(),
                    "endpoint decoded native export prepared"
                );
                let mut surface = wire::PaneSurfaceFrame {
                    boot_id: self.endpoint_boot_id.clone(),
                    projection_revision: snapshot.revision,
                    surface_revision: client.surface_revision,
                    frame: rendered.frame,
                    panes: rendered.panes,
                    splits: rendered.splits,
                    popup: rendered.popup,
                    graphics,
                };
                if !client.write_pending
                    && client.surface.as_ref() == Some(&surface)
                    && client.popup_read_facts == popup_before
                    && client.published_projection_revision == Some(snapshot.revision)
                {
                    continue;
                }
                surface.surface_revision = surface.surface_revision.saturating_add(1);
                let mut message = wire::ServerMessage::PaneSurface(surface.clone());
                let delta = client
                    .surface_delta
                    .then_some(client.surface.as_ref())
                    .flatten()
                    .and_then(|last| {
                        crate::protocol::surface_delta::message(last, &mut message)
                            .map_err(|error| warn!(%error, "failed to encode surface delta"))
                            .ok()
                            .flatten()
                    });
                let reused = if let wire::ServerMessage::PaneSurface(next) = &mut message {
                    (delta.is_none() && client.surface_reuse)
                        .then_some(client.surface.as_ref())
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
                let Ok(data) = framed(&delta.or(reused).unwrap_or(message)) else {
                    continue;
                };
                batch.extend(data);
                if let Some((_, _, message)) = &pane_prepared {
                    let Ok(data) = framed(message) else {
                        continue;
                    };
                    batch.extend(data);
                }
                #[cfg(unix)]
                if let Some((_, message)) = &prepared {
                    let Ok(data) = framed(message) else {
                        continue;
                    };
                    batch.extend(data);
                }
                let client = self
                    .endpoint_clients
                    .get_mut(&client_id)
                    .expect("live client");
                if client.writer.render.send_ordered(batch).is_ok() {
                    if let Some((key, image_id, _)) = pane_prepared {
                        if let Some(gate) = self
                            .app
                            .pane_graphics
                            .slots
                            .get_mut(&key)
                            .and_then(|slot| slot.direct_gate.as_mut())
                        {
                            gate.endpoint_image_id = Some(image_id);
                        }
                    }
                    #[cfg(unix)]
                    {
                        let inline_assets = surface
                            .graphics
                            .assets
                            .iter()
                            .map(|a| a.key.clone())
                            .collect::<Vec<_>>();
                        self.endpoint_native.commit_scene(
                            client_id,
                            &surface.graphics,
                            &inline_assets,
                        );
                        if let Some((pending, _)) = prepared {
                            self.endpoint_native.commit(client_id, pending);
                        }
                    }
                    client.graphics_delivery = graphics_delivery;
                    client.surface_revision = surface.surface_revision;
                    client.surface = Some(surface);
                    client.popup_read_facts = popup_before;
                    client.published_projection_revision = Some(snapshot.revision);
                    client.write_pending = false;
                } else {
                    client.write_pending = true;
                }
            } else {
                let client = self
                    .endpoint_clients
                    .get_mut(&client_id)
                    .expect("live client");
                client.write_pending = client.writer.render.send_ordered(batch).is_err();
                if !client.write_pending {
                    client.published_projection_revision = Some(snapshot.revision);
                }
            }
        }
        for client_id in self.endpoint_clients.keys().copied().collect::<Vec<_>>() {
            self.sync_endpoint_presentation(client_id, None);
        }
    }

    fn endpoint_error(&self, client_id: u64, message: String) {
        if let Some(client) = self.endpoint_clients.get(&client_id) {
            if let Ok(data) = framed(&wire::ServerMessage::ClientShellError { message }) {
                let _ = client.writer.control.send(data);
            }
        }
    }

    fn popup_read_facts(&self, client_id: u64) -> Option<PopupReadFacts> {
        let target = self.endpoint_target(client_id)?;
        let popup = self
            .app
            .state
            .popup_pane_for_workspace(&self.app.state.workspaces[target.workspace_index].id)?;
        let runtime = self.app.terminal_runtimes.get(&popup.terminal_id)?;
        let content_revision = runtime.content_seq();
        if !content_revision.is_multiple_of(2) {
            return None;
        }
        let scroll = runtime.scroll_metrics()?;
        if runtime.content_seq() != content_revision {
            return None;
        }
        Some(PopupReadFacts {
            terminal_id: popup.terminal_id.to_string(),
            content_revision,
            scroll: api::schema::PaneScrollInfo {
                offset_from_bottom: scroll.offset_from_bottom as u64,
                max_offset_from_bottom: scroll.max_offset_from_bottom as u64,
                viewport_rows: scroll.viewport_rows as u64,
            },
        })
    }

    fn handle_endpoint_request(&mut self, client_id: u64, boot_id: &str, request: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(request) else {
            return;
        };
        let Some(id) = value.get("id").and_then(serde_json::Value::as_str) else {
            return;
        };
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let response = if boot_id != self.endpoint_boot_id {
            serde_json::json!({"id":id,"error":{"code":"stale_endpoint","message":"endpoint boot changed"}}).to_string()
        } else if !supported_methods().contains(&method) {
            serde_json::json!({"id":id,"error":{"code":"unsupported_method","message":"method was not advertised"}}).to_string()
        } else if method == "client_shell.surface.set" {
            if let Some(active) = value
                .get("params")
                .and_then(|params| params.get("active"))
                .and_then(serde_json::Value::as_bool)
            {
                let revision = self
                    .set_endpoint_surface_active(client_id, active)
                    .expect("checked client");
                serde_json::json!({"id":id,"result":{"type":"client_shell_surface_set","active":active,"projection_revision":revision}}).to_string()
            } else {
                serde_json::json!({"id":id,"error":{"code":"invalid_params","message":"active must be a boolean"}}).to_string()
            }
        } else if !self.endpoint_clients[&client_id].active {
            serde_json::json!({"id":id,"error":{"code":"surface_inactive","message":"this method requires an active client shell surface"}}).to_string()
        } else {
            match serde_json::from_value::<api::schema::Request>(value.clone()) {
                Ok(mut request) => {
                    if matches!(&request.method, api::schema::Method::PaneFocus(params) if params.viewer.is_some()) {
                        self.send_endpoint_response(client_id, id, serde_json::json!({"id":id,"error":{"code":"invalid_params","message":"viewer callbacks use the owning JSON API"}}).to_string());
                        return;
                    }
                    let focus = match &request.method {
                        api::schema::Method::WorkspaceFocus(params) => self.app.parse_workspace_id(&params.workspace_id)
                            .map(|index| (self.app.public_workspace_id(index), None)),
                        api::schema::Method::TabFocus(params) => self.app.parse_tab_id(&params.tab_id)
                            .and_then(|(index, tab)| Some((self.app.public_workspace_id(index), Some(self.app.public_tab_id(index, tab)?)))),
                        api::schema::Method::PaneFocus(params) => self.app.parse_pane_id(&params.pane_id)
                            .and_then(|(index, pane)| Some((self.app.public_workspace_id(index), Some(self.app.public_tab_id(index, self.app.state.workspaces.get(index)?.find_tab_index_for_pane(pane)?)?)))),
                        _ => None,
                    };
                    if let Some((workspace, tab)) = focus {
                        let mut location = self.endpoint_clients[&client_id].location.clone();
                        if let Some(tab) = tab {
                            if let api::schema::Method::PaneFocus(params) = &request.method {
                                location.focus_pane(workspace, tab, params.pane_id.clone());
                            } else { location.focus_tab(workspace, tab); }
                        } else { location.focus_workspace(workspace); }
                        let was_focused = self.endpoint_clients[&client_id].outer_focus == Some(true)
                            && self.endpoint_clients[&client_id].location.focused_pane_id() != location.focused_pane_id();
                        let tab_changed = self.endpoint_clients[&client_id].location.focused_tab_id()
                            != location.focused_tab_id();
                        if was_focused { self.send_endpoint_focus(client_id, false); }
                        let client = self.endpoint_clients.get_mut(&client_id).expect("checked client");
                        client.location = location;
                        client.surface = None;
                        client.presentation_committed = false;
                        if tab_changed { self.claim_endpoint_geometry(client_id); }
                        request.method = match request.method {
                            api::schema::Method::WorkspaceFocus(params) => api::schema::Method::WorkspaceGet(params),
                            api::schema::Method::TabFocus(params) => api::schema::Method::TabGet(params),
                            api::schema::Method::PaneFocus(params) => api::schema::Method::PaneGet(api::schema::PaneTarget { pane_id: params.pane_id }),
                            method => method,
                        };
                        if was_focused { self.send_endpoint_focus(client_id, true); }
                    } else if matches!(request.method, api::schema::Method::WorkspaceFocus(_) | api::schema::Method::TabFocus(_) | api::schema::Method::PaneFocus(_)) {
                        self.send_endpoint_response(client_id, id, serde_json::json!({"id":id,"error":{"code":"not_found","message":"resource does not exist"}}).to_string());
                        return;
                    }
                    // Creation is server-owned; navigation belongs to this viewer.
                    let duplicate = matches!(request.method, api::schema::Method::WorkspaceDuplicate(_));
                    let create_focus = match &mut request.method {
                        api::schema::Method::PaneSplit(params) => std::mem::replace(&mut params.focus, false),
                        api::schema::Method::WorkspaceCreate(params) => std::mem::replace(&mut params.focus, false),
                        api::schema::Method::WorkspaceDuplicate(params) => std::mem::replace(&mut params.focus, false),
                        api::schema::Method::TabCreate(params) => std::mem::replace(&mut params.focus, false),
                        api::schema::Method::WorktreeCreate(params) => std::mem::replace(&mut params.focus, false),
                        api::schema::Method::WorktreeOpen(params) => std::mem::replace(&mut params.focus, false),
                        _ => false,
                    };
                    if matches!(request.method, api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)) {
                        let (sender, receiver) = std::sync::mpsc::channel();
                        self.app.handle_deferred_worktree_api_request(request, sender);
                        let events = self.endpoint_event_tx.clone();
                        std::thread::spawn(move || {
                            if let Ok(response) = receiver.recv() {
                                let _ = events.blocking_send(EndpointTransportEvent::ApiResponse(Box::new(crate::server::endpoint_transport::EndpointApiResponse { client_id, response, create_focus })));
                            }
                        });
                        return;
                    }
                    let response = match request.method {
                        api::schema::Method::WorkspaceDuplicate(params) => {
                            let location = &self.endpoint_clients[&client_id].location;
                            let source = self.app.parse_workspace_id(&params.workspace_id);
                            let active_tab = location.active_tab_ids.get(&params.workspace_id)
                                .and_then(|tab| self.app.parse_tab_id(tab))
                                .filter(|(workspace, _)| Some(*workspace) == source)
                                .map(|(_, tab)| tab);
                            let focused = location.focused_pane_ids.iter().filter_map(|(tab, pane)| {
                                let (workspace, tab) = self.app.parse_tab_id(tab)?;
                                let (pane_workspace, pane) = self.app.parse_pane_id(pane)?;
                                (Some(workspace) == source && pane_workspace == workspace
                                    && self.app.state.workspaces[workspace].find_tab_index_for_pane(pane) == Some(tab))
                                    .then_some((tab, pane))
                            }).collect::<Vec<_>>();
                            self.app.handle_workspace_duplicate_at(request.id, params, active_tab, &focused)
                        }
                        api::schema::Method::ServerReloadConfig(_) => {
                            let report = self.reload_server_config(true);
                            serde_json::to_string(&api::schema::SuccessResponse {
                                id: request.id,
                                result: api::schema::ResponseResult::ConfigReload {
                                    status: report.status, diagnostics: report.diagnostics,
                                },
                            }).unwrap_or_else(|_| "{}".to_owned())
                        }
                        api::schema::Method::PaneSwap(params) => self.app.handle_viewer_pane_swap(request.id, params),
                        api::schema::Method::PaneArrange(params) => {
                            let size = self.endpoint_clients[&client_id].size;
                            self.app.handle_pane_arrange_in_area(request.id, params, ratatui::layout::Rect::new(0, 0, size.cols, size.rows))
                        }
                        api::schema::Method::PaneResize(params) => {
                            let size = self.endpoint_clients[&client_id].size;
                            self.app.handle_viewer_pane_resize(request.id, params, ratatui::layout::Rect::new(0, 0, size.cols, size.rows))
                        }
                        api::schema::Method::RunLogOpen(params) => {
                            self.open_endpoint_job_log(client_id, request.id, &params.job_id)
                        }
                        api::schema::Method::PaneAgentStart(params) => {
                            self.start_endpoint_context_agent(client_id, request.id, params)
                        }
                        api::schema::Method::PaneCommandExecute(params) => {
                            self.execute_endpoint_custom_command(client_id, request.id, params)
                        }
                        api::schema::Method::PaneScrollbackEdit(params) => {
                            self.open_endpoint_scrollback_editor(client_id, request.id, params)
                        }
                        method @ (api::schema::Method::PopupGet(_)
                        | api::schema::Method::PopupSelectionRead(_)
                        | api::schema::Method::PopupScroll(_)) => {
                            let terminal_id = match &method {
                                api::schema::Method::PopupGet(target) => &target.terminal_id,
                                api::schema::Method::PopupSelectionRead(params) => &params.terminal_id,
                                api::schema::Method::PopupScroll(params) => &params.terminal_id,
                                _ => unreachable!(),
                            };
                            let published = self.endpoint_clients[&client_id].surface.as_ref()
                                .and_then(|surface| surface.popup.as_deref())
                                .is_some_and(|popup| popup.terminal_id == *terminal_id);
                            let owned = self.endpoint_target(client_id)
                                .and_then(|target| self.app.state.popup_pane_for_workspace(
                                    &self.app.state.workspaces[target.workspace_index].id,
                                )).is_some_and(|popup| popup.terminal_id.to_string() == *terminal_id);
                            if published && owned {
                                let client = &self.endpoint_clients[&client_id];
                                let surface_revision = client.surface.as_ref().map(|surface| surface.surface_revision);
                                if client.popup_read_facts.is_none() || client.popup_read_facts != self.popup_read_facts(client_id) {
                                    serde_json::json!({"id":request.id,"error":{"code":"stale_content","message":"pane content changed"}}).to_string()
                                } else {
                                    let response = self.app.handle_popup_content(request.id, method);
                                    let mut response: serde_json::Value = serde_json::from_str(&response).expect("encoded API response");
                                    if response["result"]["type"] == "popup_terminal" {
                                        response["result"]["surface_revision"] = serde_json::json!(surface_revision);
                                    }
                                    response.to_string()
                                }
                            } else {
                                serde_json::json!({"id":request.id,"error":{"code":"popup_not_open","message":"no popup is open"}}).to_string()
                            }
                        }
                        api::schema::Method::PopupClose(_) => {
                            let popup = self.endpoint_target(client_id)
                                .and_then(|target| self.app.state.popup_pane_for_workspace(
                                    &self.app.state.workspaces[target.workspace_index].id,
                                )).map(|popup| popup.pane_id);
                            if popup.is_some_and(|pane| self.close_endpoint_popup(pane)) {
                                serde_json::json!({"id":request.id,"result":{"type":"ok"}}).to_string()
                            } else {
                                serde_json::json!({"id":request.id,"error":{"code":"popup_not_open","message":"no popup is open"}}).to_string()
                            }
                        }
                        method => self.app.handle_api_request(api::schema::Request { id: request.id, method }),
                    };
                    self.project_endpoint_api_response(client_id, response, create_focus, duplicate)
                }
                Err(error) => serde_json::json!({"id":id,"error":{"code":"invalid_params","message":error.to_string()}}).to_string(),
            }
        };
        self.send_endpoint_response(client_id, id, response);
    }

    fn project_endpoint_api_response(
        &mut self,
        client_id: u64,
        response: String,
        create_focus: bool,
        duplicate: bool,
    ) -> String {
        if let Ok(mut success) = serde_json::from_str::<api::schema::SuccessResponse>(&response) {
            if let api::schema::ResponseResult::PaneSwap { swap } = &success.result {
                if swap.changed {
                    self.focus_endpoint_pane(client_id, &swap.source_pane_id);
                }
            }
            if create_focus && self.endpoint_clients[&client_id].active {
                match &success.result {
                    api::schema::ResponseResult::PaneInfo { pane } => {
                        self.focus_endpoint_pane(client_id, &pane.pane_id)
                    }
                    api::schema::ResponseResult::WorkspaceCreated { workspace, .. }
                        if duplicate =>
                    {
                        let pane = self
                            .app
                            .parse_workspace_id(&workspace.workspace_id)
                            .and_then(|index| {
                                self.app.state.workspaces[index]
                                    .focused_pane_id()
                                    .and_then(|pane| self.app.public_pane_id(index, pane))
                            });
                        if let Some(pane) = pane {
                            self.focus_endpoint_pane(client_id, &pane);
                        }
                    }
                    api::schema::ResponseResult::WorkspaceCreated { root_pane, .. }
                    | api::schema::ResponseResult::TabCreated { root_pane, .. }
                    | api::schema::ResponseResult::WorktreeCreated { root_pane, .. }
                    | api::schema::ResponseResult::WorktreeOpened { root_pane, .. } => {
                        self.focus_endpoint_pane(client_id, &root_pane.pane_id)
                    }
                    _ => {}
                }
            }
            let focused_pane = self.endpoint_clients[&client_id]
                .location
                .focused_pane_id()
                .map(str::to_owned);
            self.endpoint_clients[&client_id]
                .location
                .project_response(&mut success.result, focused_pane.as_deref());
            match serde_json::to_string(&success) {
                            Ok(response) => response,
                            Err(error) => serde_json::json!({"id":success.id,"error":{"code":"internal_error","message":error.to_string()}}).to_string(),
                        }
        } else {
            response
        }
    }

    fn start_endpoint_context_agent(
        &mut self,
        client_id: u64,
        id: String,
        params: api::schema::PaneAgentStartParams,
    ) -> String {
        let target = self.endpoint_target(client_id);
        if !target.is_some_and(|target| {
            target.pane_focus.is_some_and(|pane| {
                self.app.parse_pane_id(&params.pane_id) == Some((target.workspace_index, pane))
            })
        }) {
            return serde_json::json!({"id": id, "error": {"code": "pane_not_found", "message": "focused pane disappeared"}}).to_string();
        }
        let response = self.app.handle_pane_agent_start(id, params, false);
        if let Ok(api::schema::SuccessResponse {
            result: api::schema::ResponseResult::AgentStarted { agent, .. },
            ..
        }) = serde_json::from_str::<api::schema::SuccessResponse>(&response)
        {
            self.focus_endpoint_pane(client_id, &agent.pane_id);
        }
        response
    }

    fn execute_endpoint_custom_command(
        &mut self,
        client_id: u64,
        id: String,
        params: api::schema::PaneCommandExecuteParams,
    ) -> String {
        let Some(target) = self.endpoint_target(client_id) else {
            return serde_json::json!({"id":id,"error":{"code":"pane_not_found","message":"focused pane disappeared"}}).to_string();
        };
        let Some(pane_id) = target.pane_focus else {
            return serde_json::json!({"id":id,"error":{"code":"pane_not_found","message":"no focused pane"}}).to_string();
        };
        if self.app.parse_pane_id(&params.pane_id) != Some((target.workspace_index, pane_id)) {
            return serde_json::json!({"id":id,"error":{"code":"pane_not_found","message":"focused pane disappeared"}}).to_string();
        }
        let tab_id = self
            .app
            .public_tab_id(target.workspace_index, target.tab_index);
        let previous_pane = params.pane_id.clone();
        let previous_zoomed =
            self.app.state.workspaces[target.workspace_index].tabs[target.tab_index].zoomed;
        let size = self.endpoint_clients[&client_id].size;
        let response =
            self.app
                .handle_pane_command_execute(id, params, (size.rows, size.cols), false);
        if let Ok(api::schema::SuccessResponse {
            result:
                api::schema::ResponseResult::CommandExecuted {
                    pane: Some(pane), ..
                },
            ..
        }) = serde_json::from_str::<api::schema::SuccessResponse>(&response)
        {
            if let (Some(tab_id), Some((_, created))) =
                (tab_id, self.app.parse_pane_id(&pane.pane_id))
            {
                self.endpoint_clients
                    .get_mut(&client_id)
                    .expect("request owner exists")
                    .editors
                    .push(EndpointEditor {
                        pane_id: created,
                        public_pane_id: pane.pane_id.clone(),
                        previous_pane,
                        tab_id,
                        workspace_index: target.workspace_index,
                        tab_index: target.tab_index,
                        previous_zoomed,
                    });
                self.focus_endpoint_pane(client_id, &pane.pane_id);
            }
        }
        response
    }

    fn open_endpoint_scrollback_editor(
        &mut self,
        client_id: u64,
        id: String,
        params: api::schema::PaneTarget,
    ) -> String {
        let error = |code: &str, message: &str| {
            serde_json::json!({
                "id": id, "error": {"code": code, "message": message},
            })
            .to_string()
        };
        let Some(target) = self.endpoint_target(client_id) else {
            return error("pane_not_found", "focused pane disappeared");
        };
        let Some(pane_id) = target.pane_focus else {
            return error("pane_not_found", "no focused pane");
        };
        if self.app.parse_pane_id(&params.pane_id) != Some((target.workspace_index, pane_id)) {
            return error("pane_not_found", "focused pane disappeared");
        }
        let Some(tab_id) = self
            .app
            .public_tab_id(target.workspace_index, target.tab_index)
        else {
            return error("pane_not_found", "focused pane disappeared");
        };
        let previous_zoomed =
            self.app.state.workspaces[target.workspace_index].tabs[target.tab_index].zoomed;
        let size = self.endpoint_clients[&client_id].size;
        let previous_pane = params.pane_id.clone();
        let response =
            self.app
                .handle_pane_scrollback_edit(id, params, (size.rows, size.cols), false);
        if let Ok(api::schema::SuccessResponse {
            result: api::schema::ResponseResult::PaneInfo { pane },
            ..
        }) = serde_json::from_str::<api::schema::SuccessResponse>(&response)
        {
            if let Some((_, editor_id)) = self.app.parse_pane_id(&pane.pane_id) {
                self.endpoint_clients
                    .get_mut(&client_id)
                    .expect("request owner exists")
                    .editors
                    .push(EndpointEditor {
                        pane_id: editor_id,
                        public_pane_id: pane.pane_id.clone(),
                        previous_pane,
                        tab_id,
                        workspace_index: target.workspace_index,
                        tab_index: target.tab_index,
                        previous_zoomed,
                    });
                self.focus_endpoint_pane(client_id, &pane.pane_id);
            }
        }
        response
    }

    pub(super) fn restore_endpoint_editor_after_exit(&mut self, pane_id: crate::layout::PaneId) {
        let owners = self
            .endpoint_clients
            .iter_mut()
            .filter_map(|(&client_id, client)| {
                let index = client
                    .editors
                    .iter()
                    .position(|editor| editor.pane_id == pane_id)?;
                let editor = client.editors.remove(index);
                let still_focused = client.location.focused_pane_ids.get(&editor.tab_id)
                    == Some(&editor.public_pane_id);
                Some((client_id, editor, still_focused))
            })
            .collect::<Vec<_>>();
        for (client_id, editor, still_focused) in owners {
            if !still_focused {
                continue;
            }
            if let Some(tab) = self
                .app
                .state
                .workspaces
                .get_mut(editor.workspace_index)
                .and_then(|workspace| workspace.tabs.get_mut(editor.tab_index))
            {
                tab.zoomed = editor.previous_zoomed;
            }
            if self.app.parse_pane_id(&editor.previous_pane).is_none() {
                continue;
            }
            let active = self.endpoint_clients[&client_id].location.focused_tab_id()
                == Some(editor.tab_id.as_str());
            if active {
                self.focus_endpoint_pane(client_id, &editor.previous_pane);
            } else if let Some(client) = self.endpoint_clients.get_mut(&client_id) {
                client
                    .location
                    .focused_pane_ids
                    .insert(editor.tab_id, editor.previous_pane);
            }
        }
    }

    fn open_endpoint_job_log(&mut self, client_id: u64, id: String, job_id: &str) -> String {
        let mut caller_pane = None;
        let mut popup_opened = false;
        if let Some((job, workspace_index, pane_id)) = self.app.job_log_target(job_id) {
            caller_pane = Some(job.caller_pane.clone());
            if self.endpoint_clients[&client_id].location.focused_pane_id()
                != Some(&job.caller_pane)
            {
                self.focus_endpoint_pane(client_id, &job.caller_pane);
            } else if let Ok(executable) = std::env::current_exe() {
                if let Some(tab_index) =
                    self.app.state.workspaces[workspace_index].find_tab_index_for_pane(pane_id)
                {
                    let path = std::path::PathBuf::from(&job.log_path);
                    let size = self.endpoint_clients[&client_id].size;
                    let argv = vec![
                        executable.display().to_string(),
                        "__job-log-view".into(),
                        job.log_path,
                    ];
                    let focused = self
                        .endpoint_clients
                        .iter()
                        .filter_map(|(id, client)| {
                            (client.active
                                && client.outer_focus == Some(true)
                                && self.endpoint_target(*id).is_some_and(|target| {
                                    target.workspace_index == workspace_index
                                }))
                            .then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    let mut old_terminals = Vec::new();
                    for viewer in &focused {
                        if let Some(target) = self.endpoint_target(*viewer) {
                            if let Some(terminal) = target.pane_focus.and_then(|pane| {
                                self.app.state.workspaces[target.workspace_index]
                                    .terminal_id(pane)
                                    .cloned()
                            }) {
                                if !old_terminals.contains(&terminal) {
                                    old_terminals.push(terminal);
                                }
                            }
                        }
                    }
                    if self
                        .app
                        .spawn_popup_argv_at(
                            crate::app::PopupOwner {
                                workspace_index,
                                tab_index,
                                pane_id,
                                terminal_area: ratatui::layout::Rect::new(
                                    0, 0, size.cols, size.rows,
                                ),
                            },
                            &argv,
                            path.parent()
                                .unwrap_or(std::path::Path::new("/"))
                                .to_path_buf(),
                            crate::app::PopupGeometry::default(),
                        )
                        .is_ok()
                    {
                        if let Some(popup) = self.app.state.popup_pane_for_workspace(
                            &self.app.state.workspaces[workspace_index].id,
                        ) {
                            let terminal = popup.terminal_id.clone();
                            if let Some(state) = self.app.state.terminals.get_mut(&terminal) {
                                state.set_manual_label(format!("job log: {}", job.label));
                            }
                        }
                        for terminal in old_terminals {
                            if let Some(runtime) = self.app.terminal_runtimes.get(&terminal) {
                                runtime.try_send_focus_event(crate::ghostty::FocusEvent::Lost);
                            }
                        }
                        if let Some(viewer) = focused.first() {
                            if let Some(runtime) = self.endpoint_focused_runtime(*viewer) {
                                runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained);
                            }
                        }
                        for viewer in focused {
                            let held = self
                                .endpoint_clients
                                .get_mut(&viewer)
                                .map(|client| client.held_inputs.drain())
                                .unwrap_or_default();
                            self.release_endpoint_inputs(viewer, held);
                            if let Some(client) = self.endpoint_clients.get_mut(&viewer) {
                                client.surface = None;
                                client.presentation_committed = false;
                            }
                        }
                        for client in self.endpoint_clients.values_mut() {
                            if client.active
                                && client.location.focused_workspace_id.as_deref()
                                    == Some(self.app.public_workspace_id(workspace_index).as_str())
                            {
                                client.surface = None;
                                client.presentation_committed = false;
                            }
                        }
                        popup_opened = true;
                    }
                }
            }
        }
        serde_json::to_string(&api::schema::SuccessResponse {
            id,
            result: api::schema::ResponseResult::RunLogOpened {
                caller_pane,
                popup_opened,
            },
        })
        .unwrap_or_else(|_| "{}".into())
    }

    fn focus_endpoint_pane(&mut self, client_id: u64, public_id: &str) {
        let Some((index, pane_id)) = self.app.parse_pane_id(public_id) else {
            return;
        };
        let Some(tab_index) = self.app.state.workspaces[index].find_tab_index_for_pane(pane_id)
        else {
            return;
        };
        let workspace = self.app.public_workspace_id(index);
        let Some(tab) = self.app.public_tab_id(index, tab_index) else {
            return;
        };
        let was_focused = self.endpoint_clients.get(&client_id).is_some_and(|client| {
            client.active
                && client.outer_focus == Some(true)
                && client.location.focused_pane_id() != Some(public_id)
        });
        if was_focused {
            self.send_endpoint_focus(client_id, false);
        }
        let Some(client) = self.endpoint_clients.get_mut(&client_id) else {
            return;
        };
        client.location.focus_pane(workspace, tab, public_id.into());
        client.surface = None;
        client.presentation_committed = false;
        if was_focused {
            self.send_endpoint_focus(client_id, true);
        }
    }

    pub(in crate::server::headless) fn send_endpoint_notification(
        &self,
        notification: &ServerMessage,
    ) -> Option<bool> {
        let owner = self
            .endpoint_clients
            .iter()
            .filter(|(_, client)| client.active && client.presentation_committed)
            .max_by_key(|(_, client)| client.last_activity);
        let (client_id, client) = owner?;
        if self
            .foreground_client_id
            .and_then(|id| self.clients.get(&id))
            .is_some_and(|legacy| legacy.last_activity > client.last_activity)
        {
            return None;
        }
        let payload = crate::protocol::endpoint_projection::ForwardedNotification {
            boot_id: self.endpoint_boot_id.clone(),
            viewer_id: Some(*client_id),
            notification: notification.clone(),
        };
        let Ok(data) = serde_json::to_string(&payload) else {
            return Some(false);
        };
        let Ok(frame) = framed(&wire::ServerMessage::EndpointControl {
            kind: crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND.into(),
            data,
        }) else {
            return Some(false);
        };
        Some(client.writer.control.send(frame).is_ok())
    }

    pub(super) fn route_viewer_pane_focus(
        &self,
        id: &str,
        target: &api::schema::PaneFocusParams,
    ) -> String {
        let reject = |code: &str, message: &str| {
            serde_json::json!({"id":id,"error":{"code":code,"message":message}}).to_string()
        };
        let Some(viewer) = target.viewer.as_ref() else {
            return reject("invalid_params", "viewer is required");
        };
        if viewer.boot_id != self.endpoint_boot_id {
            return reject("stale_endpoint", "endpoint boot changed");
        }
        let Some(client) = self.endpoint_clients.get(&viewer.client_id) else {
            return reject("viewer_unavailable", "viewer is disconnected");
        };
        if self.app.parse_pane_id(&target.pane_id).is_none() {
            return reject("pane_not_found", "pane not found");
        }
        let Ok(data) = serde_json::to_string(target) else {
            return reject("serialization_error", "viewer focus could not be encoded");
        };
        let Ok(frame) = framed(&wire::ServerMessage::EndpointControl {
            kind: crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND.into(),
            data,
        }) else {
            return reject("serialization_error", "viewer focus could not be framed");
        };
        if client.writer.control.send(frame).is_err() {
            return reject("viewer_unavailable", "viewer delivery failed");
        }
        serde_json::json!({"id":id,"result":{"type":"ok"}}).to_string()
    }

    fn send_endpoint_response(&self, client_id: u64, id: &str, response: String) {
        if let Some(client) = self.endpoint_clients.get(&client_id) {
            let bytes = response.into_bytes();
            let count = bytes
                .len()
                .div_ceil(crate::protocol::endpoint::ENDPOINT_RESPONSE_CHUNK_BYTES)
                .max(1);
            for (index, chunk) in bytes
                .chunks(crate::protocol::endpoint::ENDPOINT_RESPONSE_CHUNK_BYTES)
                .chain(bytes.is_empty().then_some(&[][..]))
                .enumerate()
            {
                let message = wire::ServerMessage::ClientShellEndpointResponseChunk {
                    boot_id: self.endpoint_boot_id.clone(),
                    request_id: id.into(),
                    final_chunk: index + 1 == count,
                    data: chunk.to_vec(),
                };
                let Ok(data) = framed(&message) else {
                    break;
                };
                if client.writer.control.send(data).is_err() {
                    break;
                }
            }
        }
    }

    fn apply_endpoint_clipboard_image(
        &mut self,
        client_id: u64,
        target: wire::ClientClipboardImageTarget,
        extension: &str,
        data: &[u8],
    ) -> bool {
        if self.handoff_in_progress
            || data.is_empty()
            || data.len() > crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD
        {
            return false;
        }
        // Validate the published owner/target contract before creating a file.
        let valid = match &target {
            wire::ClientClipboardImageTarget::Pane(id) => {
                self.apply_endpoint_pane_input(client_id, id, Vec::new())
            }
            wire::ClientClipboardImageTarget::Popup(id) => {
                self.apply_endpoint_popup_input(client_id, id, Vec::new())
            }
            wire::ClientClipboardImageTarget::DirectTerminal => false,
        };
        if !valid {
            return false;
        }
        let staged = match crate::server::clipboard_image::stage(client_id, extension, data) {
            Ok(staged) => staged,
            Err(error) => {
                self.endpoint_error(client_id, error.to_string());
                return false;
            }
        };
        let events = vec![ClientPaneInputEvent::Paste(staged.paste_text)];
        let accepted = match target {
            wire::ClientClipboardImageTarget::Pane(id) => {
                self.apply_endpoint_pane_input(client_id, &id, events)
            }
            wire::ClientClipboardImageTarget::Popup(id) => {
                self.apply_endpoint_popup_input(client_id, &id, events)
            }
            wire::ClientClipboardImageTarget::DirectTerminal => false,
        };
        if accepted {
            if let Some(client) = self.endpoint_clients.get_mut(&client_id) {
                client.staged_clipboard_files.push(staged.path);
            }
        } else {
            crate::server::clipboard_image::remove_files(vec![staged.path]);
        }
        accepted
    }

    fn apply_endpoint_popup_input(
        &mut self,
        client_id: u64,
        terminal_id: &str,
        mut events: Vec<ClientPaneInputEvent>,
    ) -> bool {
        let Some(client) = self.endpoint_clients.get(&client_id) else {
            return false;
        };
        if !client.active
            || !client.surface.as_ref().is_some_and(|surface| {
                surface.projection_revision == client.projection_revision
                    && surface
                        .popup
                        .as_ref()
                        .is_some_and(|popup| popup.terminal_id == terminal_id)
            })
        {
            return false;
        }
        let pixel_mouse = client.pixel_mouse;
        let Some(target) = self.endpoint_target(client_id) else {
            return false;
        };
        let Some(popup) = self
            .app
            .state
            .popup_pane_for_workspace(&self.app.state.workspaces[target.workspace_index].id)
            .filter(|popup| popup.terminal_id.to_string() == terminal_id)
        else {
            return false;
        };
        let pane_id = popup.pane_id;
        let Some(runtime) = self.app.terminal_runtimes.get(&popup.terminal_id) else {
            return false;
        };
        let close_at = (!crate::app::popup_child_claims_escape(runtime))
            .then(|| {
                events.iter().position(|event| {
                    matches!(
                        event,
                        ClientPaneInputEvent::Key {
                            code: wire::ClientKeyCode::Esc,
                            modifiers: 0,
                            kind: wire::ClientKeyKind::Press,
                            ..
                        }
                    )
                })
            })
            .flatten();
        if let Some(index) = close_at {
            events.truncate(index);
        }
        let (accepted, failure) =
            crate::server::endpoint_input::apply_events(runtime, events, pixel_mouse, None);
        for event in &accepted {
            self.endpoint_clients
                .get_mut(&client_id)
                .expect("live client")
                .held_inputs
                .track(terminal_id, event);
        }
        if let Some(error) = failure {
            self.endpoint_error(client_id, error);
            return false;
        }
        if close_at.is_some() {
            self.close_endpoint_popup(pane_id);
        }
        true
    }

    fn apply_endpoint_pane_input(
        &mut self,
        client_id: u64,
        public_id: &str,
        events: Vec<ClientPaneInputEvent>,
    ) -> bool {
        let Some(client) = self.endpoint_clients.get(&client_id) else {
            return false;
        };
        if !client.active
            || !client.surface.as_ref().is_some_and(|surface| {
                surface.projection_revision == client.projection_revision
                    && surface.popup.is_none()
                    && surface.panes.iter().any(|pane| pane.pane_id == public_id)
            })
        {
            return false;
        }
        let pixel_mouse = client.pixel_mouse;
        let Some(target) = self.endpoint_target(client_id) else {
            return false;
        };
        let Some((workspace_index, pane)) = self.app.parse_pane_id(public_id) else {
            return false;
        };
        if self
            .app
            .state
            .popup_pane_for_workspace(&self.app.state.workspaces[target.workspace_index].id)
            .is_some()
            || workspace_index != target.workspace_index
            || self.app.state.workspaces[workspace_index].find_tab_index_for_pane(pane)
                != Some(target.tab_index)
        {
            return false;
        }
        let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
            &self.app.terminal_runtimes,
            workspace_index,
            pane,
        ) else {
            return false;
        };
        let page_lines = client
            .surface
            .as_ref()
            .and_then(|surface| surface.panes.iter().find(|pane| pane.pane_id == public_id))
            .map(|pane| usize::from(pane.inner_rect.height).max(1));
        let (accepted, failure) =
            crate::server::endpoint_input::apply_events(runtime, events, pixel_mouse, page_lines);
        let client = self
            .endpoint_clients
            .get_mut(&client_id)
            .expect("live client");
        for event in &accepted {
            client.held_inputs.track(public_id, event);
        }
        if let Some(error) = failure {
            self.endpoint_error(client_id, error);
            return false;
        }
        if !accepted.is_empty() {
            self.claim_endpoint_geometry(client_id);
        }
        true
    }
}
