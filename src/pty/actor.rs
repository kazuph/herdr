#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(any(windows, test))]
mod mailbox;

#[cfg(windows)]
mod windows {
    use super::mailbox::*;
    use std::io::{self, Read, Write};
    use std::sync::{mpsc as std_mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use portable_pty::{MasterPty, PtySize};
    use tokio::sync::mpsc;
    use tracing::{debug, warn};

    pub(crate) struct PtyReadResult {
        pub terminal_responses: Vec<Bytes>,
    }

    type ReadCallback = Box<dyn FnMut(&[u8]) -> PtyReadResult + Send + 'static>;
    type ReaderExitCallback = Box<dyn FnOnce() + Send + 'static>;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct PtyResize {
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    }

    struct PtyResizeRequest {
        resize: PtyResize,
        terminal_responses: Vec<Bytes>,
    }

    pub(crate) struct PtyIoActorConfig {
        pub pane_id: u32,
        pub master: Box<dyn MasterPty + Send>,
        pub initially_quiesced: bool,
        pub on_read: ReadCallback,
        pub on_reader_exit: Option<ReaderExitCallback>,
    }

    enum PtyIoControlCommand {
        Resize(PtyResizeRequest),
        Shutdown,
    }

    #[expect(
        clippy::enum_variant_names,
        reason = "Retain upstream input command names alongside the fork's WriteMailboxInput"
    )]
    enum PtyIoDataCommand {
        WriteUserInput(Bytes),
        SubmitUserInput {
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
        WriteMailboxInput(Bytes, MailboxCompletion),
    }

    enum PtyIoWriteCommand {
        Write(Bytes),
        WriteMailboxInput(Bytes, MailboxCompletion),
        SubmissionPart {
            bytes: Bytes,
            deadline: Option<Instant>,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
    }

    #[derive(Clone)]
    pub(crate) struct PtyIoActorHandle {
        data_tx: mpsc::Sender<PtyIoDataCommand>,
        control_tx: std_mpsc::Sender<PtyIoControlCommand>,
        accepting: Arc<Mutex<bool>>,
        pending_mailbox_completions: PendingMailboxCompletions,
    }

    impl PtyIoActorHandle {
        pub(crate) fn try_write_mailbox_input(
            &self,
            bytes: Bytes,
            completion: Box<dyn FnOnce(io::Result<()>) + Send>,
        ) -> io::Result<()> {
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*accepting {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "PTY actor is not accepting input",
                ));
            }
            let completion = Arc::new(Mutex::new(Some(completion)));
            self.pending_mailbox_completions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Arc::clone(&completion));
            if let Err(err) = self.data_tx.try_send(PtyIoDataCommand::WriteMailboxInput(
                bytes,
                Arc::clone(&completion),
            )) {
                remove_pending_mailbox_completion(&self.pending_mailbox_completions, &completion);
                return Err(match err {
                    mpsc::error::TrySendError::Full(_) => {
                        io::Error::new(io::ErrorKind::WouldBlock, "PTY actor input queue is full")
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        io::Error::new(io::ErrorKind::BrokenPipe, "PTY actor closed")
                    }
                });
            }
            Ok(())
        }

        pub(crate) async fn write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::SendError<Bytes>> {
            {
                let accepting = self
                    .accepting
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !*accepting {
                    return Err(mpsc::error::SendError(bytes));
                }
            }

            let permit = match self.data_tx.reserve().await {
                Ok(permit) => permit,
                Err(_) => return Err(mpsc::error::SendError(bytes)),
            };
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*accepting {
                return Err(mpsc::error::SendError(bytes));
            }
            permit.send(PtyIoDataCommand::WriteUserInput(bytes));
            Ok(())
        }

        pub(crate) fn try_write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
            if !*self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                return Err(mpsc::error::TrySendError::Closed(bytes));
            }
            self.data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(bytes))
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(command) => {
                        let PtyIoDataCommand::WriteUserInput(bytes) = command else {
                            unreachable!("queued write returned another command")
                        };
                        mpsc::error::TrySendError::Full(bytes)
                    }
                    mpsc::error::TrySendError::Closed(command) => {
                        let PtyIoDataCommand::WriteUserInput(bytes) = command else {
                            unreachable!("queued write returned another command")
                        };
                        mpsc::error::TrySendError::Closed(bytes)
                    }
                })
        }

        pub(crate) fn queue_user_input_submission(
            &self,
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
        ) -> std::io::Result<std_mpsc::Receiver<std::io::Result<()>>> {
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*accepting {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pty actor closed",
                ));
            }
            let (reply_tx, reply_rx) = std_mpsc::channel();
            self.data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text,
                    enter,
                    delay,
                    deadline,
                    reply: reply_tx,
                })
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(_) => std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "pty input queue is full",
                    ),
                    mpsc::error::TrySendError::Closed(_) => {
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
                    }
                })?;
            Ok(reply_rx)
        }

        pub(crate) fn resize(
            &self,
            rows: u16,
            cols: u16,
            cell_width_px: u32,
            cell_height_px: u32,
            terminal_responses: Vec<Bytes>,
        ) {
            let _ = self
                .control_tx
                .send(PtyIoControlCommand::Resize(PtyResizeRequest {
                    resize: PtyResize {
                        rows,
                        cols,
                        cell_width_px,
                        cell_height_px,
                    },
                    terminal_responses,
                }));
        }

        pub(crate) fn shutdown(&self) {
            fail_windows_writer(&self.accepting, &self.pending_mailbox_completions);
            let _ = self.control_tx.send(PtyIoControlCommand::Shutdown);
        }
    }

    pub(crate) struct PtyIoActor;

    impl PtyIoActor {
        pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
            let PtyIoActorConfig {
                pane_id,
                master,
                initially_quiesced,
                mut on_read,
                on_reader_exit,
            } = config;

            let mut reader = master
                .try_clone_reader()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let mut writer = master
                .take_writer()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let (data_tx, mut data_rx) = mpsc::channel::<PtyIoDataCommand>(1024);
            let (control_tx, control_rx) = std_mpsc::channel::<PtyIoControlCommand>();
            let (write_tx, write_rx) = std_mpsc::channel::<PtyIoWriteCommand>();
            let accepting = Arc::new(Mutex::new(!initially_quiesced));

            let pending_mailbox_completions = Arc::new(Mutex::new(Vec::new()));
            {
                let pending = Arc::clone(&pending_mailbox_completions);
                let accepting = Arc::clone(&accepting);
                std::thread::spawn(move || {
                    run_writer(&mut writer, write_rx, &pending, &accepting);
                    fail_windows_writer(&accepting, &pending);
                    debug!(pane_id, "windows pty writer thread exiting");
                });
            }

            {
                let write_tx = write_tx.clone();
                let accepting = Arc::clone(&accepting);
                std::thread::spawn(move || {
                    run_input_forwarder(&mut data_rx, write_tx, accepting);
                    debug!(pane_id, "windows pty input thread exiting");
                });
            }

            {
                let write_tx = write_tx.clone();
                let accepting = Arc::clone(&accepting);
                let pending = Arc::clone(&pending_mailbox_completions);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let result = on_read(&buf[..n]);
                                if result.terminal_responses.into_iter().any(|response| {
                                    write_tx.send(PtyIoWriteCommand::Write(response)).is_err()
                                }) {
                                    break;
                                }
                            }
                            Err(err) => {
                                debug!(pane_id, err = %err, "windows pty reader failed");
                                break;
                            }
                        }
                    }
                    fail_windows_writer(&accepting, &pending);
                    if let Some(on_reader_exit) = on_reader_exit {
                        on_reader_exit();
                    }
                    debug!(pane_id, "windows pty reader thread exiting");
                });
            }

            {
                let write_tx = write_tx.clone();
                std::thread::spawn(move || {
                    for command in control_rx {
                        match command {
                            PtyIoControlCommand::Resize(request) => {
                                let size = request.resize;
                                if let Err(err) = master.resize(PtySize {
                                    rows: size.rows,
                                    cols: size.cols,
                                    pixel_width: size.cell_width_px.min(u16::MAX as u32) as u16,
                                    pixel_height: size.cell_height_px.min(u16::MAX as u32) as u16,
                                }) {
                                    warn!(pane_id, err = %err, "windows pty resize failed");
                                }
                                if request.terminal_responses.into_iter().any(|response| {
                                    write_tx.send(PtyIoWriteCommand::Write(response)).is_err()
                                }) {
                                    break;
                                }
                            }
                            PtyIoControlCommand::Shutdown => break,
                        }
                    }
                    debug!(pane_id, "windows pty control thread exiting");
                });
            }

            Ok(PtyIoActorHandle {
                data_tx,
                control_tx,
                accepting,
                pending_mailbox_completions,
            })
        }
    }

    fn run_writer(
        writer: &mut impl Write,
        write_rx: std_mpsc::Receiver<PtyIoWriteCommand>,
        pending: &PendingMailboxCompletions,
        accepting: &Arc<Mutex<bool>>,
    ) {
        for command in write_rx {
            let result = match command {
                PtyIoWriteCommand::Write(bytes) => write_and_flush(writer, &bytes),
                PtyIoWriteCommand::WriteMailboxInput(bytes, completion) => {
                    if write_mailbox_input(writer, &bytes, pending, &completion, accepting) {
                        break;
                    }
                    continue;
                }
                PtyIoWriteCommand::SubmissionPart {
                    bytes,
                    deadline,
                    reply,
                } => {
                    let result = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        Err(input_submission_timed_out())
                    } else {
                        write_and_flush(writer, &bytes)
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                    continue;
                }
            };
            if result.is_err() {
                break;
            }
        }
    }

    fn run_input_forwarder(
        data_rx: &mut mpsc::Receiver<PtyIoDataCommand>,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        accepting: Arc<Mutex<bool>>,
    ) {
        while let Some(command) = data_rx.blocking_recv() {
            match command {
                PtyIoDataCommand::WriteUserInput(bytes) => {
                    if write_tx.send(PtyIoWriteCommand::Write(bytes)).is_err() {
                        break;
                    }
                }
                PtyIoDataCommand::WriteMailboxInput(bytes, completion) => {
                    if write_tx
                        .send(PtyIoWriteCommand::WriteMailboxInput(bytes, completion))
                        .is_err()
                    {
                        break;
                    }
                }
                PtyIoDataCommand::SubmitUserInput {
                    text,
                    enter,
                    delay,
                    deadline,
                    reply,
                } => {
                    let result = if deadline.is_some_and(|deadline| {
                        deadline.saturating_duration_since(Instant::now()) <= delay
                    }) {
                        Err(input_submission_timed_out())
                    } else {
                        let text_deadline =
                            deadline.and_then(|deadline| deadline.checked_sub(delay));
                        write_submission_part(&write_tx, text, text_deadline).and_then(|()| {
                            // A started text write is committed. Finish Enter even if the caller
                            // stops waiting so a timeout cannot leave a partial prompt.
                            std::thread::sleep(delay);
                            // Hold the lock only to order Enter against shutdown; waiting
                            // for a blocked PTY write under it would stall input and shutdown.
                            let completion = {
                                let accepting = accepting
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                if !*accepting {
                                    return Err(pty_actor_closed());
                                }
                                queue_submission_part(&write_tx, enter, None)?
                            };
                            wait_submission_part(completion)
                        })
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                }
            }
        }
    }

    fn write_submission_part(
        write_tx: &std_mpsc::Sender<PtyIoWriteCommand>,
        bytes: Bytes,
        deadline: Option<Instant>,
    ) -> std::io::Result<()> {
        wait_submission_part(queue_submission_part(write_tx, bytes, deadline)?)
    }

    fn queue_submission_part(
        write_tx: &std_mpsc::Sender<PtyIoWriteCommand>,
        bytes: Bytes,
        deadline: Option<Instant>,
    ) -> std::io::Result<std_mpsc::Receiver<std::io::Result<()>>> {
        let (reply, completion) = std_mpsc::channel();
        write_tx
            .send(PtyIoWriteCommand::SubmissionPart {
                bytes,
                deadline,
                reply,
            })
            .map_err(|_| pty_actor_closed())?;
        Ok(completion)
    }

    fn wait_submission_part(
        completion: std_mpsc::Receiver<std::io::Result<()>>,
    ) -> std::io::Result<()> {
        completion
            .recv()
            .unwrap_or_else(|_| Err(pty_actor_closed()))
    }

    fn pty_actor_closed() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
    }

    fn input_submission_timed_out() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "agent prompt timed out before input submission",
        )
    }

    fn write_and_flush(writer: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
        writer.write_all(bytes)?;
        writer.flush()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn shutdown_fails_pending_mailbox_completion_once() {
            let (data_tx, _data_rx) = mpsc::channel(1);
            let (control_tx, control_rx) = std_mpsc::channel();
            let accepting = Arc::new(Mutex::new(true));
            let pending_mailbox_completions = Arc::new(Mutex::new(Vec::new()));
            let (result_tx, result_rx) = std_mpsc::channel();
            let completion = Arc::new(Mutex::new(Some(Box::new(move |result: io::Result<()>| {
                let _ = result_tx.send(result);
            }) as MailboxCallback)));
            pending_mailbox_completions
                .lock()
                .expect("pending mailbox lock")
                .push(Arc::clone(&completion));
            let handle = PtyIoActorHandle {
                data_tx,
                control_tx,
                accepting: Arc::clone(&accepting),
                pending_mailbox_completions: Arc::clone(&pending_mailbox_completions),
            };

            handle.shutdown();
            fail_windows_writer(&accepting, &pending_mailbox_completions);

            assert!(result_rx.recv().expect("shutdown completion").is_err());
            assert!(result_rx.try_recv().is_err());
            assert!(!*accepting.lock().expect("accepting lock"));
            assert!(matches!(
                control_rx.recv().expect("shutdown command"),
                PtyIoControlCommand::Shutdown
            ));
        }

        #[test]
        fn blocked_enter_write_does_not_hold_accepting_lock() {
            let (data_tx, mut data_rx) = mpsc::channel(1);
            let (write_tx, write_rx) = std_mpsc::channel();
            let accepting = Arc::new(Mutex::new(true));
            let forwarder_accepting = Arc::clone(&accepting);
            let forwarder = std::thread::spawn(move || {
                run_input_forwarder(&mut data_rx, write_tx, forwarder_accepting);
            });
            let (reply, submitted) = std_mpsc::channel();
            data_tx
                .blocking_send(PtyIoDataCommand::SubmitUserInput {
                    text: Bytes::from_static(b"hello"),
                    enter: Bytes::from_static(b"\r"),
                    delay: Duration::from_millis(1),
                    deadline: None,
                    reply,
                })
                .expect("queue submission");

            let mut parts = Vec::new();
            for _ in 0..2 {
                let Ok(PtyIoWriteCommand::SubmissionPart { bytes, reply, .. }) = write_rx.recv()
                else {
                    panic!("expected a submission part");
                };
                parts.push((bytes, reply));
                if parts.len() == 1 {
                    let _ = parts[0].1.send(Ok(()));
                }
            }
            // Enter is queued but its PTY write has not finished: shutdown and
            // keystrokes must still be able to take the lock.
            assert_eq!(parts[1].0, Bytes::from_static(b"\r"));
            assert!(accepting.try_lock().is_ok());
            let _ = parts[1].1.send(Ok(()));

            assert!(submitted.recv().expect("submission reply").is_ok());
            drop(data_tx);
            forwarder.join().expect("forwarder thread");
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;
