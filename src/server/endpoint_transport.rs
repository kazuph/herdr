//! Generation-1 endpoint sockets are a separate codec from the fork's private client wire.
//! The first decoded hello pins this transport; no message is retried with another codec.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tokio::sync::mpsc;

use super::client_transport::{
    set_client_recv_timeout, ClientWriter, ClientWriterSignal, HANDSHAKE_TIMEOUT,
    MAX_INPUT_EVENT_BATCH, MAX_INPUT_PAYLOAD,
};
use crate::ipc::LocalStream;
use crate::protocol::endpoint::{self, EndpointClientHello};
use crate::protocol::endpoint_wire::{ClientMessage, ClientPaneInputEvent, ServerMessage};

#[cfg(all(test, unix))]
mod tests;

#[derive(Debug)]
pub(crate) struct EndpointApiResponse {
    pub(crate) client_id: u64,
    pub(crate) response: String,
    pub(crate) create_focus: bool,
}

#[derive(Debug)]
pub(crate) enum EndpointTransportEvent {
    Connected {
        client_id: u64,
        hello: Box<EndpointClientHello>,
        writer: ClientWriter,
    },
    Message {
        client_id: u64,
        message: ClientMessage,
    },
    Disconnected {
        client_id: u64,
    },
    WriterDrained {
        client_id: u64,
    },
    ApiResponse(Box<EndpointApiResponse>),
}

pub(crate) fn handle_endpoint_handshake(
    mut stream: LocalStream,
    client_id: u64,
    hello: EndpointClientHello,
    methods: Vec<String>,
    mut capabilities: Vec<String>,
    event_tx: &mpsc::Sender<EndpointTransportEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    if hello.surface_reuse {
        capabilities.push(crate::protocol::surface_reuse::CAPABILITY.into());
    }
    if hello.surface_reuse && hello.surface_delta {
        capabilities.push(crate::protocol::surface_delta::CAPABILITY.into());
    }
    let welcome = if hello
        .cell_width_px
        .checked_mul(u32::from(hello.surface_size.cols))
        .is_none()
        || hello
            .cell_height_px
            .checked_mul(u32::from(hello.surface_size.rows))
            .is_none()
    {
        endpoint::EndpointServerWelcome::incompatible(
            "invalid_surface_geometry",
            "endpoint pixel geometry exceeds the terminal pixel representation",
        )
    } else {
        endpoint::negotiate_core(&hello, methods, capabilities)
    };
    let message = ServerMessage::EndpointControl {
        kind: endpoint::ENDPOINT_WELCOME_KIND.into(),
        data: serde_json::to_string(&welcome).map_err(io::Error::other)?,
    };
    crate::protocol::write_message(&mut stream, &message).map_err(io::Error::other)?;
    if welcome.error.is_some() {
        return Ok(());
    }
    set_client_recv_timeout(&stream, None, "endpoint handshake completed", client_id)?;
    let writer_events = event_tx.clone();
    let writer = ClientWriter::spawn(stream.try_clone()?, move |signal| {
        let event = match signal {
            ClientWriterSignal::Drained => EndpointTransportEvent::WriterDrained { client_id },
            ClientWriterSignal::Disconnected => EndpointTransportEvent::Disconnected { client_id },
        };
        let _ = writer_events.blocking_send(event);
    })?;
    let control = writer.control.clone();
    if event_tx
        .blocking_send(EndpointTransportEvent::Connected {
            client_id,
            hello: Box::new(hello),
            writer,
        })
        .is_err()
    {
        return Ok(());
    }
    while !should_quit.load(Ordering::Acquire) {
        let message = match crate::protocol::read_message::<_, ClientMessage>(
            &mut stream,
            crate::protocol::MAX_GRAPHICS_FRAME_SIZE,
        ) {
            Ok(message) if input_within_limits(&message) => message,
            Ok(_) => {
                tracing::warn!(client_id, "endpoint input exceeds transport limits");
                break;
            }
            Err(error) => {
                tracing::debug!(client_id, %error, "endpoint read ended");
                break;
            }
        };
        if matches!(message, ClientMessage::Detach) {
            break;
        }
        if let ClientMessage::EndpointControl { kind, data } = &message {
            if kind == endpoint::HEALTH_PING_KIND {
                let pong = ServerMessage::EndpointControl {
                    kind: endpoint::HEALTH_PONG_KIND.into(),
                    data: data.clone(),
                };
                let mut bytes = Vec::new();
                crate::protocol::write_message(&mut bytes, &pong).map_err(io::Error::other)?;
                if control.send(bytes).is_err() {
                    break;
                }
                continue;
            }
        }
        if event_tx
            .blocking_send(EndpointTransportEvent::Message { client_id, message })
            .is_err()
        {
            break;
        }
    }
    let _ = event_tx.blocking_send(EndpointTransportEvent::Disconnected { client_id });
    Ok(())
}

