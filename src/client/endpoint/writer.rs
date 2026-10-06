// Ported from fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;

use super::EndpointTransport;
use crate::ipc::LocalStream;
use crate::protocol::endpoint_wire::ClientMessage;

const MAX_QUEUED_BATCHES: usize = 256;
const MAX_BATCH_BYTES: usize = 64 * 1024;
const MAX_QUEUED_BYTES: usize = 2 * crate::protocol::MAX_GRAPHICS_FRAME_SIZE;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const IO_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Default)]
struct FrameBatch {
    frames: Vec<Vec<u8>>,
    bytes: usize,
}

enum WriterCommand {
    Frames(Arc<Mutex<FrameBatch>>),
    Flush(mpsc::Sender<()>),
}

/// The UI batches complete frames until the worker claims them, so a short burst of tiny input
/// frames does not exhaust command slots. Frames retain their individual write boundaries.
/// A worker owns partial writes, cancellation, and the bridge lifetime; socket backpressure and
/// bridge teardown never block other endpoints.
pub(crate) struct NativeEndpointTransport {
    sender: mpsc::SyncSender<WriterCommand>,
    pending_batch: Option<Arc<Mutex<FrameBatch>>>,
    queued_bytes: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    error: Arc<Mutex<Option<io::Error>>>,
}

impl NativeEndpointTransport {
    pub(crate) fn with_lifetime(
        mut stream: LocalStream,
        lifetime: impl Send + 'static,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        let (sender, receiver) = mpsc::sync_channel::<WriterCommand>(MAX_QUEUED_BATCHES);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let worker_bytes = queued_bytes.clone();
        let worker_stop = stopped.clone();
        let worker_error = error.clone();
        std::thread::Builder::new()
            .name("endpoint-writer".into())
            .spawn(move || {
                let _lifetime = lifetime;
                while let Ok(command) = receiver.recv() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let batch = match command {
                        WriterCommand::Frames(batch) => batch,
                        WriterCommand::Flush(done) => {
                            let _ = done.send(());
                            continue;
                        }
                    };
                    let result = write_batch(&mut stream, &batch, &worker_stop, &worker_bytes);
                    if let Err(error) = result {
                        if let Ok(mut slot) = worker_error.lock() {
                            *slot = Some(error);
                        }
                        worker_stop.store(true, Ordering::Release);
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender,
            pending_batch: None,
            queued_bytes,
            stopped,
            error,
        })
    }

    fn enqueue_frame(&mut self, frame: Vec<u8>) -> io::Result<()> {
        if let Some(batch) = &self.pending_batch {
            let mut batch = batch
                .lock()
                .map_err(|_| io::Error::other("endpoint batch lock poisoned"))?;
            // An empty batch has already been claimed by the worker. Never append to it.
            if !batch.frames.is_empty()
                && frame.len() <= MAX_BATCH_BYTES.saturating_sub(batch.bytes)
            {
                batch.bytes += frame.len();
                batch.frames.push(frame);
                return Ok(());
            }
        }
        let batch = Arc::new(Mutex::new(FrameBatch {
            bytes: frame.len(),
            frames: vec![frame],
        }));
        self.sender
            .try_send(WriterCommand::Frames(batch.clone()))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => queue_full(),
                mpsc::TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "endpoint writer stopped")
                }
            })?;
        self.pending_batch = Some(batch);
        Ok(())
    }

    pub(crate) fn stop_handle(&self) -> Arc<AtomicBool> {
        self.stopped.clone()
    }
}

impl EndpointTransport for NativeEndpointTransport {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "endpoint writer stopped",
            ));
        }
        let mut frame = Vec::new();
        crate::protocol::write_message(&mut frame, message)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let len = frame.len();
        if self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bytes| {
                bytes
                    .checked_add(len)
                    .filter(|total| *total <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            return Err(queue_full());
        }
        self.enqueue_frame(frame).inspect_err(|_| {
            self.queued_bytes.fetch_sub(len, Ordering::AcqRel);
        })
    }

    fn disconnect(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn flush(&mut self, deadline: Instant) -> io::Result<()> {
        // Later frames must stay after the flush command, even if its wait times out.
        self.pending_batch = None;
        let (done, completion) = mpsc::channel();
        self.sender
            .try_send(WriterCommand::Flush(done))
            .map_err(|_| queue_full())?;
        completion
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, "endpoint flush timed out")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "endpoint writer stopped")
                }
            })
    }

    fn take_error(&mut self) -> Option<io::Error> {
        self.error.lock().ok()?.take()
    }
}

impl Drop for NativeEndpointTransport {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn queue_full() -> io::Error {
    // A message may already be partially written. Retrying or dropping only this message would
    // lose input ordering; revoke the connection and recover through the normal lifecycle.
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "endpoint output queue is full",
    )
}

fn write_batch(
    writer: &mut impl io::Write,
    batch: &Mutex<FrameBatch>,
    stopped: &AtomicBool,
    queued_bytes: &AtomicUsize,
) -> io::Result<()> {
    // Claim the frames before doing any I/O. The producer never waits for socket progress.
    let batch = std::mem::take(
        &mut *batch
            .lock()
            .map_err(|_| io::Error::other("endpoint batch lock poisoned"))?,
    );
    for frame in batch.frames {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        let result = write_frame(writer, &frame, stopped);
        queued_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
        result?;
    }
    Ok(())
}

