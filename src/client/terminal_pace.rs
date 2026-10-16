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

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

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
}

impl TerminalPace {
    /// Records a probe the caller just wrote to the terminal.
    pub(crate) fn sent(&self) {
        self.0.enabled.store(true, Ordering::Release);
        self.0.outstanding.fetch_add(1, Ordering::AcqRel);
    }

    /// True while a written probe has not been answered.
    pub(crate) fn waiting(&self) -> bool {
        self.0.outstanding.load(Ordering::Acquire) > 0
    }

    /// Stops waiting for unanswered probes, for a terminal that never replies.
    pub(crate) fn give_up(&self) {
        self.0.outstanding.store(0, Ordering::Release);
    }

    /// Resolves after a reply arrives. A reply that came first is not lost.
    pub(crate) async fn answered(&self) {
        self.0.answered.notified().await;
    }

    /// Removes probe replies from raw stdin bytes. Returns `None` when the
    /// bytes contain no reply and pass through untouched.
    pub(crate) fn strip_replies(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        if !self.0.enabled.load(Ordering::Acquire) {
            return None;
        }
        let (rest, replies) = strip(bytes);
        if replies == 0 {
            return None;
        }
        let _ = self
            .0
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_sub(replies))
            });
        self.0.answered.notify_one();
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
    fn replies_are_stripped_only_after_a_probe_was_sent() {
        let pace = TerminalPace::default();
        assert_eq!(pace.strip_replies(b"a\x1b[0nb"), None);

        pace.sent();
        pace.sent();
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
        pace.sent();
        pace.give_up();
        assert!(!pace.waiting());
        assert_eq!(pace.strip_replies(b"\x1b[0nx").as_deref(), Some(&b"x"[..]));
        assert!(!pace.waiting());
    }

    #[test]
    fn other_status_reports_pass_through() {
        let pace = TerminalPace::default();
        pace.sent();
        assert_eq!(pace.strip_replies(b"\x1b[3n\x1b[12;4R"), None);
    }

    #[tokio::test]
    async fn a_reply_before_the_wait_still_wakes_it() {
        let pace = TerminalPace::default();
        pace.sent();
        let _ = pace.strip_replies(b"\x1b[0n");
        tokio::time::timeout(std::time::Duration::from_secs(1), pace.answered())
            .await
            .expect("stored wakeup");
    }
}
