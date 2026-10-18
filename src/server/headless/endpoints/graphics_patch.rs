//! Large images to a viewer that rebuilds them from changed rectangles.
use super::*;
use crate::kitty_graphics::patch::{self, GraphicsPatch};

/// Patch messages for one frame and the images they deliver.
#[derive(Default)]
pub(super) struct PatchedFrame {
    pub(super) messages: Vec<wire::ServerMessage>,
    pub(super) bases: Vec<(wire::SurfaceGraphicsAssetKey, std::sync::Arc<Vec<u8>>)>,
}

/// Longest a viewer may take to acknowledge a patched frame before the next
/// frame is sent anyway.
const PATCH_ACK_LIMIT: std::time::Duration = std::time::Duration::from_millis(500);

fn record_patch_ack_release(
    trace: &mut Option<(u64, std::time::Instant)>,
    client_id: u64,
    reason: &'static str,
) {
    if let Some((surface_revision, started)) = trace.take() {
        tracing::info!(target: "herdr_latency", event = "surface.ack_released",
                time_us = crate::logging::latency_trace_time_us(), pid = std::process::id(),
                client_id, surface_revision, reason, waited_ms = started.elapsed().as_millis(),
                "graphics acknowledgement wait released");
    }
}

impl EndpointClient {
    pub(super) fn finish_patch_ack_trace(&mut self, client_id: u64, reason: &'static str) {
        record_patch_ack_release(&mut self.patch_ack_trace, client_id, reason);
    }

    /// True while the last patched frame is unacknowledged and recent: the
    /// viewer is still receiving or rebuilding it, so another frame would only
    /// queue in the connection.
    pub(super) fn waiting_for_patch_ack(&self) -> bool {
        self.patch_ack_wait
            .is_some_and(|(_, sent)| sent.elapsed() < PATCH_ACK_LIMIT)
    }
}

impl HeadlessServer {
    /// Turn this frame's large raw images into patch messages for a viewer
    /// that accepts them. The images leave the frame's asset list; the viewer
    /// rebuilds them from the patch against the image it already holds (or
    /// from a whole-image patch) before the frame that places them arrives.
    pub(super) fn endpoint_graphics_patches(
        &mut self,
        client_id: u64,
        graphics: &mut wire::SurfaceGraphicsScene,
    ) -> PatchedFrame {
        let mut out = PatchedFrame::default();
        let Some(client) = self.endpoint_clients.get(&client_id) else {
            return out;
        };
        if !client.graphics_patches {
            return out;
        }
        for asset in &mut graphics.assets {
            if !patch::patchable(&asset.key) || asset.data.len() as u64 != asset.key.data_len {
                continue;
            }
            let raw = std::mem::take(&mut asset.data);
            // Only a base the viewer still holds can be patched against.
            let base = client
                .patch_bases
                .get(&asset.key.source)
                .filter(|(key, _)| client.graphics_delivery.contains(key));
            let message = base
                .and_then(|(base_key, base_data)| {
                    patch::diff(base_key, base_data, &asset.key, &raw)
                })
                .or_else(|| GraphicsPatch::whole(&asset.key, &raw));
            let Some(message) = message else {
                // Could not encode: keep the image in the frame as before.
                asset.data = raw;
                continue;
            };
            let Ok(data) = serde_json::to_string(&message) else {
                asset.data = raw;
                continue;
            };
            out.messages.push(wire::ServerMessage::EndpointControl {
                kind: crate::protocol::endpoint::GRAPHICS_PATCH_KIND.into(),
                data,
            });
            out.bases
                .push((asset.key.clone(), std::sync::Arc::new(raw)));
        }
        graphics.assets.retain(|asset| !asset.data.is_empty());
        out
    }

    /// The viewer has the patched frame `data` names: the next one may go.
    pub(super) fn endpoint_graphics_patch_ack(&mut self, client_id: u64, data: &str) -> bool {
        let Some(client) = self.endpoint_clients.get_mut(&client_id) else {
            return false;
        };
        let Ok(revision) = data.trim().parse::<u64>() else {
            return false;
        };
        if client
            .patch_ack_wait
            .is_none_or(|(waiting, _)| revision < waiting)
        {
            return false;
        }
        client.patch_ack_wait = None;
        client.write_pending = true;
        client.finish_patch_ack_trace(client_id, "ack");
        true
    }

    /// The viewer could not rebuild an image: forget every base and resend.
    pub(super) fn endpoint_graphics_patch_miss(&mut self, client_id: u64) -> bool {
        let Some(client) = self.endpoint_clients.get_mut(&client_id) else {
            return false;
        };
        client.patch_ack_wait = None;
        client.patch_bases.clear();
        client.graphics_delivery = Default::default();
        client.write_pending = true;
        client.finish_patch_ack_trace(client_id, "miss");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_release_trace_is_once_per_wait_for_each_reason() {
        let path = std::env::temp_dir().join(format!("herdr-ack-trace-{}.log", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || file.try_clone().unwrap())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            for reason in ["ack", "timeout", "miss"] {
                let mut trace = Some((17, std::time::Instant::now()));
                record_patch_ack_release(&mut trace, 3, reason);
                assert!(trace.is_none());
                record_patch_ack_release(&mut trace, 3, reason);
            }
        });
        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(log.matches("surface.ack_released").count(), 3);
        for reason in ["ack", "timeout", "miss"] {
            assert_eq!(log.matches(&format!("reason=\"{reason}\"")).count(), 1);
        }
        assert_eq!(log.matches("surface_revision=17").count(), 3);
        assert_eq!(log.matches("waited_ms=").count(), 3);
        std::fs::remove_file(path).unwrap();
    }
}
