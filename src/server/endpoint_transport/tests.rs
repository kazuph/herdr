use std::time::{SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::Listener as _;

use super::*;

struct OwnedSocket(std::path::PathBuf);

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn streams() -> (LocalStream, LocalStream, OwnedSocket) {
    let path = std::path::PathBuf::from("/tmp").join(format!(
        "herdr-endpoint-server-{}-{}.sock",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let listener = crate::ipc::bind_local_listener(&path).unwrap();
    let client = crate::ipc::connect_local_stream(&path).unwrap();
    let server = listener.accept().unwrap();
    client.set_recv_timeout(Some(HANDSHAKE_TIMEOUT)).unwrap();
    (client, server, OwnedSocket(path))
}

fn hello() -> EndpointClientHello {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/upstream-gen1-endpoint-hello-v1.json"
    )))
    .unwrap()
}

fn send_hello(client: &mut LocalStream, hello: &EndpointClientHello) {
    crate::protocol::write_message(
        client,
        &ClientMessage::EndpointControl {
            kind: endpoint::ENDPOINT_HELLO_KIND.into(),
            data: serde_json::to_string(hello).unwrap(),
        },
    )
    .unwrap();
}

fn start(
    server: LocalStream,
    events: mpsc::Sender<EndpointTransportEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut server = server;
        let crate::protocol::endpoint_wire::InitialHello::Endpoint(hello) =
            read_endpoint_hello(&mut server, 7).unwrap()
        else {
            panic!("expected stable initial frame");
        };
        handle_endpoint_handshake(
            server,
            7,
            hello,
            Vec::new(),
            Vec::new(),
            &events,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    })
}

fn welcome(client: &mut LocalStream) -> endpoint::EndpointServerWelcome {
    let message: ServerMessage =
        crate::protocol::read_message(client, crate::protocol::MAX_FRAME_SIZE).unwrap();
    let ServerMessage::EndpointControl { kind, data } = message else {
        panic!("expected stable welcome");
    };
    assert_eq!(kind, endpoint::ENDPOINT_WELCOME_KIND);
    serde_json::from_str(&data).unwrap()
}

fn event(receiver: &mut mpsc::Receiver<EndpointTransportEvent>) -> EndpointTransportEvent {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(HANDSHAKE_TIMEOUT, receiver.recv())
                .await
                .unwrap()
                .unwrap()
        })
}

#[test]
fn actual_socket_handshake_writer_and_semantic_input_use_generation_one_bytes() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    send_hello(&mut client, &hello());
    let welcome = welcome(&mut client);
    assert_eq!(welcome.generation, 1);
    assert!(welcome.error.is_none());
    let EndpointTransportEvent::Connected {
        client_id,
        writer,
        hello,
    } = event(&mut receiver)
    else {
        panic!("expected socket connection");
    };
    assert_eq!(client_id, 7);
    assert!(hello.supports_required_codecs());
    let snapshot: crate::protocol::endpoint_wire::ClientShellSnapshot =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
    let snapshot_message = endpoint::snapshot_message(&snapshot).unwrap();
    let mut framed_snapshot = Vec::new();
    crate::protocol::write_message(&mut framed_snapshot, &snapshot_message).unwrap();
    writer.control.send(framed_snapshot).unwrap();
    let received: ServerMessage =
        crate::protocol::read_message(&mut client, crate::protocol::MAX_FRAME_SIZE).unwrap();
    assert_eq!(received, snapshot_message);
    let input = ClientMessage::ClientShellPaneInput {
        pane_id: "opaque:remote/p1".into(),
        events: vec![ClientPaneInputEvent::Paste("日本語 target marker".into())],
    };
    crate::protocol::write_message(&mut client, &input).unwrap();
    let EndpointTransportEvent::Message { client_id, message } = event(&mut receiver) else {
        panic!("expected real socket input");
    };
    assert_eq!(client_id, 7);
    assert_eq!(message, input);
    crate::protocol::write_message(&mut client, &ClientMessage::Detach).unwrap();
    assert!(matches!(
        event(&mut receiver),
        EndpointTransportEvent::Disconnected { client_id: 7 }
    ));
    drop(writer);
    worker.join().unwrap();
}

