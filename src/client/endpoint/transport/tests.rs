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
