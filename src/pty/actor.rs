#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows {
    use std::io::{self, Read, Write};
    use std::sync::{mpsc as std_mpsc, Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes;
    use portable_pty::{MasterPty, PtySize};
    use tokio::sync::mpsc;
    use tracing::{debug, warn};

    pub(crate) struct PtyReadResult {
        pub terminal_responses: Vec<Bytes>,
    }

    type ReadCallback = Box<dyn FnMut(&[u8]) -> PtyReadResult + Send + 'static>;
    type ReaderExitCallback = Box<dyn FnOnce() + Send + 'static>;
    type MailboxCallback = Box<dyn FnOnce(io::Result<()>) + Send>;
    type MailboxCompletion = Arc<Mutex<Option<MailboxCallback>>>;
    type PendingMailboxCompletions = Arc<Mutex<Vec<MailboxCompletion>>>;

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

    enum PtyIoDataCommand {
        WriteUserInput(Bytes),
        WriteMailboxInput(Bytes, MailboxCompletion),
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
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !*accepting {
                return Err(mpsc::error::TrySendError::Closed(bytes));
            }
            self.data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(bytes))
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput(bytes)) => {
                        mpsc::error::TrySendError::Full(bytes)
                    }
                    mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput(bytes)) => {
                        mpsc::error::TrySendError::Closed(bytes)
                    }
                    mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteMailboxInput(
                        bytes,
                        _,
                    )) => mpsc::error::TrySendError::Full(bytes),
                    mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteMailboxInput(
                        bytes,
                        _,
                    )) => mpsc::error::TrySendError::Closed(bytes),
                })
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
            let writer = master
                .take_writer()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let writer = Arc::new(Mutex::new(writer));
            let (data_tx, mut data_rx) = mpsc::channel::<PtyIoDataCommand>(1024);
            let (control_tx, control_rx) = std_mpsc::channel::<PtyIoControlCommand>();
            let accepting = Arc::new(Mutex::new(!initially_quiesced));
            let pending_mailbox_completions = Arc::new(Mutex::new(Vec::new()));

            {
                let writer = Arc::clone(&writer);
                let accepting = Arc::clone(&accepting);
                let pending_mailbox_completions = Arc::clone(&pending_mailbox_completions);
                std::thread::spawn(move || {
                    while let Some(command) = data_rx.blocking_recv() {
                        match command {
                            PtyIoDataCommand::WriteUserInput(bytes) => {
                                if write_all_locked(&writer, &bytes).is_err() {
                                    fail_windows_writer(&accepting, &pending_mailbox_completions);
                                    break;
                                }
                            }
                            PtyIoDataCommand::WriteMailboxInput(bytes, completion) => {
                                let result = write_all_locked(&writer, &bytes);
                                let failed = result.is_err();
                                finish_mailbox_write(
                                    &pending_mailbox_completions,
                                    &completion,
                                    result,
                                );
                                if failed {
                                    fail_windows_writer(&accepting, &pending_mailbox_completions);
                                    break;
                                }
                            }
                        }
                    }
                    debug!(pane_id, "windows pty writer thread exiting");
                });
            }

            {
                let writer = Arc::clone(&writer);
                let accepting = Arc::clone(&accepting);
                let pending_mailbox_completions = Arc::clone(&pending_mailbox_completions);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let result = on_read(&buf[..n]);
                                for response in result.terminal_responses {
                                    if write_all_locked(&writer, &response).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(err) => {
                                debug!(pane_id, err = %err, "windows pty reader failed");
                                break;
                            }
                        }
                    }
                    fail_windows_writer(&accepting, &pending_mailbox_completions);
                    if let Some(on_reader_exit) = on_reader_exit {
                        on_reader_exit();
                    }
                    debug!(pane_id, "windows pty reader thread exiting");
                });
            }

            {
                let writer = Arc::clone(&writer);
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
                                for response in request.terminal_responses {
                                    if write_all_locked(&writer, &response).is_err() {
                                        break;
                                    }
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

    fn write_all_locked(
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let mut writer = writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(bytes)?;
        writer.flush()
    }

    fn remove_pending_mailbox_completion(
        pending: &PendingMailboxCompletions,
        target: &MailboxCompletion,
    ) {
        pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|completion| !Arc::ptr_eq(completion, target));
    }

    fn finish_mailbox_write(
        pending: &PendingMailboxCompletions,
        target: &MailboxCompletion,
        result: io::Result<()>,
    ) {
        let callback = {
            let mut pending = pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.retain(|completion| !Arc::ptr_eq(completion, target));
            target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        };
        if let Some(callback) = callback {
            callback(result);
        }
    }

    fn fail_windows_writer(accepting: &Arc<Mutex<bool>>, pending: &PendingMailboxCompletions) {
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

    #[allow(dead_code)]
    fn _assert_duration_send(_: Duration) {}

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
            let completion = Arc::new(Mutex::new(Some(Box::new(move |result| {
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
            finish_mailbox_write(&pending_mailbox_completions, &completion, Ok(()));

            assert!(result_rx.recv().expect("shutdown completion").is_err());
            assert!(result_rx.try_recv().is_err());
            assert!(!*accepting.lock().expect("accepting lock"));
            assert!(matches!(
                control_rx.recv().expect("shutdown command"),
                PtyIoControlCommand::Shutdown
            ));
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;