#[test]
fn actual_socket_rejects_incompatible_floor_without_registering_or_falling_back() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    let mut hello = hello();
    hello.generation = 2;
    hello.min_generation = 2;
    send_hello(&mut client, &hello);
    assert!(welcome(&mut client).error.is_some());
    worker.join().unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn actual_socket_rejects_private_raw_input_on_a_pinned_endpoint_connection() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    send_hello(&mut client, &hello());
    assert!(welcome(&mut client).error.is_none());
    let connected = event(&mut receiver);
    assert!(matches!(
        &connected,
        EndpointTransportEvent::Connected { .. }
    ));
    crate::protocol::write_message(
        &mut client,
        &ClientMessage::Input {
            data: b"must-not-dispatch".to_vec(),
        },
    )
    .unwrap();
    assert!(matches!(
        event(&mut receiver),
        EndpointTransportEvent::Disconnected { client_id: 7 }
    ));
    drop(connected);
    worker.join().unwrap();
}

#[test]
fn actual_socket_refuses_oversized_semantic_paste_before_dispatch() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    send_hello(&mut client, &hello());
    assert!(welcome(&mut client).error.is_none());
    let connected = event(&mut receiver);
    assert!(matches!(
        &connected,
        EndpointTransportEvent::Connected { .. }
    ));
    crate::protocol::write_message(
        &mut client,
        &ClientMessage::ClientShellPaneInput {
            pane_id: "opaque:pane".into(),
            events: vec![ClientPaneInputEvent::Paste(
                "x".repeat(MAX_INPUT_PAYLOAD + 1),
            )],
        },
    )
    .unwrap();
    assert!(matches!(
        event(&mut receiver),
        EndpointTransportEvent::Disconnected { client_id: 7 }
    ));
    drop(connected);
    worker.join().unwrap();
}

#[test]
fn actual_socket_health_pong_does_not_wait_for_the_headless_state_loop() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    send_hello(&mut client, &hello());
    assert!(welcome(&mut client).error.is_none());
    let connected = event(&mut receiver);
    assert!(matches!(
        connected,
        EndpointTransportEvent::Connected { .. }
    ));
    crate::protocol::write_message(
        &mut client,
        &ClientMessage::EndpointControl {
            kind: endpoint::HEALTH_PING_KIND.into(),
            data: "owned socket probe".into(),
        },
    )
    .unwrap();
    let pong: ServerMessage =
        crate::protocol::read_message(&mut client, crate::protocol::MAX_FRAME_SIZE).unwrap();
    assert_eq!(
        pong,
        ServerMessage::EndpointControl {
            kind: endpoint::HEALTH_PONG_KIND.into(),
            data: "owned socket probe".into(),
        }
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    crate::protocol::write_message(&mut client, &ClientMessage::Detach).unwrap();
    assert!(matches!(
        event(&mut receiver),
        EndpointTransportEvent::Disconnected { client_id: 7 }
    ));
    drop(connected);
    worker.join().unwrap();
}

#[test]
fn actual_socket_rejects_overflowing_initial_pixel_geometry_before_registration() {
    let (mut client, server, _socket) = streams();
    let (sender, mut receiver) = mpsc::channel(MAX_INPUT_EVENT_BATCH);
    let worker = start(server, sender);
    let mut hello = hello();
    hello.surface_size.cols = u16::MAX;
    hello.cell_width_px = u32::MAX;
    send_hello(&mut client, &hello);
    let welcome = welcome(&mut client);
    assert_eq!(welcome.error.unwrap().code, "invalid_surface_geometry");
    worker.join().unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}