fn input_within_limits(message: &ClientMessage) -> bool {
    match message {
        ClientMessage::ClientShellPaneInput { events, .. }
        | ClientMessage::ClientShellPopupInput { events, .. } => {
            let mut bytes = 0usize;
            events.len() <= MAX_INPUT_EVENT_BATCH
                && events.iter().all(|event| {
                    bytes = bytes.saturating_add(match event {
                        ClientPaneInputEvent::Paste(text)
                        | ClientPaneInputEvent::TextCommit(text) => text.len(),
                        ClientPaneInputEvent::Key { generated_text, .. } => {
                            generated_text.as_ref().map_or(0, String::len)
                        }
                        ClientPaneInputEvent::Mouse { .. } => 0,
                    });
                    bytes <= MAX_INPUT_PAYLOAD
                })
        }
        ClientMessage::ClipboardImage { data, .. } => {
            data.len() <= crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD
        }
        ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
            boot_id.len() <= endpoint::MAX_ENDPOINT_BOOT_ID_BYTES
                && request.len() <= endpoint::MAX_ENDPOINT_COMMAND_BYTES
                && serde_json::from_str::<serde_json::Value>(request).is_ok_and(|value| {
                    value
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id.len() <= endpoint::MAX_ENDPOINT_REQUEST_ID_BYTES)
                        && value
                            .get("method")
                            .is_some_and(serde_json::Value::is_string)
                })
        }
        ClientMessage::ClientShellResize { .. }
        | ClientMessage::ClientShellHostTheme { .. }
        | ClientMessage::ClientShellFocus { .. }
        | ClientMessage::ClientShellMouseCapture { .. }
        | ClientMessage::EndpointControl { .. }
        | ClientMessage::Detach
        | ClientMessage::GraphicsTransmissionStarted { .. }
        | ClientMessage::GraphicsTransmissionResult { .. } => true,
        // These envelopes belong to the private/direct-terminal transport, not a shell endpoint.
        _ => false,
    }
}

/// Read exactly one initial frame before selecting the private or stable transport.
pub(crate) fn read_endpoint_hello(
    stream: &mut LocalStream,
    client_id: u64,
) -> Result<crate::protocol::endpoint_wire::InitialHello, crate::protocol::FramingError> {
    stream.set_nonblocking(false)?;
    set_client_recv_timeout(
        stream,
        Some(HANDSHAKE_TIMEOUT),
        "endpoint initial hello",
        client_id,
    )?;
    crate::protocol::endpoint_wire::read_initial_hello(stream)
}

/// Select the codec once at the shared listening socket, before client registration.
pub(crate) fn handle_socket_handshake(
    mut stream: LocalStream,
    client_id: u64,
    private_events: &mpsc::Sender<super::client_transport::ServerEvent>,
    endpoint_events: &mpsc::Sender<EndpointTransportEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    match read_endpoint_hello(&mut stream, client_id).map_err(io::Error::other)? {
        crate::protocol::endpoint_wire::InitialHello::Private(hello) => {
            super::client_transport::handle_private_hello(
                stream,
                client_id,
                hello,
                private_events,
                should_quit,
            )
        }
        crate::protocol::endpoint_wire::InitialHello::Endpoint(hello) => handle_endpoint_handshake(
            stream,
            client_id,
            hello,
            super::headless::endpoints::supported_methods()
                .iter()
                .map(|method| (*method).into())
                .collect(),
            vec![
                endpoint::HEALTH_CHECK_CAPABILITY.into(),
                endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                crate::protocol::endpoint_jobs::JOBS_PROJECTION_CAPABILITY.into(),
                crate::protocol::endpoint_decisions::DECISIONS_PROJECTION_CAPABILITY.into(),
            ],
            endpoint_events,
            should_quit,
        ),
    }
}
