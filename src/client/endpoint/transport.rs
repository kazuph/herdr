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
            if events
                .blocking_send(EndpointReaderEvent::Message {
                    endpoint_id: endpoint_id.clone(),
                    generation,
                    message: Box::new(message),
                })
                .is_err()
            {
                break;
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
