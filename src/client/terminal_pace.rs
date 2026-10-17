//! Output pacing against the host terminal.
//!
//! A terminal decodes kitty images on the thread that reads its input, so a
//! stream of image frames (a browser pane scrolling) can be written far faster
//! than the terminal shows them. The excess waits in the pty buffer, and a
//! workspace switch or a keystroke echo queued behind it appears only after
//! every older image is decoded. After a frame that carries images the client
//! writes a device status request (`CSI 5 n`); the terminal answers `CSI 0 n`
//! only once it has processed everything before it. Until that answer arrives
//! the client keeps drawing deferred, so at most one image frame is in flight
//! and the next draw shows the latest state instead of every stale frame.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Device status request written after a paced frame.
pub(crate) const PROBE: &[u8] = b"\x1b[5n";
/// The terminal's "ready" answer to [`PROBE`].
const REPLY: &[u8] = b"\x1b[0n";

#[derive(Clone, Default)]
pub(crate) struct TerminalPace(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// Set once the first probe is written. A keyboard never produces the
    /// reply sequence, so from then on every reply is stripped from input,
    /// including late ones that arrive after a timeout gave up on them.
    enabled: AtomicBool,
    outstanding: AtomicU32,
    answered: tokio::sync::Notify,
    trace: Mutex<TraceState>,
}

#[derive(Clone, Copy)]
pub(crate) struct ProbeTrace {
    draw_seq: u64,
    time_us: u128,
}

#[derive(Default)]
struct TraceState {
    pending: VecDeque<ProbeTrace>,
    // DSR has no request ID; after a timeout, late replies cannot be assigned safely.
    ambiguous: bool,
}

impl TerminalPace {
    pub(crate) fn sent(&self, draw_seq: u64) -> Option<ProbeTrace> {
        let mut trace = crate::logging::latency_trace_enabled()
            .then(|| self.0.trace.lock().unwrap_or_else(|e| e.into_inner()));
        self.0.enabled.store(true, Ordering::Release);
        self.0.outstanding.fetch_add(1, Ordering::AcqRel);
        trace.as_mut().map(|trace| {
            let probe = ProbeTrace {
                draw_seq,
                time_us: crate::logging::latency_trace_time_us(),
            };
            trace.pending.push_back(probe);
            probe
        })
    }

    /// The caller records only after flushing and committing its presentation state.
    pub(crate) fn record_sent(&self, probe: Option<ProbeTrace>) {
        if let Some(probe) = probe {
            tracing::info!(target: "herdr_latency", event = "terminal.probe_sent",
                time_us = probe.time_us, pid = std::process::id(), draw_seq = probe.draw_seq,
                pace_id = Arc::as_ptr(&self.0) as usize, "terminal processing probe written");
        }
    }

    pub(crate) fn waiting(&self) -> bool {
        self.0.outstanding.load(Ordering::Acquire) > 0
    }

    pub(crate) fn give_up(&self) -> Option<VecDeque<ProbeTrace>> {
        let mut trace = crate::logging::latency_trace_enabled()
            .then(|| self.0.trace.lock().unwrap_or_else(|e| e.into_inner()));
        self.0.outstanding.store(0, Ordering::Release);
        let pending = trace.as_mut().map(|trace| {
            trace.ambiguous = true;
            std::mem::take(&mut trace.pending)
        });
        drop(trace);
        pending
    }

    pub(crate) fn record_give_up(
        &self,
        pending: Option<VecDeque<ProbeTrace>>,
        reason: &'static str,
    ) {
        if let Some(pending) = pending {
            for probe in pending {
                tracing::info!(target: "herdr_latency", event = "terminal.probe_give_up",
                    time_us = crate::logging::latency_trace_time_us(), pid = std::process::id(),
                    pace_id = Arc::as_ptr(&self.0) as usize, draw_seq = probe.draw_seq, reason,
                    "terminal processing wait abandoned");
            }
        }
    }

    pub(crate) async fn answered(&self) {
        self.0.answered.notified().await;
    }

    pub(crate) fn strip_replies(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        if !self.0.enabled.load(Ordering::Acquire) {
            return None;
        }
        let (rest, replies) = strip(bytes);
        if replies == 0 {
            return None;
        }
        let mut trace = crate::logging::latency_trace_enabled()
            .then(|| self.0.trace.lock().unwrap_or_else(|e| e.into_inner()));
        let _ = self
            .0
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_sub(replies))
            });
        self.0.answered.notify_one();
        let records = trace.as_mut().map(|trace| {
            (0..replies)
                .map(|_| {
                    let probe = trace.pending.pop_front();
                    if trace.ambiguous {
                        None
                    } else {
                        probe.map(|probe| probe.draw_seq)
                    }
                })
                .collect::<Vec<_>>()
        });
        drop(trace);
        if let Some(records) = records {
            for draw_seq in records {
                tracing::info!(target: "herdr_latency", event = "terminal.probe_reply",
                    time_us = crate::logging::latency_trace_time_us(), pid = std::process::id(),
                    pace_id = Arc::as_ptr(&self.0) as usize, draw_seq,
                    matched = draw_seq.is_some(), "terminal processing reply received");
            }
        }
        Some(rest)
    }
}

