use super::super::EndpointTransport as _;
use super::*;
use crate::protocol::endpoint_wire::ClientMessage;
use interprocess::local_socket::traits::{Listener as _, Stream as _};
use std::io::Write as _;
use std::sync::atomic::AtomicUsize;

// Owned test socket bound mirrors the existing server's four-second handshake test bound.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

struct OwnedSocket(std::path::PathBuf);

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn pair() -> (LocalStream, LocalStream, OwnedSocket) {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "herdr-reader-{}-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    let listener = crate::ipc::bind_local_listener(&path).unwrap();
    let client = crate::ipc::connect_local_stream(&path).unwrap();
    let peer = listener.accept().unwrap();
    peer.set_recv_timeout(Some(TIMEOUT)).unwrap();
    (client, peer, OwnedSocket(path))
}

#[tokio::test]
async fn endpoint_reader_fragmented_frame_keeps_generation_and_cancels_partial_old_connection() {
    let (client, mut peer, _socket) = pair();
    let (events, mut received) = mpsc::channel(8);
    let endpoint = ClientEndpointId::Ssh("m-fragmented".into());
    let mut transport = start(
        client,
        (),
        endpoint.clone(),
        12,
        &EndpointNegotiation::default(),
        events,
    )
    .unwrap();
    let message = ServerMessage::EndpointControl {
        kind: crate::protocol::endpoint::HEALTH_PONG_KIND.into(),
        data: "fragment-marker".into(),
    };
    let mut bytes = Vec::new();
    crate::protocol::write_message(&mut bytes, &message).unwrap();
    let (prefix_written, prefix_received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        peer.write_all(&bytes[..2]).unwrap();
        prefix_written.send(()).unwrap();
        let input: ClientMessage =
            crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE).unwrap();
        assert_eq!(input, ClientMessage::ClientShellFocus { focused: true });
        peer.write_all(&bytes[2..]).unwrap();
        peer
    });
    prefix_received.recv_timeout(TIMEOUT).unwrap();
    transport
        .send(&ClientMessage::ClientShellFocus { focused: true })
        .unwrap();
    let event = tokio::time::timeout(TIMEOUT, received.recv())
        .await
        .unwrap()
        .unwrap();
    let EndpointReaderEvent::Message {
        endpoint_id,
        generation,
        message: actual,
    } = event
    else {
        panic!("complete fragmented frame expected")
    };
    assert_eq!(endpoint_id, endpoint);
    assert_eq!(generation, 12);
    assert_eq!(*actual, message);
    let mut peer = worker.join().unwrap();
    // Cancel while a subsequent frame has only half its length prefix. No partial decode is emitted.
    peer.write_all(&[1, 0]).unwrap();
    drop(transport);
    assert!(tokio::time::timeout(TIMEOUT, received.recv())
        .await
        .unwrap()
        .is_none());

    let (replacement, mut replacement_peer, _replacement_socket) = pair();
    let (events, mut received) = mpsc::channel(8);
    let replacement = start(
        replacement,
        (),
        endpoint.clone(),
        13,
        &EndpointNegotiation::default(),
        events,
    )
    .unwrap();
    crate::protocol::write_message(&mut replacement_peer, &message).unwrap();
    let event = tokio::time::timeout(TIMEOUT, received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, EndpointReaderEvent::Message { endpoint_id, generation: 13, .. } if endpoint_id == endpoint)
    );
    drop(replacement_peer);
    let event = tokio::time::timeout(TIMEOUT, received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, EndpointReaderEvent::Disconnected { endpoint_id, generation: 13, error } if endpoint_id == endpoint && error.kind() == io::ErrorKind::UnexpectedEof)
    );
    drop(replacement);
}

mod patch_rebuild {
    use super::super::PatchRebuild;
    use crate::kitty_graphics::patch::{self, GraphicsPatch};
    use crate::protocol::endpoint_wire::{
        PaneSurfaceFrame, ServerMessage, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
        SurfaceGraphicsPlacement, SurfaceGraphicsScene, SurfaceGraphicsSource,
        SurfaceGraphicsTarget,
    };
    use crate::protocol::FrameData;

    const W: u32 = 512;
    const H: u32 = 256;

    fn key(fingerprint: u64) -> SurfaceGraphicsAssetKey {
        SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "p1".into(),
                },
                image_id: 7,
            },
            image_width: W,
            image_height: H,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: u64::from(W * H * 4),
            data_fingerprint: fingerprint,
        }
    }

    fn message(patch: &GraphicsPatch) -> ServerMessage {
        ServerMessage::EndpointControl {
            kind: crate::protocol::endpoint::GRAPHICS_PATCH_KIND.into(),
            data: serde_json::to_string(patch).unwrap(),
        }
    }

    fn frame(placed: &SurfaceGraphicsAssetKey) -> ServerMessage {
        ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData::from_ratatui_buffer(
                &ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 4, 2)),
                None,
            ),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene {
                assets: Vec::new(),
                placements: vec![SurfaceGraphicsPlacement {
                    asset: placed.clone(),
                    logical_placement_id: 1,
                    x: 0,
                    y: 0,
                    cols: 4,
                    rows: 2,
                    source_x: 0,
                    source_y: 0,
                    source_width: W,
                    source_height: H,
                    x_offset: 0,
                    y_offset: 0,
                    z: 0,
                    scrollback_offset: 0,
                }],
                retained_assets: Vec::new(),
            },
        })
    }

    fn image_of(messages: &[ServerMessage], key: &SurfaceGraphicsAssetKey) -> Option<Vec<u8>> {
        messages.iter().find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => surface
                .graphics
                .assets
                .iter()
                .find(|asset| asset.key == *key)
                .map(|asset| asset.data.clone()),
            _ => None,
        })
    }

    #[test]
    fn the_reader_rebuilds_whole_and_changed_rectangle_images_into_their_frame() {
        let first = vec![200u8; (W * H * 4) as usize];
        let mut second = first.clone();
        for byte in &mut second[..4 * 64] {
            *byte = 7;
        }
        let mut rebuild = PatchRebuild::default();

        let whole = GraphicsPatch::whole(&key(1), &first).unwrap();
        assert!(rebuild.receive(message(&whole)).is_empty());
        let out = rebuild.receive(frame(&key(1)));
        assert_eq!(out.len(), 1, "only the frame is delivered");
        assert_eq!(image_of(&out, &key(1)).as_deref(), Some(&first[..]));

        let changed = patch::diff(&key(1), &first, &key(2), &second).unwrap();
        assert!(!changed.is_whole());
        assert!(rebuild.receive(message(&changed)).is_empty());
        let out = rebuild.receive(frame(&key(2)));
        assert_eq!(image_of(&out, &key(2)).as_deref(), Some(&second[..]));
    }

    #[test]
    fn a_patch_without_its_base_reaches_the_event_loop_ahead_of_the_frame() {
        let base = vec![1u8; (W * H * 4) as usize];
        let mut next = base.clone();
        next[0] = 9;
        let mut rebuild = PatchRebuild::default();
        let orphan = patch::diff(&key(1), &base, &key(2), &next).unwrap();
        assert!(rebuild.receive(message(&orphan)).is_empty());
        let out = rebuild.receive(frame(&key(2)));
        assert_eq!(out.len(), 2);
        assert!(
            matches!(&out[0], ServerMessage::EndpointControl { kind, .. }
            if kind == crate::protocol::endpoint::GRAPHICS_PATCH_KIND)
        );
        assert!(matches!(&out[1], ServerMessage::PaneSurface(_)));
        assert_eq!(image_of(&out, &key(2)), None);
    }
}
