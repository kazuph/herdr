use std::io;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;

use crate::ipc::LocalStream;
use crate::protocol::endpoint::{self, EndpointClientHello, EndpointServerWelcome};
use crate::protocol::endpoint_wire::{ClientMessage, ClientSurfaceSize, ServerMessage};

// Fixed upstream client/handshake.rs; these are distinct from private wire version checks.
const LOCAL_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub(crate) struct EndpointConnectOptions {
    pub(crate) surface_size: ClientSurfaceSize,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
    pub(crate) pixel_geometry_exact: bool,
    pub(crate) endpoint_keybindings: bool,
    pub(crate) mouse_capture: bool,
    pub(crate) surface_active: bool,
}

fn validate_welcome(welcome: &EndpointServerWelcome) -> io::Result<()> {
    if let Some(error) = &welcome.error {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{}: {}", error.code, error.message),
        ));
    }
    let client = endpoint::EndpointHello {
        generation: endpoint::ENDPOINT_PROTOCOL_GENERATION,
        min_generation: endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION,
        client_version: crate::build_info::version(),
        cols: 0,
        rows: 0,
        capabilities: Vec::new(),
    };
    let range = endpoint::negotiate_with(
        &client,
        endpoint::EndpointGenerationRange::at(welcome.generation, welcome.min_generation),
        Vec::new(),
    );
    if let Some(error) = range.error {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{}: {}", error.code, error.message),
        ));
    }
    if welcome.snapshot_codec != endpoint::SNAPSHOT_CODEC_V1
        || welcome.surface_codec != endpoint::SURFACE_CODEC_V1
        || welcome.input_codec != endpoint::INPUT_CODEC_V1
        || welcome.blob_codec != endpoint::BLOB_CODEC_V1
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "endpoint has no compatible core codecs",
        ));
    }
    Ok(())
}

/// Pin the stable codec once. A failed handshake never retries the private wire.
pub(crate) fn connect(
    stream: &mut LocalStream,
    options: EndpointConnectOptions,
    local_transport: bool,
) -> io::Result<EndpointServerWelcome> {
    stream.set_nonblocking(false)?;
    let hello = EndpointClientHello {
        generation: endpoint::ENDPOINT_PROTOCOL_GENERATION,
        min_generation: endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION,
        cell_width_px: options.cell_width_px,
        cell_height_px: options.cell_height_px,
        surface_size: options.surface_size,
        pixel_mouse: options.pixel_geometry_exact && cfg!(unix),
        direct_graphics: local_transport
            && options.pixel_geometry_exact
            && options.cell_width_px > 0
            && options.cell_height_px > 0
            && super::super::direct_graphics_profile_allowed(false),
        endpoint_keybindings: options.endpoint_keybindings,
        mouse_capture: options.mouse_capture,
        surface_active: options.surface_active,
        surface_reuse: true,
        surface_delta: true,
        surface_scroll: true,
        snapshot_codecs: vec![endpoint::SNAPSHOT_CODEC_V1.into()],
        surface_codecs: vec![endpoint::SURFACE_CODEC_V1.into()],
        input_codecs: vec![endpoint::INPUT_CODEC_V1.into()],
        blob_codecs: vec![endpoint::BLOB_CODEC_V1.into()],
    };
    crate::protocol::write_message(
        stream,
        &ClientMessage::EndpointControl {
            kind: endpoint::ENDPOINT_HELLO_KIND.into(),
            data: serde_json::to_string(&hello).map_err(io::Error::other)?,
        },
    )
    .map_err(framing_error)?;
    let timeout = if local_transport {
        LOCAL_HANDSHAKE_READ_TIMEOUT
    } else {
        REMOTE_HANDSHAKE_READ_TIMEOUT
    };
    super::super::set_handshake_recv_timeout(
        stream,
        Some(timeout),
        "endpoint handshake read timeout",
    )
    .map_err(io::Error::other)?;
    let message: ServerMessage =
        crate::protocol::read_message(stream, crate::protocol::MAX_FRAME_SIZE)
            .map_err(framing_error)?;
    super::super::set_handshake_recv_timeout(stream, None, "clear endpoint handshake timeout")
        .map_err(io::Error::other)?;
    let ServerMessage::EndpointControl { kind, data } = message else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected stable endpoint welcome",
        ));
    };
    if kind != endpoint::ENDPOINT_WELCOME_KIND {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected stable endpoint welcome control",
        ));
    }
    let welcome: EndpointServerWelcome = serde_json::from_str(&data)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    validate_welcome(&welcome)?;
    Ok(welcome)
}

fn framing_error(error: crate::protocol::FramingError) -> io::Error {
    match error {
        crate::protocol::FramingError::Io(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_handshake_retains_range_and_missing_method_independence() {
        let mut welcome =
            EndpointServerWelcome::compatible(vec!["workspace.list".into()], Vec::new());
        assert!(validate_welcome(&welcome).is_ok());
        welcome.generation += 1;
        assert!(validate_welcome(&welcome).is_ok());
        assert_eq!(welcome.methods, ["workspace.list"]);
        welcome.min_generation = welcome.generation;
        assert_eq!(
            validate_welcome(&welcome).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        welcome.min_generation = endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION;
        welcome.input_codec = "future.core".into();
        assert_eq!(
            validate_welcome(&welcome).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
