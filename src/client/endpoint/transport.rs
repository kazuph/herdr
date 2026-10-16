//! Connection-local decoding precedes selection and presentation filtering.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use interprocess::TryClone as _;
use tokio::sync::mpsc;

use super::writer::NativeEndpointTransport;
use super::{ClientEndpointId, EndpointNegotiation};
use crate::ipc::LocalStream;
use crate::protocol::endpoint_wire::ServerMessage;

#[cfg(all(test, unix))]
mod tests;

pub(crate) enum EndpointReaderEvent {
    Message {
        endpoint_id: ClientEndpointId,
        generation: u64,
        message: Box<ServerMessage>,
    },
    Disconnected {
        endpoint_id: ClientEndpointId,
        generation: u64,
        error: io::Error,
    },
}

pub(crate) fn start(
    stream: LocalStream,
    lifetime: impl Send + 'static,
    endpoint_id: ClientEndpointId,
    generation: u64,
    negotiation: &EndpointNegotiation,
    events: mpsc::Sender<EndpointReaderEvent>,
) -> io::Result<NativeEndpointTransport> {
    let reader = stream.try_clone()?;
    let writer = NativeEndpointTransport::with_lifetime(stream, lifetime)?;
    let stopped = writer.stop_handle();
    let decoder = negotiation
        .supports_capability(crate::protocol::surface_reuse::CAPABILITY)
        .then(|| {
            crate::protocol::surface_reuse::Decoder::new(
                negotiation.supports_capability(crate::protocol::surface_delta::CAPABILITY),
                negotiation.supports_capability(crate::protocol::surface_scroll::CAPABILITY),
            )
        });
    std::thread::Builder::new()
        .name("endpoint-reader".into())
        .spawn(move || read_loop(reader, stopped, endpoint_id, generation, decoder, events))?;
    Ok(writer)
}

fn read_loop(
    mut stream: LocalStream,
    stopped: Arc<AtomicBool>,
    endpoint_id: ClientEndpointId,
    generation: u64,
    mut decoder: Option<crate::protocol::surface_reuse::Decoder>,
    events: mpsc::Sender<EndpointReaderEvent>,
) {
    let result = (|| {
        crate::ipc::set_local_stream_polling(&mut stream, true)?;
        let mut reader = EndpointReader {
            stream: &mut stream,
            stopped: &stopped,
        };
        let mut patches = PatchRebuild::default();
        while !stopped.load(Ordering::Acquire) {
            let message = crate::protocol::read_message(
                &mut reader,
                crate::protocol::MAX_GRAPHICS_FRAME_SIZE,
            )
            .map_err(|error| match error {
                crate::protocol::FramingError::Io(error) => error,
                crate::protocol::FramingError::UnexpectedEof => io::ErrorKind::UnexpectedEof.into(),
                error => io::Error::new(io::ErrorKind::InvalidData, error),
            })?;
            let message = if let Some(decoder) = &mut decoder {
                decoder
                    .decode(message)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            } else {
                message
            };
            for message in patches.receive(message) {
                if events
                    .blocking_send(EndpointReaderEvent::Message {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                        message: Box::new(message),
                    })
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
        Ok::<(), io::Error>(())
    })();
    // Cancellation belongs to the retiring generation and must not fail its replacement.
    if !stopped.load(Ordering::Acquire) {
        let error = result
            .err()
            .unwrap_or_else(|| io::ErrorKind::ConnectionAborted.into());
        let _ = events.blocking_send(EndpointReaderEvent::Disconnected {
            endpoint_id,
            generation,
            error,
        });
    }
}

struct EndpointReader<'a> {
    stream: &'a mut LocalStream,
    stopped: &'a AtomicBool,
}

impl io::Read for EndpointReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Ok(0);
            }
            match crate::ipc::poll_local_stream_read_count(self.stream, buffer) {
                Ok(crate::ipc::LocalStreamReadCount::Data(count)) => return Ok(count),
                Ok(crate::ipc::LocalStreamReadCount::Closed) => return Ok(0),
                Ok(crate::ipc::LocalStreamReadCount::Pending) => {
                    crate::platform::wait_client_stream_readable(self.stream)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

/// Rebuilds large images from their changed-rectangle patches on the reader
/// thread. Decoding a browser pane's patches takes tens of milliseconds per
/// frame; doing it on the event loop left clicks and keys waiting behind every
/// frame of a scrolling page. A patch that cannot be rebuilt here is passed on
/// unchanged, so the event loop's own rebuild reports it as a miss.
#[derive(Default)]
struct PatchRebuild {
    pending: Vec<(crate::kitty_graphics::patch::GraphicsPatch, ServerMessage)>,
    boot_id: String,
    /// Full pixels of the large images the last frame placed or retained.
    images:
        std::collections::HashMap<crate::protocol::endpoint_wire::SurfaceGraphicsAssetKey, Vec<u8>>,
}

impl PatchRebuild {
    fn receive(&mut self, message: ServerMessage) -> Vec<ServerMessage> {
        match message {
            ServerMessage::EndpointControl { ref kind, ref data }
                if kind == crate::protocol::endpoint::GRAPHICS_PATCH_KIND =>
            {
                match serde_json::from_str::<crate::kitty_graphics::patch::GraphicsPatch>(data) {
                    Ok(patch) => {
                        self.pending.push((patch, message));
                        Vec::new()
                    }
                    Err(_) => vec![message],
                }
            }
            ServerMessage::PaneSurface(mut surface) => {
                let mut out = self.rebuild(&mut surface);
                out.push(ServerMessage::PaneSurface(surface));
                out
            }
            message @ ServerMessage::PaneSurfacePatch(_) => {
                // Not rebuilt here: the event loop applies the patches to this frame.
                let mut out = std::mem::take(&mut self.pending)
                    .into_iter()
                    .map(|(_, message)| message)
                    .collect::<Vec<_>>();
                out.push(message);
                out
            }
            message => vec![message],
        }
    }

    /// Adds the rebuilt images to `surface` and returns the patches it could
    /// not rebuild, to be delivered ahead of the frame.
    fn rebuild(
        &mut self,
        surface: &mut crate::protocol::endpoint_wire::PaneSurfaceFrame,
    ) -> Vec<ServerMessage> {
        if self.boot_id != surface.boot_id {
            self.boot_id.clone_from(&surface.boot_id);
            self.images.clear();
        }
        let mut unbuilt = Vec::new();
        for (patch, message) in std::mem::take(&mut self.pending) {
            if surface
                .graphics
                .assets
                .iter()
                .any(|asset| asset.key == patch.key)
            {
                continue;
            }
            let base = self.images.get(&patch.base).map(Vec::as_slice);
            match crate::kitty_graphics::patch::apply(base, &patch) {
                Some(data) => surface.graphics.assets.push(
                    crate::protocol::endpoint_wire::SurfaceGraphicsAsset {
                        key: patch.key,
                        data,
                    },
                ),
                None => unbuilt.push(message),
            }
        }
        for asset in &surface.graphics.assets {
            if crate::kitty_graphics::patch::patchable(&asset.key)
                && asset.data.len() as u64 == asset.key.data_len
            {
                self.images.insert(asset.key.clone(), asset.data.clone());
            }
        }
        let graphics = &surface.graphics;
        self.images.retain(|key, _| {
            graphics
                .placements
                .iter()
                .any(|placement| placement.asset == *key)
                || graphics.retained_assets.contains(key)
        });
        unbuilt
    }
}