fn write_frame(
    writer: &mut impl io::Write,
    mut frame: &[u8],
    stopped: &AtomicBool,
) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    #[cfg(windows)]
    let mut deadline = deadline;
    while !frame.is_empty() && !stopped.load(Ordering::Acquire) {
        // Match interprocess's 512-byte pipe buffer hint: larger nonblocking Windows
        // writes can make no progress when the peer polls instead of blocking on read.
        #[cfg(windows)]
        let chunk = &frame[..frame.len().min(512)];
        #[cfg(not(windows))]
        let chunk = frame;
        match writer.write(chunk) {
            Ok(0) => {}
            Ok(written) => {
                frame = &frame[written..];
                #[cfg(windows)]
                {
                    deadline = Instant::now() + WRITE_TIMEOUT;
                }
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "endpoint write timed out",
            ));
        }
        std::thread::sleep(IO_POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::endpoint::{
        ClientEndpointId, EndpointNegotiation, EndpointRegistry, EndpointSendOutcome,
    };
    use interprocess::local_socket::traits::Listener as _;

    struct OwnedSocket(std::path::PathBuf);

    impl Drop for OwnedSocket {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    struct Lifetime(mpsc::Sender<()>);

    impl Drop for Lifetime {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn streams() -> (LocalStream, LocalStream, OwnedSocket) {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::PathBuf::from("/tmp").join(format!(
            "herdr-endpoint-{}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let listener = crate::ipc::bind_local_listener(&path).expect("isolated listener");
        let client = crate::ipc::connect_local_stream(&path).expect("real client stream");
        let server = listener.accept().expect("real server stream");
        server
            .set_recv_timeout(Some(WRITE_TIMEOUT))
            .expect("bound failed test read");
        (client, server, OwnedSocket(path))
    }

    #[test]
    fn native_writer_preserves_whole_frame_order_and_releases_owned_lifetime() {
        let (client, mut peer, _socket) = streams();
        let (released, release) = mpsc::channel();
        let mut writer =
            NativeEndpointTransport::with_lifetime(client, Lifetime(released)).expect("writer");
        let frames = [
            ClientMessage::ClientShellPaneInput {
                pane_id: "opaque:pane".into(),
                events: vec![crate::protocol::endpoint_wire::ClientPaneInputEvent::Paste(
                    "日本語 marker".into(),
                )],
            },
            ClientMessage::ClientShellResize {
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: crate::protocol::endpoint_wire::ClientSurfaceSize {
                    cols: 80,
                    rows: 24,
                },
                pixel_mouse: true,
            },
            ClientMessage::Detach,
        ];
        for message in &frames {
            writer.send(message).expect("enqueue whole frame");
        }
        writer
            .flush(Instant::now() + WRITE_TIMEOUT)
            .expect("all bytes written");
        for expected in frames {
            let actual: ClientMessage =
                crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE)
                    .expect("read genuine socket frame");
            assert_eq!(actual, expected);
        }
        drop(writer);
        release
            .recv_timeout(WRITE_TIMEOUT)
            .expect("owned bridge lifetime released");
    }

    #[test]
    fn unread_socket_backpressure_revokes_only_its_endpoint_without_mutation_replay() {
        let (local_stream, mut local_peer, _local_socket) = streams();
        let (remote_stream, _unread_remote_peer, _remote_socket) = streams();
        let (released, release) = mpsc::channel();
        let local = NativeEndpointTransport::with_lifetime(local_stream, ()).expect("local writer");
        let remote = NativeEndpointTransport::with_lifetime(remote_stream, Lifetime(released))
            .expect("remote writer");
        let negotiation = || EndpointNegotiation::new(Vec::new(), Vec::new());
        let mut registry = EndpointRegistry::new(local, 1, negotiation());
        let remote_id = ClientEndpointId::Ssh("existing-fork-profile-id".into());
        registry.insert(remote_id.clone(), remote, 2, negotiation(), false);
        let blocked = ClientMessage::ClientShellPaneInput {
            pane_id: "remote:pane".into(),
            events: vec![crate::protocol::endpoint_wire::ClientPaneInputEvent::Paste(
                "R".repeat(MAX_BATCH_BYTES),
            )],
        };
        let maximum_attempts = MAX_QUEUED_BYTES / MAX_BATCH_BYTES + MAX_QUEUED_BATCHES;
        let mut refused = false;
        for _ in 0..maximum_attempts {
            if registry.send_to(&remote_id, &blocked) == EndpointSendOutcome::NotSent {
                refused = true;
                break;
            }
        }
        assert!(
            refused,
            "real unread socket must exhaust a bounded writer lane"
        );
        assert!(!registry.accepts(&remote_id, 2));
        assert!(registry.accepts(&ClientEndpointId::Local, 1));
        assert_eq!(registry.active_id(), &ClientEndpointId::Local);
        let failures = registry.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].endpoint_id, remote_id);
        assert_eq!(failures[0].generation, 2);
        let marker = ClientMessage::ClientShellPaneInput {
            pane_id: "local:pane".into(),
            events: vec![crate::protocol::endpoint_wire::ClientPaneInputEvent::Paste(
                "LOCAL-AFTER-REMOTE-STALL".into(),
            )],
        };
        assert_eq!(registry.send(&marker), EndpointSendOutcome::Sent);
        let received: ClientMessage =
            crate::protocol::read_message(&mut local_peer, crate::protocol::MAX_FRAME_SIZE)
                .expect("healthy lane remains usable");
        assert_eq!(received, marker);
        release
            .recv_timeout(WRITE_TIMEOUT)
            .expect("stalled bridge released after cancellation");
        assert!(registry.take_failures().is_empty());
    }
}