fn strip(bytes: &[u8]) -> (Vec<u8>, u32) {
    let mut rest = Vec::with_capacity(bytes.len());
    let mut replies = 0u32;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(REPLY) {
            replies = replies.saturating_add(1);
            index += REPLY.len();
        } else {
            rest.push(bytes[index]);
            index += 1;
        }
    }
    (rest, replies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_trace_writer_cannot_delay_pace_state_or_notification() {
        const CHILD: &str = "HERDR_PACE_TRACE_ORDER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "client::terminal_pace::tests::slow_trace_writer_cannot_delay_pace_state_or_notification", "--nocapture"])
                .env(CHILD, "1").env("HERDR_LATENCY_TRACE", "1").status().unwrap();
            assert!(result.success());
            return;
        }
        for reply_log in [false, true] {
            let pace = TerminalPace::default();
            let probe = pace.sent(41);
            let (entered, notified) = std::sync::mpsc::channel();
            let (release, resume) = std::sync::mpsc::channel();
            let resume = Mutex::new(resume);
            let path = std::env::temp_dir().join(format!(
                "herdr-slow-pace-{}-{reply_log}.log",
                std::process::id()
            ));
            let file = std::fs::File::create(&path).unwrap();
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(move || {
                    entered.send(()).unwrap();
                    resume.lock().unwrap().recv().unwrap();
                    file.try_clone().unwrap()
                })
                .finish();
            let worker_pace = pace.clone();
            let worker = std::thread::spawn(move || {
                tracing::subscriber::with_default(subscriber, || {
                    if reply_log {
                        worker_pace.strip_replies(REPLY);
                    } else {
                        worker_pace.record_sent(probe);
                    }
                })
            });
            notified
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
            if !reply_log {
                assert_eq!(pace.strip_replies(REPLY), Some(Vec::new()));
            }
            assert!(
                !pace.waiting(),
                "state must commit before blocked log writer"
            );
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    tokio::time::timeout(std::time::Duration::from_secs(1), pace.answered())
                        .await
                        .unwrap();
                });
            release.send(()).unwrap();
            worker.join().unwrap();
            let log = std::fs::read_to_string(&path).unwrap();
            assert!(log.contains("draw_seq=41"));
            assert!(log.contains(if reply_log {
                "terminal.probe_reply"
            } else {
                "terminal.probe_sent"
            }));
            std::fs::remove_file(path).unwrap();
            assert!(!pace.waiting());
        }
    }

    #[test]
    fn replies_are_stripped_only_after_a_probe_was_sent() {
        let pace = TerminalPace::default();
        assert_eq!(pace.strip_replies(b"a\x1b[0nb"), None);

        let _ = pace.sent(1);
        let _ = pace.sent(1);
        assert!(pace.waiting());
        assert_eq!(pace.strip_replies(b"plain"), None);
        assert_eq!(
            pace.strip_replies(b"a\x1b[0nb").as_deref(),
            Some(&b"ab"[..])
        );
        assert!(pace.waiting());
        assert_eq!(pace.strip_replies(b"\x1b[0n").as_deref(), Some(&b""[..]));
        assert!(!pace.waiting());
    }

    #[test]
    fn a_late_reply_after_giving_up_is_still_kept_out_of_input() {
        let pace = TerminalPace::default();
        let _ = pace.sent(1);
        let _ = pace.give_up();
        assert!(!pace.waiting());
        assert_eq!(pace.strip_replies(b"\x1b[0nx").as_deref(), Some(&b"x"[..]));
        assert!(!pace.waiting());
    }

    #[test]
    fn other_status_reports_pass_through() {
        let pace = TerminalPace::default();
        let _ = pace.sent(1);
        assert_eq!(pace.strip_replies(b"\x1b[3n\x1b[12;4R"), None);
    }

    #[tokio::test]
    async fn a_reply_before_the_wait_still_wakes_it() {
        let pace = TerminalPace::default();
        let _ = pace.sent(1);
        let _ = pace.strip_replies(b"\x1b[0n");
        tokio::time::timeout(std::time::Duration::from_secs(1), pace.answered())
            .await
            .expect("stored wakeup");
    }
}
