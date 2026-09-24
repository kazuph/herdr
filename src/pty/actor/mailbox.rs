use std::io::{self, Write};
use std::sync::{Arc, Mutex};

pub(super) type MailboxCallback = Box<dyn FnOnce(io::Result<()>) + Send>;
pub(super) type MailboxCompletion = Arc<Mutex<Option<MailboxCallback>>>;
pub(super) type PendingMailboxCompletions = Arc<Mutex<Vec<MailboxCompletion>>>;

pub(super) fn write_mailbox_input(
    writer: &mut impl Write,
    bytes: &[u8],
    pending: &PendingMailboxCompletions,
    completion: &MailboxCompletion,
    accepting: &Arc<Mutex<bool>>,
) -> bool {
    // Claim a write while input is accepted. Shutdown cancels only unclaimed
    // completions; holding either lock through PTY I/O would block shutdown too.
    let callback = {
        let accepting = accepting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !*accepting {
            return false;
        }
        let Some(callback) = completion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return false;
        };
        callback
    };
    let result = writer.write_all(bytes).and_then(|()| writer.flush());
    let failed = result.is_err();
    remove_pending_mailbox_completion(pending, completion);
    callback(result);
    failed
}

pub(super) fn remove_pending_mailbox_completion(
    pending: &PendingMailboxCompletions,
    target: &MailboxCompletion,
) {
    pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|completion| !Arc::ptr_eq(completion, target));
}

pub(super) fn fail_windows_writer(
    accepting: &Arc<Mutex<bool>>,
    pending: &PendingMailboxCompletions,
) {
    let mut accepting = accepting
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *accepting = false;
    let callbacks = {
        let mut pending = pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending
            .drain(..)
            .filter_map(|completion| {
                completion
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
            })
            .collect::<Vec<_>>()
    };
    drop(accepting);
    for callback in callbacks {
        callback(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "windows PTY writer stopped before mailbox write completed",
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn windows_mailbox_shutdown_does_not_write_failed_input() {
        let accepting = Arc::new(Mutex::new(true));
        let (tx, rx) = mpsc::channel();
        let completion: MailboxCompletion = Arc::new(Mutex::new(Some(Box::new(move |result| {
            tx.send(result).unwrap();
        }))));
        let pending = Arc::new(Mutex::new(vec![Arc::clone(&completion)]));
        let path = std::env::temp_dir().join(format!(
            "herdr-mailbox-{}-{}",
            std::process::id(),
            crate::terminal::TerminalId::alloc()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        fail_windows_writer(&accepting, &pending);
        assert_eq!(
            rx.recv().unwrap().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        write_mailbox_input(
            &mut file,
            b"must not be delivered\r",
            &pending,
            &completion,
            &accepting,
        );
        fail_windows_writer(&accepting, &pending);
        assert!(rx.try_recv().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn windows_mailbox_shutdown_does_not_wait_for_blocked_write() {
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};
        let (mut writer, peer) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        loop {
            match writer.write(&[0; 8192]) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("prefill failed: {err}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let accepting = Arc::new(Mutex::new(true));
        let (tx, rx) = mpsc::channel();
        let completion: MailboxCompletion = Arc::new(Mutex::new(Some(Box::new(move |result| {
            tx.send(result).unwrap();
        }))));
        let pending = Arc::new(Mutex::new(vec![Arc::clone(&completion)]));
        let pending_write = Arc::clone(&pending);
        let write_completion = Arc::clone(&completion);
        let writing_accepting = Arc::clone(&accepting);
        let writing = std::thread::spawn(move || {
            write_mailbox_input(
                &mut writer,
                b"mail\r",
                &pending_write,
                &write_completion,
                &writing_accepting,
            )
        });
        let began = Instant::now();
        loop {
            match completion.try_lock() {
                Ok(completion) if completion.is_some() => {}
                _ => break,
            }
            assert!(
                began.elapsed() < Duration::from_secs(1),
                "writer did not start"
            );
            std::thread::yield_now();
        }
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stopping = std::thread::spawn(move || {
            fail_windows_writer(&accepting, &pending);
            stopped_tx.send(()).unwrap();
        });
        let stopped = stopped_rx.recv_timeout(Duration::from_secs(1));
        // Always release the blocked OS write before an assertion can unwind.
        drop(peer);
        writing.join().unwrap();
        stopping.join().unwrap();
        assert!(stopped.is_ok(), "shutdown waited for blocked mailbox I/O");
        assert!(rx.recv().unwrap().is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn windows_mailbox_success_is_written_and_acknowledged_once() {
        let accepting = Arc::new(Mutex::new(true));
        let (tx, rx) = mpsc::channel();
        let completion: MailboxCompletion = Arc::new(Mutex::new(Some(Box::new(move |result| {
            tx.send(result).unwrap();
        }))));
        let pending = Arc::new(Mutex::new(vec![Arc::clone(&completion)]));
        let path = std::env::temp_dir().join(format!(
            "herdr-mailbox-{}-{}",
            std::process::id(),
            crate::terminal::TerminalId::alloc()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        assert!(!write_mailbox_input(
            &mut file,
            b"delivered\r",
            &pending,
            &completion,
            &accepting
        ));
        assert!(rx.recv().unwrap().is_ok());
        fail_windows_writer(&accepting, &pending);
        assert!(rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"delivered\r");
        std::fs::remove_file(path).unwrap();
    }
}
