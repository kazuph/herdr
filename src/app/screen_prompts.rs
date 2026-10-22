//! Screen-prompt decisions: turning agent selection/permission prompts into
//! pending decisions and delivering answers back to the pane.
//!
//! While an agent pane is `blocked`, the detection screen is parsed with
//! `crate::screen_prompt`. A parsed prompt becomes a pending `agent_prompt` or
//! `tool_permission` decision; answering it sends the option's number key (or
//! the advertised single-key shortcut) to the pane, and free-text answers
//! select the "Type something" slot, type the text, then press Enter. A watch
//! keeps tracking the prompt until it disappears or changes; two consecutive
//! mismatched reads end the watch, mirroring saberante's `still_blocked`
//! tolerance for one torn frame.

use std::time::{Duration, Instant};

use bytes::Bytes;
use tracing::{debug, warn};

use super::App;
use crate::api::schema::{
    Decision, DecisionKind, DecisionOption, DecisionOptionRole, DecisionOrigin, DecisionStatus,
};
use crate::decision::{new_decision_id, unix_ms_now, DecisionStore, ResolveOutcome};
use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::screen_prompt::{
    normalize_volatile, parse_screen_prompt, Focused, OptionShortcut, ScreenPrompt,
};

/// Decision origin reason marking decisions created from pane screens.
pub(crate) const SCREEN_PROMPT_REASON: &str = "screen_prompt";
/// How often blocked panes are re-scanned while the feature is active.
pub(super) const SCREEN_PROMPT_SCAN_INTERVAL: Duration = Duration::from_millis(400);
/// Consecutive reads that no longer show the watched prompt before the watch
/// is released and the pending decision cancelled.
const SCREEN_PROMPT_MISMATCH_LIMIT: u8 = 2;
/// Extra scrollback rows above the viewport included in every screen-prompt
/// read — a permission dialog taller than the pane keeps its header, the
/// full command box and the `⏺` call line reachable for the title.
pub(super) const SCREEN_PROMPT_EXTRA_ROWS: usize = 24;
/// Cancel+recreate rebuilds inside this window mark a pane as churning.
const SCREEN_PROMPT_FLOOD_WINDOW: Duration = Duration::from_secs(30);
/// Rebuild count inside the window that triggers the flood brake — beyond
/// this cadence the pending decision updates in place instead of emitting
/// another resolved+created pair per redraw.
const SCREEN_PROMPT_FLOOD_LIMIT: usize = 3;
/// Delay between typing the free-text option number and the answer text, and
/// again before Enter (CC redraws the option row when a number is typed).
const FREE_TEXT_OPTION_DELAY: Duration = Duration::from_millis(500);
const FREE_TEXT_ENTER_DELAY: Duration = Duration::from_millis(300);
/// Spacing between multiSelect toggle keys. Bundled into one write, option
/// digits read as a single keypress and nothing toggles (verified against
/// Claude Code 2.1.x AskUserQuestion).
const TOGGLE_KEY_DELAY: Duration = Duration::from_millis(150);
/// The submit pass re-reads the prompt after toggle keys; redraws can leave
/// a torn frame, so retry a few times before abandoning the answer.
const MULTISELECT_SUBMIT_RETRIES: u8 = 4;
/// Retry budget for a permission answer with a note: arrow keys and torn
/// reads spend it.
const AMEND_ATTEMPTS: u8 = 16;
const MULTISELECT_SUBMIT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// When a question draws option previews beside the list, Claude Code's
/// option digits only move the cursor; Enter confirms. The confirm pass
/// re-reads the prompt and retries torn frames a few times.
const PREVIEW_CONFIRM_RETRIES: u8 = 4;
/// How recent pane-bound input from outside the screen-prompt machinery —
/// operator keystrokes, pastes, mouse reports, `pane send-*`/`agent send`
/// API calls, attach/endpoint client input — must be before herdr stops
/// sending keys into that pane. Preview scans refuse to start, in-flight
/// scans abort without restore arrows, and decision-answer delivery backs
/// off until this quiet period passes.
const PANE_USER_INPUT_QUIET: Duration = Duration::from_secs(3);
/// Between two arrow keys of a preview scan: long enough for the agent TUI
/// to redraw the newly focused option's preview box, short enough to finish
/// a walk through a handful of options before the user notices.
const PREVIEW_SCAN_STEP_DELAY: Duration = Duration::from_millis(200);
/// Consecutive torn/mismatched reads tolerated inside a preview scan before
/// the walk is abandoned — the prompt genuinely changed, not a redraw.
const PREVIEW_SCAN_MISMATCH_LIMIT: u8 = 3;

/// Fields a parsed screen prompt contributes to a decision record. Built
/// once and shared by create and the in-place refresh paths.
pub(crate) struct PromptDecisionParts {
    pub public_id: Option<String>,
    pub kind: DecisionKind,
    pub options: Vec<DecisionOption>,
    pub allow_text: bool,
    pub title: String,
    pub body: Option<String>,
    pub question_label: Option<String>,
    pub origin: DecisionOrigin,
}

impl PromptDecisionParts {
    /// Refresh an existing pending decision's content fields, keeping its
    /// id, created timestamp, status and answer.
    fn apply_to(&self, decision: &mut Decision) {
        decision.title = self.title.clone();
        decision.body = self.body.clone();
        decision.question_label = self.question_label.clone();
        decision.options = self.options.clone();
        decision.origin = Some(self.origin.clone());
    }
}

/// `cut` is `full` with its beginning missing: a non-empty strict suffix
/// once whitespace (rewrapping) and volatile displays are ignored.
fn title_is_cut_top_of(cut: &str, full: &str) -> bool {
    let compact = |text: &str| -> String {
        normalize_volatile(text)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect()
    };
    let (cut, full) = (compact(cut), compact(full));
    !cut.is_empty() && cut.len() < full.len() && full.ends_with(&cut)
}

/// An in-place refresh never shortens the question to a cut-off view of
/// itself; the decision keeps the full text it was created with.
fn keep_full_title(existing: &Decision, updated: &mut Decision) {
    if title_is_cut_top_of(&updated.title, &existing.title) {
        updated.title = existing.title.clone();
    }
}

/// A screen prompt a pane is currently showing, tracked against the decision
/// it created so redraws, tab moves and disappearances reconcile it.
#[derive(Debug, Clone)]
pub(crate) struct ScreenPromptWatch {
    pub signature: String,
    pub decision_id: String,
    /// Option number of the free-text slot when the prompt offered one; kept
    /// so a text answer still finds the slot when the live read is torn.
    pub free_text_slot: Option<u32>,
    pub codex_question: bool,
    /// Normalized Codex notice last published without changing identity.
    pub notice: Option<String>,
    /// The decision was resolved (answered, cancelled, expired); the watch
    /// stays only to observe the prompt leaving the screen so the same prompt
    /// is not re-offered.
    pub resolved: bool,
    /// Consecutive scans whose parse did not match `signature`.
    pub mismatches: u8,
}

/// A one-shot walk through a preview-bearing prompt's options. Claude Code
/// only paints the focused option's preview box, so before the decision is
/// created the cursor is moved (Up/Down arrow keys only — never a digit,
/// Enter or Space, which would answer) onto each option to capture its box,
/// then put back onto the option the user had focused.
#[derive(Debug, Clone)]
pub(crate) struct PreviewScan {
    /// The prompt snapshot the decision is built from when the scan ends.
    pub prompt: ScreenPrompt,
    /// Signature the scan expects; a mismatch aborts the walk.
    pub signature: String,
    /// Previews captured so far, keyed by option number.
    pub previews: std::collections::BTreeMap<u32, String>,
    /// Options the cursor already sat on (box seen or not).
    pub visited: std::collections::BTreeSet<u32>,
    /// Option numbers still to visit.
    pub remaining: std::collections::VecDeque<u32>,
    /// The cursor position to restore after the walk.
    pub restore: Option<crate::screen_prompt::Focused>,
    /// True while sending the cursor back to `restore`.
    pub restoring: bool,
    /// Step budget; exhaustion finishes the scan with whatever was read.
    pub steps_left: u32,
    /// Consecutive steps whose parse did not match `signature` — a redraw can
    /// leave one torn frame, so a single mismatch does not kill the walk.
    pub mismatches: u8,
    /// True once the first arrow has been sent. Until then `restore` keeps
    /// refreshing from each clean read, so a torn first frame cannot pin the
    /// resting cursor to the wrong option.
    pub arrows_sent: bool,
    /// Question-text-and-options identity the scan registers as done: unlike
    /// the volatile signature, a redraw or a typed character must not turn
    /// the same question into a new scan.
    pub scan_key: String,
}

/// Multi-question (AskUserQuestion) tabs herdr answered through decision
/// delivery. When the pane reaches the final `Submit answers` confirmation
/// with every question answered through herdr and no external input since,
/// the confirmation is submitted automatically instead of surfaced as a
/// decision.
#[derive(Debug)]
pub(crate) struct ScreenPromptFlow {
    /// Identity of the tab strip the answers belong to (`mq.tabs` joined).
    pub tabs_key: String,
    /// When this flow's first answer was recorded. External input timestamped
    /// after it means a person took over the pane and auto-submit must not
    /// fire.
    pub began_at: Instant,
    /// `mq.index` values answered through decision delivery.
    pub answered: std::collections::BTreeSet<usize>,
}

/// Work to perform against a pane once a deadline passes. Typed text and
/// Enter for free-text answers need delays so the agent TUI can redraw
/// between keys; multiSelect answers need a second pass that re-reads the
/// cursor position before aiming arrow keys at the advance row.
#[derive(Debug, Clone)]
pub(crate) enum DeferredPaneWrite {
    Bytes {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
        bytes: Bytes,
    },
    /// Finish a multiSelect answer: re-parse the prompt, move the cursor to
    /// the free-text slot / `Next`/`Submit` row, type text, press Enter.
    MultiSelectSubmit {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
        decision_id: String,
        text: Option<String>,
        /// Retry budget for torn reads while the prompt redraws between the
        /// toggle keys and the submit pass.
        attempts_left: u8,
    },
    /// Confirm a single-select answer on a question that draws option
    /// previews: the digit moved the cursor, Enter selects it once the
    /// cursor sits on the answered option of the same prompt.
    McpConfirm {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
        signature: String,
        number: u32,
    },
    PreviewConfirm {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
        signature: String,
        number: u32,
        attempts_left: u8,
    },
    /// One step of a preview scan: re-parse the pane, harvest the focused
    /// option's preview box, then send one arrow toward the next option.
    PreviewScanStep {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
    },
    /// One step of a permission answer that carries a note (Claude Code
    /// `Tab to amend`): walk the cursor to the option, open its note with
    /// Tab, type the note, then Enter. Every step re-reads the pane first.
    AmendStep {
        at: Instant,
        ws_idx: usize,
        pane_id: PaneId,
        question: String,
        number: u32,
        note: String,
        phase: AmendPhase,
        attempts_left: u8,
        codex: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AmendPhase {
    /// Move the cursor onto the option, then press Tab.
    Aim,
    /// The note field is open on the option: type the note.
    Type,
    /// The note is typed: press Enter.
    Submit,
}

impl DeferredPaneWrite {
    fn at(&self) -> Instant {
        match self {
            Self::Bytes { at, .. }
            | Self::MultiSelectSubmit { at, .. }
            | Self::McpConfirm { at, .. }
            | Self::PreviewConfirm { at, .. }
            | Self::PreviewScanStep { at, .. }
            | Self::AmendStep { at, .. } => *at,
        }
    }
}

impl App {
    pub(super) fn screen_prompt_input_in_flight(&self, pane: PaneId) -> bool {
        self.preview_scans.contains_key(&pane)
            || self.deferred_pane_writes.iter().any(|write| {
                let (DeferredPaneWrite::Bytes { pane_id, .. }
                | DeferredPaneWrite::MultiSelectSubmit { pane_id, .. }
                | DeferredPaneWrite::McpConfirm { pane_id, .. }
                | DeferredPaneWrite::PreviewConfirm { pane_id, .. }
                | DeferredPaneWrite::PreviewScanStep { pane_id, .. }
                | DeferredPaneWrite::AmendStep { pane_id, .. }) = write;
                *pane_id == pane
            })
    }

    /// Arm the next scan tick. Called from pane state events and the answer
    /// path; scanning itself runs on the loop deadline so a parse never
    /// stalls event handling.
    pub(super) fn arm_screen_prompt_scan(&mut self) {
        if self.state.decisions_screen_prompts {
            self.screen_prompt_scan_at = Some(Instant::now());
        }
    }

    /// The next instant the loop must wake for screen-prompt work: a pending
    /// scan or a deferred pane write, whichever is earlier. An armed scan
    /// only wakes the loop while a watched pane or a blocked agent exists —
    /// an idle app must not pay for periodic screen reads.
    pub(crate) fn screen_prompt_deadline(&self) -> Option<Instant> {
        let scan_deadline = self.screen_prompt_scan_at.filter(|_| {
            !self.screen_prompt_watches.is_empty()
                || self
                    .state
                    .terminals
                    .values()
                    .any(|terminal| terminal.state == AgentState::Blocked)
        });
        [
            scan_deadline,
            self.input_guard_scan_at,
            self.deferred_pane_writes
                .iter()
                .map(DeferredPaneWrite::at)
                .min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Deadline-driven work for the TUI and headless loops: flush due writes
    /// and run a scan when its scheduled instant passed.
    pub(crate) fn screen_prompt_tasks(&mut self, now: Instant) -> bool {
        self.input_guard_tasks(now);
        let mut changed = self.flush_deferred_pane_writes(now);
        if self
            .screen_prompt_scan_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.screen_prompt_scan_at = None;
            changed |= self.scan_screen_prompts(now);
        }
        changed
    }

    /// Called when a decision answer resolves a screen-prompt decision: mark
    /// the watch resolved and deliver the answer's keys/text to the pane.
    pub(super) fn screen_prompt_answer(&mut self, decision: &Decision) {
        let Some((ws_idx, pane_id)) = self.screen_prompt_pane_ref(decision) else {
            return;
        };
        if let Some(watch) = self.screen_prompt_watches.get_mut(&pane_id) {
            if watch.decision_id == decision.decision_id {
                watch.resolved = true;
            }
        }
        let answer = decision.answer.clone();
        self.deliver_screen_prompt_answer(ws_idx, pane_id, decision, answer.as_ref());
        // The answer changes the screen; rescan promptly so the next prompt in
        // a multi-question flow becomes a decision without extra delay.
        self.arm_screen_prompt_scan();
    }

    /// Called when a screen-prompt decision is cancelled or expires through an
    /// API surface. The watch stays resolved-but-tracked so a prompt that is
    /// still on screen is not re-offered as a fresh decision.
    pub(super) fn screen_prompt_resolved(&mut self, decision: &Decision) {
        let Some((_, pane_id)) = self.screen_prompt_pane_ref(decision) else {
            return;
        };
        if let Some(watch) = self.screen_prompt_watches.get_mut(&pane_id) {
            if watch.decision_id == decision.decision_id {
                watch.resolved = true;
            }
        }
    }

    /// True when the decision originated from a pane screen prompt.
    pub(super) fn is_screen_prompt_decision(decision: &Decision) -> bool {
        decision
            .origin
            .as_ref()
            .is_some_and(|origin| origin.reason.as_deref() == Some(SCREEN_PROMPT_REASON))
    }

    /// Freshness gate for `decision.answer` on screen-prompt decisions,
    /// checked before the answer commits. True only when the pane's live
    /// screen still parses to the watched prompt's signature, or when the
    /// read is torn (no prompt parses) while the last scan still matched
    /// (`mismatches == 0`). A different prompt, a gone pane, or a missing
    /// watch means the keys would answer a different question or type into
    /// the agent's input line.
    pub(super) fn screen_prompt_answer_is_fresh(&self, decision: &Decision) -> bool {
        let Some((ws_idx, pane_id)) = self.screen_prompt_pane_ref(decision) else {
            return false;
        };
        let Some(watch) = self.screen_prompt_watches.get(&pane_id) else {
            return false;
        };
        if watch.decision_id != decision.decision_id {
            return false;
        }
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return false;
        };
        match parse_screen_prompt(&runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS))
        {
            Some(prompt) => prompt.signature() == watch.signature,
            None => watch.mismatches == 0,
        }
    }

    /// Operator-activity gate for `decision.answer`: a pending screen-prompt
    /// decision must not commit while a person's input recently reached the
    /// pane. Answering anyway would send keys under an active typist.
    pub(super) fn screen_prompt_pane_has_recent_user_input(&self, decision: &Decision) -> bool {
        let Some((ws_idx, pane_id)) = self.screen_prompt_pane_ref(decision) else {
            return false;
        };
        self.pane_user_input_recent(ws_idx, pane_id)
    }

    /// True when input from outside the screen-prompt machinery reached this
    /// pane inside `PANE_USER_INPUT_QUIET` — operator keystrokes, pastes,
    /// mouse reports, `pane send-*`/`agent send` calls, attach or endpoint
    /// client input. While true herdr sends no keys into the pane.
    fn pane_user_input_recent(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.lookup_runtime_sender(ws_idx, pane_id)
            .and_then(|runtime| runtime.user_input_at())
            .is_some_and(|at| at.elapsed() < PANE_USER_INPUT_QUIET)
    }

    /// Register the pane's in-flight (or just-finished) scan as done so the
    /// same question is never walked again.
    fn mark_preview_scan_done(&mut self, pane_id: PaneId) {
        let Some(scan) = self.preview_scans.get(&pane_id) else {
            return;
        };
        let key = scan.scan_key.clone();
        self.preview_scan_done
            .entry(pane_id)
            .or_default()
            .insert(key);
    }

    /// A decision answer landed on a multi-question prompt: record which tab
    /// herdr answered so the final `Submit answers` confirmation may be
    /// submitted automatically only when every question went through herdr.
    fn record_screen_prompt_answer(&mut self, pane_id: PaneId, prompt: &ScreenPrompt) {
        let Some(mq) = &prompt.mq else {
            return;
        };
        if mq.at_submit {
            return;
        }
        let tabs_key = mq.tabs.join("\u{1f}");
        match self.screen_prompt_flows.get_mut(&pane_id) {
            // Question 1 re-answered means the flow restarted — a stale
            // record from an earlier run must not count the new flow's
            // later questions as delivered.
            Some(flow) if flow.tabs_key == tabs_key && mq.index > 0 => {
                flow.answered.insert(mq.index);
            }
            _ => {
                let mut answered = std::collections::BTreeSet::new();
                answered.insert(mq.index);
                self.screen_prompt_flows.insert(
                    pane_id,
                    ScreenPromptFlow {
                        tabs_key,
                        began_at: Instant::now(),
                        answered,
                    },
                );
            }
        }
    }

    /// A `Submit answers` / `Cancel`-style confirmation never becomes a
    /// decision and never receives cursor keys. When every question of the
    /// multi-question flow was answered through decision delivery and no
    /// external input has touched the pane since, `Submit answers` is picked
    /// once: by Enter when it is already focused, otherwise by its number
    /// key — never by arrows, so the cursor can never land on `Cancel`.
    /// When a person answered the questions themselves the pane saw their
    /// input and nothing is sent.
    fn handle_submit_confirmation(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        sig: &str,
    ) -> bool {
        let Some(mq) = &prompt.mq else {
            return false;
        };
        if self
            .screen_prompt_auto_submits
            .get(&pane_id)
            .is_some_and(|done| done == sig)
        {
            return false;
        }
        let Some(submit_number) = prompt
            .choices
            .iter()
            .find(|(_, label)| label.trim_start().to_lowercase().starts_with("submit"))
            .map(|(number, _)| *number)
        else {
            return false;
        };
        let Some(flow) = self.screen_prompt_flows.get(&pane_id) else {
            return false;
        };
        if flow.tabs_key != mq.tabs.join("\u{1f}") || flow.answered.len() < mq.total {
            return false;
        }
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return false;
        };
        if runtime
            .user_input_at()
            .is_some_and(|at| at >= flow.began_at || at.elapsed() < PANE_USER_INPUT_QUIET)
        {
            return false;
        }
        let already_focused = matches!(
            prompt.focused,
            Some(crate::screen_prompt::Focused::Option(number)) if number == submit_number
        );
        let bytes = if already_focused {
            Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )))
        } else {
            Bytes::copy_from_slice(submit_number.to_string().as_bytes())
        };
        if let Err(err) = runtime.try_send_bytes(bytes) {
            warn!(
                pane_id = ?pane_id,
                error = %err,
                "failed to auto-submit answers confirmation"
            );
            return false;
        }
        debug!(
            pane_id = ?pane_id,
            "auto-submitted answers confirmation"
        );
        self.screen_prompt_auto_submits
            .insert(pane_id, sig.to_string());
        false
    }

    /// Flood brake: the pane rebuilt its pending decision at least
    /// SCREEN_PROMPT_FLOOD_LIMIT times inside the churn window — the latest
    /// rebuild updates in place instead of emitting another cancel pair.
    fn pane_is_flooding(&mut self, pane_id: PaneId) -> bool {
        let rebuilds = self.screen_prompt_rebuilds.entry(pane_id).or_default();
        while rebuilds
            .front()
            .is_some_and(|at| at.elapsed() > SCREEN_PROMPT_FLOOD_WINDOW)
        {
            rebuilds.pop_front();
        }
        rebuilds.len() >= SCREEN_PROMPT_FLOOD_LIMIT
    }

    /// Record a cancel+recreate so repeated rebuilds trip the flood brake.
    fn record_pane_rebuild(&mut self, pane_id: PaneId) {
        self.screen_prompt_rebuilds
            .entry(pane_id)
            .or_default()
            .push_back(Instant::now());
    }

    /// The internal pane reference for a screen-prompt decision's origin.
    fn screen_prompt_pane_ref(&self, decision: &Decision) -> Option<(usize, PaneId)> {
        if !Self::is_screen_prompt_decision(decision) {
            return None;
        }
        let pane_id = decision.origin.as_ref()?.pane_id.as_deref()?;
        self.parse_pane_id(pane_id)
    }

    /// Flush deferred pane writes whose deadlines passed. Returns true when
    /// writes were delivered (the screen changed).
    fn flush_deferred_pane_writes(&mut self, now: Instant) -> bool {
        if self.deferred_pane_writes.is_empty() {
            return false;
        }
        let (due, pending): (Vec<DeferredPaneWrite>, Vec<DeferredPaneWrite>) =
            std::mem::take(&mut self.deferred_pane_writes)
                .into_iter()
                .partition(|write| write.at() <= now);
        self.deferred_pane_writes = pending;
        for write in due {
            match write {
                DeferredPaneWrite::Bytes {
                    ws_idx,
                    pane_id,
                    bytes,
                    ..
                } => {
                    let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                        warn!(
                            pane_id = ?pane_id,
                            "dropping deferred screen prompt write: pane gone"
                        );
                        continue;
                    };
                    if runtime
                        .user_input_at()
                        .is_some_and(|at| at.elapsed() < PANE_USER_INPUT_QUIET)
                    {
                        warn!(
                            pane_id = ?pane_id,
                            "dropping deferred screen prompt write: operator input is recent"
                        );
                        continue;
                    }
                    if let Err(err) = runtime.try_send_bytes(bytes) {
                        warn!(
                            pane_id = ?pane_id,
                            error = %err,
                            "deferred screen prompt write failed"
                        );
                    }
                }
                DeferredPaneWrite::MultiSelectSubmit {
                    ws_idx,
                    pane_id,
                    decision_id,
                    text,
                    attempts_left,
                    ..
                } => {
                    if self.pane_user_input_recent(ws_idx, pane_id) {
                        warn!(
                            pane_id = ?pane_id,
                            "multi-select submit dropped: operator input is recent"
                        );
                        continue;
                    }
                    self.finish_multi_select_submit(
                        ws_idx,
                        pane_id,
                        &decision_id,
                        text.as_deref(),
                        attempts_left,
                    );
                }
                DeferredPaneWrite::McpConfirm {
                    ws_idx,
                    pane_id,
                    signature,
                    number,
                    ..
                } => {
                    if !self.pane_user_input_recent(ws_idx, pane_id) {
                        self.finish_mcp_confirm(ws_idx, pane_id, &signature, number);
                    }
                }
                DeferredPaneWrite::PreviewConfirm {
                    ws_idx,
                    pane_id,
                    signature,
                    number,
                    attempts_left,
                    ..
                } => {
                    if self.pane_user_input_recent(ws_idx, pane_id) {
                        warn!(
                            pane_id = ?pane_id,
                            "preview answer confirm dropped: operator input is recent"
                        );
                        continue;
                    }
                    self.finish_preview_confirm(ws_idx, pane_id, signature, number, attempts_left);
                }
                DeferredPaneWrite::PreviewScanStep {
                    ws_idx, pane_id, ..
                } => {
                    self.preview_scan_step(ws_idx, pane_id);
                }
                DeferredPaneWrite::AmendStep {
                    ws_idx,
                    pane_id,
                    question,
                    number,
                    note,
                    phase,
                    attempts_left,
                    codex,
                    ..
                } => {
                    if self.pane_user_input_recent(ws_idx, pane_id) {
                        warn!(
                            pane_id = ?pane_id,
                            "permission note answer dropped: operator input is recent"
                        );
                        continue;
                    }
                    self.amend_step(
                        ws_idx,
                        pane_id,
                        question,
                        number,
                        note,
                        phase,
                        attempts_left,
                        codex,
                    );
                }
            }
        }
        true
    }

    /// One scan pass over tracked panes and blocked agent panes.
    fn scan_screen_prompts(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if !self.state.decisions_screen_prompts {
            return false;
        }

        // Snapshot candidate panes: watched panes (any state, so a prompt that
        // leaves while the agent briefly reports working still reconciles)
        // plus every blocked agent pane that could be holding a prompt.
        let mut candidates: Vec<(usize, PaneId, bool)> = Vec::new();
        for pane_id in self
            .screen_prompt_watches
            .keys()
            .copied()
            .collect::<Vec<_>>()
        {
            if let Some((ws_idx, _)) = self.state.find_pane(pane_id) {
                candidates.push((ws_idx, pane_id, true));
            } else {
                self.release_watch(pane_id);
                changed = true;
            }
        }
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in workspace.tabs.iter() {
                for (pane_id, pane) in tab.panes.iter() {
                    if self.screen_prompt_watches.contains_key(pane_id) {
                        continue;
                    }
                    let blocked = self
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .is_some_and(|terminal| terminal.state == AgentState::Blocked);
                    if blocked {
                        candidates.push((ws_idx, *pane_id, false));
                    }
                }
            }
        }

        for (ws_idx, pane_id, allow_unblocked) in candidates {
            let screen = self
                .lookup_runtime_sender(ws_idx, pane_id)
                .map(|runtime| runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS));
            let blocked = self.pane_is_blocked(ws_idx, pane_id);
            match screen {
                Some(screen) => {
                    changed |= self.reconcile_pane_prompt(
                        ws_idx,
                        pane_id,
                        &screen,
                        blocked,
                        allow_unblocked,
                    );
                }
                None => {
                    // The pane's runtime is gone; release the watch so its
                    // pending decision does not linger.
                    changed |= self.release_watch(pane_id);
                }
            }
        }

        // Keep scanning while any blocked agent pane or live watch exists.
        if !self.screen_prompt_watches.is_empty()
            || self
                .state
                .terminals
                .values()
                .any(|terminal| terminal.state == AgentState::Blocked)
        {
            self.screen_prompt_scan_at = Some(now + SCREEN_PROMPT_SCAN_INTERVAL);
        }
        changed
    }

    /// Compare the current parse against the pane's watch, cancelling stale
    /// decisions after two consecutive mismatches and creating decisions for
    /// new prompts.
    fn reconcile_pane_prompt(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        screen: &str,
        blocked: bool,
        allow_unblocked: bool,
    ) -> bool {
        let prompt = parse_screen_prompt(screen);
        let current_sig = prompt.as_ref().map(ScreenPrompt::signature);
        let mut changed = false;

        // A person operating the pane — typing into a free-text field,
        // moving the cursor, answering on screen — freezes the watch:
        // no new decisions, no cancels, no in-place refreshes from
        // mid-edit frames. Once the input settles the next scan
        // reconciles whatever is on screen exactly once. A
        // preview-bearing question first seen under their hands still
        // counts as walked — the scan must never move a cursor the
        // operator is using, not even retroactively after they stop.
        if self.pane_user_input_recent(ws_idx, pane_id) {
            if let Some(prompt) = &prompt {
                if prompt.preview_box
                    && matches!(
                        prompt.focused,
                        Some(crate::screen_prompt::Focused::Option(_))
                    )
                    && prompt
                        .choices
                        .keys()
                        .any(|number| !prompt.previews.contains_key(number))
                {
                    self.preview_scan_done
                        .entry(pane_id)
                        .or_default()
                        .insert(preview_scan_key(prompt));
                }
            }
            return false;
        }

        if let Some(watch) = self.screen_prompt_watches.get_mut(&pane_id) {
            if current_sig.as_deref() == Some(watch.signature.as_str()) {
                watch.mismatches = 0;
                if let Some(prompt) = prompt.as_ref().filter(|prompt| {
                    prompt.is_codex_question() && prompt.notice != watch.notice && !watch.resolved
                }) {
                    return self.refresh_watched_decision(
                        ws_idx,
                        pane_id,
                        prompt,
                        current_sig.as_deref().unwrap_or_default(),
                    );
                }
                return false;
            }
            watch.mismatches += 1;
            if watch.mismatches < SCREEN_PROMPT_MISMATCH_LIMIT {
                return false;
            }
        }
        if self.screen_prompt_watches.contains_key(&pane_id) {
            // Two consecutive mismatched reads: the prompt is gone or changed.
            // A same-prompt redraw (only volatile text changed) or a pane
            // churning rebuilds refreshes the pending decision in place — no
            // resolved+created pair per redraw to flood watchers.
            if let (Some(prompt), Some(sig)) = (&prompt, &current_sig) {
                if self.refresh_watched_decision(ws_idx, pane_id, prompt, sig) {
                    return true;
                }
                // This release leads into a fresh create for a new prompt.
                self.record_pane_rebuild(pane_id);
            }
            changed |= self.release_watch(pane_id);
        }

        // Fresh prompt: adopt or create a decision. Watched panes keep
        // creating while a flow advances even if `blocked` briefly flaps.
        let Some(prompt) = prompt else {
            return changed;
        };
        if !(blocked || allow_unblocked) {
            return changed;
        }
        changed |= self.create_or_adopt_watch(ws_idx, pane_id, &prompt, prompt.signature());
        changed
    }

    /// End a watch, cancelling its pending decision when it was left
    /// unresolved. Returns true when a decision was cancelled.
    fn release_watch(&mut self, pane_id: PaneId) -> bool {
        let Some(watch) = self.screen_prompt_watches.remove(&pane_id) else {
            return false;
        };
        if watch.resolved {
            return false;
        }
        let mut store = match DecisionStore::open_active() {
            Ok(store) => store,
            Err(err) => {
                warn!(error = %err, "failed to open decision store for screen prompt");
                return false;
            }
        };
        match store.cancel(&watch.decision_id) {
            Ok(ResolveOutcome::Resolved(decision)) => {
                debug!(
                    pane_id = ?pane_id,
                    decision_id = %watch.decision_id,
                    "screen prompt left; cancelling decision"
                );
                self.emit_decision_resolved(*decision);
                true
            }
            Ok(ResolveOutcome::NotFound) | Ok(ResolveOutcome::AlreadyResolved) => false,
            Err(err) => {
                warn!(
                    decision_id = %watch.decision_id,
                    error = %err,
                    "failed to cancel resolved screen prompt decision"
                );
                false
            }
        }
    }

    /// Everything a screen prompt contributes to a decision record, shared
    /// by create and by the in-place refresh paths.
    fn prompt_decision_parts(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
    ) -> Option<PromptDecisionParts> {
        let kind = if prompt.is_question() {
            DecisionKind::AgentPrompt
        } else {
            DecisionKind::ToolPermission
        };
        let options: Vec<DecisionOption> = prompt
            .choices
            .iter()
            .map(|(number, label)| DecisionOption {
                id: number.to_string(),
                label: label.clone(),
                role: option_role(label),
                description: prompt.descriptions.get(number).cloned(),
                preview: prompt.previews.get(number).cloned(),
                // Claude Code paints only the focused option's preview; when
                // a preview box is up, options without a captured preview are
                // explicitly marked so nothing is silently dropped. Options
                // the preview scan already visited and saw draw no box are
                // confirmed absent, not unavailable.
                preview_unavailable: prompt.preview_box
                    && !prompt.previews.contains_key(number)
                    && !prompt.preview_none.contains(number),
            })
            .collect();
        // Claude Code permission prompts take a note on any option.
        let allow_text =
            prompt.free_text.is_some() || (kind == DecisionKind::ToolPermission && prompt.amend);
        if options.is_empty() && !allow_text {
            return None;
        }
        let public_id = self.public_pane_id(ws_idx, pane_id);
        let pane = self.pane_info(ws_idx, pane_id);
        let title = {
            let title = prompt.title();
            if title.is_empty() {
                let agent = pane
                    .as_ref()
                    .and_then(|pane| pane.agent.clone())
                    .unwrap_or_else(|| "Agent".to_string());
                match kind {
                    DecisionKind::AgentPrompt => format!("{agent} question"),
                    _ => format!("{agent} permission"),
                }
            } else {
                title
            }
        };
        // Volatile notices (auto-deny countdown, digit-free) ride in `body`
        // so a deadline is visible without churning the decision title.
        let progress = prompt.progress();
        let body = [prompt.notice(), progress.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let body = (!body.is_empty()).then_some(body);
        // The AskUserQuestion tab label + question progress ride next to the
        // dialog header; `body` keeps carrying them for Slack/glasses.
        let question_label = progress.filter(|label| !label.is_empty());
        let origin = {
            let mut origin = DecisionOrigin {
                pane_id: public_id.clone(),
                agent: pane.as_ref().and_then(|pane| pane.agent.clone()),
                cwd: pane.as_ref().and_then(|pane| pane.cwd.clone()),
                reason: Some(SCREEN_PROMPT_REASON.to_string()),
                machine: Some("local".to_string()),
                ..DecisionOrigin::default()
            };
            if let Some(server) = &prompt.mcp_server {
                origin.agent = Some(format!(
                    "{} / MCP server “{server}”",
                    origin.agent.as_deref().unwrap_or("Claude Code")
                ));
            }
            self.enrich_decision_origin(&mut origin);
            origin
        };
        Some(PromptDecisionParts {
            public_id,
            kind,
            options,
            allow_text,
            title,
            body,
            question_label,
            origin,
        })
    }

    /// Identical MCP source and option ids+labels after volatile-display
    /// normalisation, including the flood-brake and startup adoption paths.
    fn same_prompt_options(existing: &Decision, parts: &PromptDecisionParts) -> bool {
        let server = |origin: Option<&DecisionOrigin>| {
            origin
                .and_then(|origin| origin.agent.as_deref())
                .and_then(|agent| agent.split_once(" / MCP server “"))
                .and_then(|(_, server)| server.strip_suffix('”'))
                .map(str::to_string)
        };
        if server(existing.origin.as_ref()) != server(Some(&parts.origin)) {
            return false;
        }
        existing
            .options
            .iter()
            .map(|option| (option.id.clone(), normalize_volatile(&option.label)))
            .eq(parts
                .options
                .iter()
                .map(|option| (option.id.clone(), normalize_volatile(&option.label))))
    }

    /// The same prompt redrawn: identical kind/answer channels, option
    /// ids+labels+descriptions, and question once volatile displays
    /// (countdowns/timers/spinners) are normalised. A description change is
    /// a different prompt — it keeps cancel+create so stale approvals stay
    /// rejected.
    fn same_prompt_family(
        existing: &Decision,
        parts: &PromptDecisionParts,
        prompt: &ScreenPrompt,
    ) -> bool {
        // Whitespace is layout, not content: decisions created before line
        // breaks were kept still match their redrawn prompt.
        let compact = |title: &str| -> String {
            normalize_volatile(title)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect()
        };
        Self::same_prompt_shape(existing, parts, prompt)
            && compact(&existing.title) == compact(&parts.title)
    }

    /// The same prompt with the top of its question scrolled off: after a
    /// live handoff an agent pane starts blank and the agent repaints only
    /// what fits, so a tall question reads from the middle. The pending
    /// decision keeps the full question instead of being replaced by the
    /// cut-off one.
    fn same_prompt_cut_top(
        existing: &Decision,
        parts: &PromptDecisionParts,
        prompt: &ScreenPrompt,
    ) -> bool {
        Self::same_prompt_shape(existing, parts, prompt)
            && title_is_cut_top_of(&parts.title, &existing.title)
    }

    /// Everything but the question text matches: kind, answer channels and
    /// option ids+labels+descriptions.
    fn same_prompt_shape(
        existing: &Decision,
        parts: &PromptDecisionParts,
        prompt: &ScreenPrompt,
    ) -> bool {
        let same_descriptions = existing
            .options
            .iter()
            .map(|option| option.description.as_deref().map(normalize_volatile))
            .eq(parts
                .options
                .iter()
                .map(|option| option.description.as_deref().map(normalize_volatile)));
        existing.kind == parts.kind
            && existing.allow_text == parts.allow_text
            && existing.multi_select == prompt.multi_select
            && Self::same_prompt_options(existing, parts)
            && same_descriptions
    }

    /// Keep the watched pane's pending decision when the re-parsed prompt is
    /// the same prompt redrawn (volatile text changed) or when the pane is
    /// churning rebuilds — update the record in place and re-arm the watch
    /// on the new signature instead of cancelling and recreating. Returns
    /// false when no eligible pending decision exists so the caller falls
    /// through to release+create.
    fn refresh_watched_decision(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        sig: &str,
    ) -> bool {
        let Some(watch) = self.screen_prompt_watches.get(&pane_id) else {
            return false;
        };
        let decision_id = watch.decision_id.clone();
        let Some(parts) = self.prompt_decision_parts(ws_idx, pane_id, prompt) else {
            return false;
        };
        let mut store = match DecisionStore::open_active() {
            Ok(store) => store,
            Err(err) => {
                warn!(error = %err, "failed to open decision store for screen prompt");
                return false;
            }
        };
        let Ok(Some(existing)) = store.get(&decision_id) else {
            return false;
        };
        if existing.status != DecisionStatus::Pending || !Self::is_screen_prompt_decision(&existing)
        {
            return false;
        }
        let churned =
            Self::same_prompt_options(&existing, &parts) && self.pane_is_flooding(pane_id);
        if !Self::same_prompt_family(&existing, &parts, prompt)
            && !Self::same_prompt_cut_top(&existing, &parts, prompt)
            && !churned
        {
            return false;
        }
        let mut updated = existing.clone();
        parts.apply_to(&mut updated);
        keep_full_title(&existing, &mut updated);
        match store.update_pending(&updated) {
            Ok(true) => {
                if let Some(watch) = self.screen_prompt_watches.get_mut(&pane_id) {
                    watch.signature = sig.to_string();
                    watch.mismatches = 0;
                    watch.resolved = false;
                    watch.free_text_slot = prompt.free_text;
                    watch.codex_question = prompt.is_codex_question();
                    watch.notice = prompt.notice.clone();
                }
                true
            }
            Ok(false) => false,
            Err(err) => {
                warn!(error = %err, "failed to refresh screen prompt decision");
                false
            }
        }
    }

    /// Reuse a still-pending decision for an identical prompt (server restart,
    /// watch lost on handoff) or create a fresh one.
    fn create_or_adopt_watch(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        sig: String,
    ) -> bool {
        // A send-only confirmation page (`Submit answers` / `Cancel`) never
        // gets a preview scan and never becomes a decision; it is touched
        // only to auto-submit a fully herdr-answered multi-question flow.
        if is_submit_confirmation(prompt) {
            return self.handle_submit_confirmation(ws_idx, pane_id, prompt, &sig);
        }

        // Claude Code paints only the focused option's preview box. Before
        // creating the decision, walk the cursor through the other options
        // once so every preview is captured. A question is walked at most
        // once, keyed by its text and option labels — a redraw or typed
        // character wobbling the volatile signature must not re-arm it.
        let scan_key = preview_scan_key(prompt);
        if prompt.preview_box
            && matches!(
                prompt.focused,
                Some(crate::screen_prompt::Focused::Option(_))
            )
            && prompt
                .choices
                .keys()
                .any(|number| !prompt.previews.contains_key(number))
            && !self.preview_scans.contains_key(&pane_id)
            && !self
                .preview_scan_done
                .get(&pane_id)
                .is_some_and(|done| done.contains(&scan_key))
        {
            if self.pane_user_input_recent(ws_idx, pane_id) {
                // A person is operating the pane: this question is never
                // walked. The decision still creates; previews stay marked
                // unavailable for options the walk never reached.
                self.preview_scan_done
                    .entry(pane_id)
                    .or_default()
                    .insert(scan_key);
            } else {
                self.start_preview_scan(ws_idx, pane_id, prompt, sig);
                return false;
            }
        }
        if self.preview_scans.contains_key(&pane_id) {
            return false;
        }
        let Some(parts) = self.prompt_decision_parts(ws_idx, pane_id, prompt) else {
            return false;
        };

        let mut store = match DecisionStore::open_active() {
            Ok(store) => store,
            Err(err) => {
                warn!(error = %err, "failed to open decision store for screen prompt");
                return false;
            }
        };
        let pending = match store.list(Some(DecisionStatus::Pending)) {
            Ok(decisions) => decisions,
            Err(err) => {
                warn!(error = %err, "failed to list decisions for screen prompt");
                return false;
            }
        };
        let mut changed = false;
        for existing in pending.iter().filter(|decision| {
            Self::is_screen_prompt_decision(decision)
                && decision
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.pane_id == parts.public_id)
        }) {
            if Self::same_prompt_family(existing, &parts, prompt)
                || Self::same_prompt_cut_top(existing, &parts, prompt)
                || (Self::same_prompt_options(existing, &parts) && self.pane_is_flooding(pane_id))
            {
                // Same prompt still pending — refresh the record in place
                // and adopt it so a restarted server does not re-offer. The
                // flood-brake branch covers rebuild churn; both avoid a
                // resolved+created pair per redraw.
                let mut updated = existing.clone();
                parts.apply_to(&mut updated);
                keep_full_title(existing, &mut updated);
                match store.update_pending(&updated) {
                    Ok(true) => {
                        self.screen_prompt_watches.insert(
                            pane_id,
                            ScreenPromptWatch {
                                signature: sig,
                                decision_id: existing.decision_id.clone(),
                                free_text_slot: prompt.free_text,
                                codex_question: prompt.is_codex_question(),
                                notice: prompt.notice.clone(),
                                resolved: false,
                                mismatches: 0,
                            },
                        );
                        return changed;
                    }
                    // Resolved between list() and the update — no longer a
                    // pending decision to adopt.
                    Ok(false) => continue,
                    Err(err) => {
                        warn!(error = %err, "failed to update screen prompt decision in place");
                        continue;
                    }
                }
            }
            // A different prompt on this pane holds a pending decision; it is
            // stale, so cancel it before offering the new one.
            self.record_pane_rebuild(pane_id);
            match store.cancel(&existing.decision_id) {
                Ok(ResolveOutcome::Resolved(decision)) => {
                    self.emit_decision_resolved(*decision);
                    changed = true;
                }
                Ok(_) => {}
                Err(err) => warn!(error = %err, "failed to replace stale screen prompt decision"),
            }
        }

        let decision = Decision {
            decision_id: new_decision_id(),
            kind: parts.kind,
            title: parts.title,
            body: parts.body,
            options: parts.options,
            allow_text: parts.allow_text,
            multi_select: prompt.multi_select,
            question_label: parts.question_label,
            origin: Some(parts.origin),
            created_unix_ms: unix_ms_now(),
            expires_unix_ms: None,
            status: DecisionStatus::Pending,
            answer: None,
        };
        if let Err(err) = store.insert(&decision) {
            warn!(error = %err, "failed to create screen prompt decision");
            return changed;
        }
        debug!(
            pane_id = ?pane_id,
            decision_id = %decision.decision_id,
            "screen prompt decision created"
        );
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::DecisionCreated,
            data: crate::api::schema::EventData::DecisionCreated {
                decision: decision.clone(),
            },
        });
        self.screen_prompt_watches.insert(
            pane_id,
            ScreenPromptWatch {
                signature: sig,
                decision_id: decision.decision_id,
                free_text_slot: prompt.free_text,
                codex_question: prompt.is_codex_question(),
                notice: prompt.notice.clone(),
                resolved: false,
                mismatches: 0,
            },
        );
        true
    }

    /// Send an answered decision's keys/text to the pane. The fresh parse is
    /// checked first so a stale answer types into the current prompt, not
    /// whatever the screen showed when the decision was created.
    fn deliver_screen_prompt_answer(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        decision: &Decision,
        answer: Option<&crate::api::schema::DecisionAnswer>,
    ) {
        let Some(screen) = self
            .lookup_runtime_sender(ws_idx, pane_id)
            .map(|runtime| runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS))
        else {
            warn!(pane_id = ?pane_id, "screen prompt answer dropped: pane gone");
            return;
        };
        let prompt = parse_screen_prompt(&screen);

        // Same freshness rule as the answer-side gate, kept at the
        // key-sending layer too: never type into a prompt that is not the
        // one this decision was raised for. A torn read is tolerated while
        // the watch's last scan still matched.
        let watch = self
            .screen_prompt_watches
            .get(&pane_id)
            .filter(|watch| watch.decision_id == decision.decision_id);
        let fresh = match (&prompt, watch) {
            (Some(prompt), Some(watch)) => prompt.signature() == watch.signature,
            (None, Some(watch)) => watch.mismatches == 0,
            _ => false,
        };
        if !fresh {
            warn!(
                pane_id = ?pane_id,
                decision_id = %decision.decision_id,
                "screen prompt changed before answer delivery; skipping keys"
            );
            return;
        }

        // A person's input reached the pane after the answer was committed:
        // never type over them. The API-side gate refuses to commit answers
        // during active input, so reaching here means input raced the commit.
        if self.pane_user_input_recent(ws_idx, pane_id) {
            warn!(
                pane_id = ?pane_id,
                decision_id = %decision.decision_id,
                "screen prompt answer dropped: operator input is recent"
            );
            return;
        }

        if let (Some(prompt), Some(answer)) = (prompt.as_ref(), answer) {
            let carries_answer = answer.option_id.is_some()
                || answer
                    .option_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.is_empty())
                || answer.text.is_some();
            if carries_answer {
                self.record_screen_prompt_answer(pane_id, prompt);
            }
        }

        // An unreadable MCP screen must never fall through to numeric delivery.
        if prompt.as_ref().is_some_and(|p| p.mcp_server.is_some())
            || decision
                .origin
                .as_ref()
                .and_then(|o| o.agent.as_deref())
                .is_some_and(|a| a.contains("/ MCP server “"))
        {
            if let (Some(live), Some(number)) = (
                prompt.as_ref().filter(|p| p.mcp_server.is_some()),
                answer
                    .and_then(|a| a.option_id.as_deref())
                    .and_then(|id| id.parse::<u32>().ok())
                    .filter(|n| matches!(n, 1 | 2)),
            ) {
                self.start_mcp_answer(ws_idx, pane_id, live, number);
            }
            return;
        }

        if decision.multi_select {
            self.deliver_multi_select(ws_idx, pane_id, decision, prompt.as_ref(), answer);
            return;
        }

        // Claude Code permission prompts take a note per option (`Tab to
        // amend`); such decisions are the only tool permissions that allow
        // text.
        let amend = decision.kind == DecisionKind::ToolPermission && decision.allow_text;

        // Free-text answers arrive without an option id and take the typing
        // path. An option with accompanying text keeps both: see below.
        let Some(number) = answer
            .and_then(|answer| answer.option_id.as_deref())
            .and_then(|option_id| option_id.parse::<u32>().ok())
        else {
            if let Some(text) = answer.and_then(|answer| answer.text.as_deref()) {
                // A permission prompt has no free-text slot: a note on its
                // own rides on the reject option ("No, <note>"), the same
                // place Claude Code's "tell Claude what to do differently"
                // text goes.
                if amend {
                    let reject = decision
                        .options
                        .iter()
                        .rev()
                        .find(|option| option.role == DecisionOptionRole::Reject)
                        .and_then(|option| option.id.parse::<u32>().ok());
                    match (reject, prompt.as_ref()) {
                        (Some(number), Some(prompt)) => {
                            self.start_amend(ws_idx, pane_id, prompt, number, text);
                        }
                        _ => warn!(
                            pane_id = ?pane_id,
                            decision_id = %decision.decision_id,
                            "permission note dropped: no reject option or live prompt"
                        ),
                    }
                    return;
                }
                self.deliver_free_text(ws_idx, pane_id, decision, prompt.as_ref(), text);
            } else {
                debug!(
                    pane_id = ?pane_id,
                    "screen prompt answer has no numeric option; nothing to send"
                );
            }
            return;
        };
        if amend {
            let note = answer
                .and_then(|answer| answer.text.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty());
            if let (Some(note), Some(live)) = (note, prompt.as_ref()) {
                self.start_amend(ws_idx, pane_id, live, number, note);
                return;
            }
        }
        // Prefer the live screen: the prompt may have moved (multi-question
        // tabs) since the decision was created.
        let shortcut = prompt
            .as_ref()
            .and_then(|prompt| prompt.shortcuts.get(&number))
            .cloned()
            .or_else(|| {
                prompt
                    .as_ref()
                    .and_then(|prompt| prompt.choices.get(&number))
                    .or_else(|| {
                        decision
                            .options
                            .iter()
                            .find(|option| option.id == number.to_string())
                            .map(|option| &option.label)
                    })
                    .and_then(|label| crate::screen_prompt::shortcut_hint(label))
            });
        let note = answer
            .and_then(|answer| answer.text.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty());
        // A question with option previews moves the cursor on a digit and
        // selects on Enter, so the digit is followed by a confirm pass.
        let preview_confirm = match (&shortcut, prompt.as_ref()) {
            (None, Some(prompt))
                if !prompt.multi_select
                    && (prompt.preview_box
                        || decision
                            .options
                            .iter()
                            .any(|option| option.preview.is_some())) =>
            {
                Some(prompt.signature())
            }
            _ => None,
        };
        // An option answered together with text must not drop the text. A
        // plain question has no place for both, so the free-text slot gets
        // the chosen option followed by the text.
        if let (Some(note), None, None) = (note, &shortcut, &preview_confirm) {
            let label = prompt
                .as_ref()
                .and_then(|prompt| prompt.choices.get(&number))
                .cloned()
                .or_else(|| {
                    decision
                        .options
                        .iter()
                        .find(|option| option.id == number.to_string())
                        .map(|option| option.label.clone())
                });
            let has_slot = prompt
                .as_ref()
                .and_then(|prompt| prompt.free_text)
                .is_some()
                || self
                    .screen_prompt_watches
                    .get(&pane_id)
                    .filter(|watch| watch.decision_id == decision.decision_id)
                    .and_then(|watch| watch.free_text_slot)
                    .is_some();
            if let (Some(label), true) = (label, has_slot) {
                let combined = format!("「{label}」を選択。{note}");
                self.deliver_free_text(ws_idx, pane_id, decision, prompt.as_ref(), &combined);
                return;
            }
            warn!(
                pane_id = ?pane_id,
                decision_id = %decision.decision_id,
                "screen prompt has no free-text slot; the answer text is not delivered"
            );
        }
        let bytes = match shortcut {
            Some(OptionShortcut::Escape) => {
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    warn!(pane_id = ?pane_id, "screen prompt answer dropped: pane gone");
                    return;
                };
                Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                )))
            }
            Some(OptionShortcut::Char(ch)) => Bytes::copy_from_slice(ch.to_string().as_bytes()),
            None => Bytes::copy_from_slice(number.to_string().as_bytes()),
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "screen prompt answer dropped: pane gone");
            return;
        };
        if let Err(err) = runtime.try_send_bytes(bytes) {
            warn!(
                pane_id = ?pane_id,
                error = %err,
                "failed to send screen prompt answer"
            );
            return;
        }
        if let Some(signature) = preview_confirm {
            // Preview questions carry a notes field: `n` opens it on the
            // focused option and the note travels with the selection.
            let mut confirm_at = Instant::now() + FREE_TEXT_ENTER_DELAY;
            if let Some(note) = note {
                let now = Instant::now();
                self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                    at: now + FREE_TEXT_OPTION_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: Bytes::from_static(b"n"),
                });
                self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                    at: now + FREE_TEXT_OPTION_DELAY + FREE_TEXT_ENTER_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: Bytes::copy_from_slice(note.as_bytes()),
                });
                confirm_at = now + FREE_TEXT_OPTION_DELAY + FREE_TEXT_ENTER_DELAY * 2;
            }
            self.deferred_pane_writes
                .push(DeferredPaneWrite::PreviewConfirm {
                    at: confirm_at,
                    ws_idx,
                    pane_id,
                    signature,
                    number,
                    attempts_left: PREVIEW_CONFIRM_RETRIES,
                });
        }
    }

    fn start_mcp_answer(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        number: u32,
    ) {
        let Some(Focused::Option(current @ (1 | 2))) = prompt.focused else {
            return;
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return;
        };
        if current != number {
            let code = if number == 1 {
                crossterm::event::KeyCode::Up
            } else {
                crossterm::event::KeyCode::Down
            };
            let bytes = runtime.encode_terminal_key(crate::input::TerminalKey::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            ));
            if runtime.try_send_bytes(Bytes::from(bytes)).is_err() {
                return;
            }
        }
        self.deferred_pane_writes
            .push(DeferredPaneWrite::McpConfirm {
                at: Instant::now() + FREE_TEXT_ENTER_DELAY,
                ws_idx,
                pane_id,
                signature: prompt.signature(),
                number,
            });
    }

    fn finish_mcp_confirm(&mut self, ws_idx: usize, pane_id: PaneId, signature: &str, number: u32) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return;
        };
        let screen = runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS);
        let Some(prompt) = parse_screen_prompt(&screen) else {
            return;
        };
        if prompt.mcp_server.is_none()
            || prompt.signature() != signature
            || prompt.focused != Some(Focused::Option(number))
        {
            return;
        }
        let bytes = runtime.encode_terminal_key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        if let Err(error) = runtime.try_send_bytes(Bytes::from(bytes)) {
            warn!(%error, "MCP answer confirmation failed");
        }
    }

    /// Press Enter on a preview-layout question once the cursor sits on the
    /// answered option. A changed prompt means the answer already advanced
    /// (or the screen moved on), so nothing is sent.
    fn finish_preview_confirm(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        signature: String,
        number: u32,
        attempts_left: u8,
    ) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "preview answer confirm dropped: pane gone");
            return;
        };
        let screen = runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS);
        let prompt = parse_screen_prompt(&screen);
        let same_prompt = prompt
            .as_ref()
            .is_some_and(|prompt| prompt.signature() == signature);
        let on_answer = same_prompt
            && prompt
                .as_ref()
                .is_some_and(|prompt| prompt.focused == Some(Focused::Option(number)));
        if on_answer {
            let enter = Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )));
            if let Err(err) = runtime.try_send_bytes(enter) {
                warn!(
                    pane_id = ?pane_id,
                    error = %err,
                    "failed to confirm preview answer"
                );
            }
            return;
        }
        // A torn read or a cursor still redrawing is retried; a different
        // parsed prompt is the screen moving on and ends the pass.
        if (prompt.is_none() || same_prompt) && attempts_left > 0 {
            self.deferred_pane_writes
                .push(DeferredPaneWrite::PreviewConfirm {
                    at: Instant::now() + MULTISELECT_SUBMIT_RETRY_DELAY,
                    ws_idx,
                    pane_id,
                    signature,
                    number,
                    attempts_left: attempts_left - 1,
                });
            return;
        }
        if prompt.is_none() || same_prompt {
            warn!(
                pane_id = ?pane_id,
                number,
                "preview answer confirm dropped: cursor never reached the answer"
            );
        }
    }

    /// Begin a permission answer with a note: the first step runs right
    /// away and schedules the rest.
    fn start_amend(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        number: u32,
        note: &str,
    ) {
        // The note field is a single line: a newline would submit early.
        let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
        self.amend_step(
            ws_idx,
            pane_id,
            normalize_volatile(&prompt.question),
            number,
            note,
            AmendPhase::Aim,
            AMEND_ATTEMPTS,
            false,
        );
    }

    /// One step of a permission answer with a note. Each step re-reads the
    /// pane and only acts when the same prompt is on screen with the cursor
    /// on the answered option, so a wrong option is never confirmed.
    #[allow(clippy::too_many_arguments)] // mirrors the AmendStep fields
    fn amend_step(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        question: String,
        number: u32,
        note: String,
        phase: AmendPhase,
        attempts_left: u8,
        codex: bool,
    ) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "permission note answer dropped: pane gone");
            return;
        };
        let screen = runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS);
        let prompt = parse_screen_prompt(&screen);
        let same_prompt = prompt.as_ref().is_some_and(|prompt| {
            if codex {
                prompt.is_codex_question() && prompt.signature() == question
            } else {
                normalize_volatile(&prompt.question) == question
            }
        });
        let focused = prompt.as_ref().and_then(|prompt| prompt.focused);
        let key = |code| {
            Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            )))
        };
        // The footer shows `Tab to amend` only while the cursor sits on an
        // option that takes a note ("Yes, and always allow …" does not), and
        // drops it once the note is open.
        let note_open = prompt.as_ref().is_some_and(|prompt| {
            if codex {
                prompt.codex_notes_open
            } else {
                !prompt.amend
            }
        });
        let (send, next) = match phase {
            AmendPhase::Aim if same_prompt && codex => match focused {
                Some(Focused::Option(at)) if at == number && note_open => (
                    Some(Bytes::copy_from_slice(note.as_bytes())),
                    Some((AmendPhase::Submit, FREE_TEXT_ENTER_DELAY)),
                ),
                Some(Focused::Option(at)) if !note_open => {
                    let arrow = key(if at < number {
                        crossterm::event::KeyCode::Down
                    } else {
                        crossterm::event::KeyCode::Up
                    });
                    let mut bytes = arrow.repeat(at.abs_diff(number) as usize);
                    bytes.extend_from_slice(&key(crossterm::event::KeyCode::Tab));
                    (
                        Some(Bytes::from(bytes)),
                        Some((AmendPhase::Type, FREE_TEXT_OPTION_DELAY)),
                    )
                }
                _ => (None, None),
            },
            AmendPhase::Aim if same_prompt => match focused {
                // On the option but no hint (yet): the redraw may lag, so the
                // step retries; an option that never offers a note stops.
                Some(Focused::Option(at)) if at == number && note_open => (None, None),
                Some(Focused::Option(at)) if at == number => (
                    Some(key(crossterm::event::KeyCode::Tab)),
                    Some((AmendPhase::Type, FREE_TEXT_OPTION_DELAY)),
                ),
                Some(Focused::Option(at)) if attempts_left > 0 => (
                    Some(key(if number > at {
                        crossterm::event::KeyCode::Down
                    } else {
                        crossterm::event::KeyCode::Up
                    })),
                    Some((AmendPhase::Aim, TOGGLE_KEY_DELAY)),
                ),
                Some(Focused::Option(_)) => {
                    warn!(
                        pane_id = ?pane_id,
                        number,
                        "permission note answer dropped: the cursor never reached the option"
                    );
                    return;
                }
                _ => (None, None),
            },
            AmendPhase::Type
                if same_prompt && note_open && focused == Some(Focused::Option(number)) =>
            {
                (
                    Some(Bytes::copy_from_slice(note.as_bytes())),
                    Some((AmendPhase::Submit, FREE_TEXT_ENTER_DELAY)),
                )
            }
            AmendPhase::Submit
                if same_prompt && note_open && focused == Some(Focused::Option(number)) =>
            {
                (Some(key(crossterm::event::KeyCode::Enter)), None)
            }
            _ => (None, None),
        };
        if let Some(bytes) = send {
            if let Err(err) = runtime.try_send_bytes(bytes) {
                warn!(
                    pane_id = ?pane_id,
                    error = %err,
                    "failed to send permission note answer"
                );
                return;
            }
            if let Some((next_phase, delay)) = next {
                // Arrow steps spend the budget so a cursor that never moves
                // cannot loop forever.
                let attempts_left = if next_phase == phase {
                    attempts_left - 1
                } else {
                    attempts_left
                };
                self.deferred_pane_writes
                    .push(DeferredPaneWrite::AmendStep {
                        at: Instant::now() + delay,
                        ws_idx,
                        pane_id,
                        question,
                        number,
                        note,
                        phase: next_phase,
                        attempts_left,
                        codex,
                    });
            }
            return;
        }
        // A torn read or a screen still redrawing is retried; anything else
        // means the prompt moved on, and nothing more is typed.
        if (prompt.is_none() || same_prompt) && attempts_left > 0 {
            self.deferred_pane_writes
                .push(DeferredPaneWrite::AmendStep {
                    at: Instant::now() + MULTISELECT_SUBMIT_RETRY_DELAY,
                    ws_idx,
                    pane_id,
                    question,
                    number,
                    note,
                    phase,
                    attempts_left: attempts_left - 1,
                    codex,
                });
            return;
        }
        warn!(
            pane_id = ?pane_id,
            number,
            phase = ?phase,
            "permission note answer dropped: the prompt never reached the expected state"
        );
    }

    /// Free-text answers select the "Type something" slot when one is on
    /// screen, then type the text, then Enter.
    fn deliver_free_text(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        decision: &Decision,
        prompt: Option<&ScreenPrompt>,
        text: &str,
    ) {
        let watch = self
            .screen_prompt_watches
            .get(&pane_id)
            .filter(|watch| watch.decision_id == decision.decision_id);
        let codex = prompt.is_some_and(ScreenPrompt::is_codex_question)
            || watch.is_some_and(|watch| watch.codex_question);
        if codex {
            let signature = watch
                .map(|watch| watch.signature.clone())
                .or_else(|| prompt.map(ScreenPrompt::signature));
            let slot = prompt
                .and_then(|prompt| prompt.free_text)
                .or_else(|| watch.and_then(|watch| watch.free_text_slot));
            if let (Some(signature), Some(number)) = (signature, slot) {
                self.amend_step(
                    ws_idx,
                    pane_id,
                    signature,
                    number,
                    text.to_string(),
                    AmendPhase::Aim,
                    AMEND_ATTEMPTS,
                    true,
                );
            }
            return;
        }
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return;
        };
        let text_bytes = Bytes::copy_from_slice(text.as_bytes());
        let enter = Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )));
        // The slot number: the live prompt first, then the slot recorded on
        // the watch when the live read missed it.
        let slot = prompt.and_then(|prompt| prompt.free_text).or_else(|| {
            self.screen_prompt_watches
                .get(&pane_id)
                .filter(|watch| watch.decision_id == decision.decision_id)
                .and_then(|watch| watch.free_text_slot)
        });
        match slot {
            Some(number) => {
                let selection = Bytes::copy_from_slice(number.to_string().as_bytes());
                if let Err(err) = runtime.try_send_bytes(selection) {
                    warn!(
                        pane_id = ?pane_id,
                        error = %err,
                        "failed to select free-text option"
                    );
                    return;
                }
                let now = Instant::now();
                self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                    at: now + FREE_TEXT_OPTION_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: text_bytes,
                });
                self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                    at: now + FREE_TEXT_OPTION_DELAY + FREE_TEXT_ENTER_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: enter,
                });
            }
            None => {
                if let Err(err) = runtime.try_send_bytes(text_bytes) {
                    warn!(
                        pane_id = ?pane_id,
                        error = %err,
                        "failed to send screen prompt answer"
                    );
                    return;
                }
                self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                    at: Instant::now() + FREE_TEXT_ENTER_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: enter,
                });
            }
        }
    }

    /// Phase 1 of a multiSelect answer: toggle every checkbox that differs
    /// between the screen state and the answer, then schedule a re-read that
    /// aims arrow keys at the free-text slot / `Next`/`Submit` row and sends
    /// Enter — matching what a user would press.
    fn deliver_multi_select(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        decision: &Decision,
        prompt: Option<&ScreenPrompt>,
        answer: Option<&crate::api::schema::DecisionAnswer>,
    ) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "screen prompt answer dropped: pane gone");
            return;
        };
        let Some(prompt) = prompt.filter(|prompt| prompt.multi_select) else {
            warn!(
                pane_id = ?pane_id,
                decision_id = %decision.decision_id,
                "multi-select answer needs a live multi-select prompt parse"
            );
            return;
        };
        // Desired checked set: every numbered option the answer names, plus
        // the free-text slot when the answer carries text.
        let mut desired: std::collections::BTreeSet<u32> = answer
            .into_iter()
            .flat_map(|answer| {
                answer
                    .option_ids
                    .iter()
                    .flatten()
                    .chain(answer.option_id.iter())
            })
            .filter_map(|id| id.parse::<u32>().ok())
            .filter(|number| prompt.choices.contains_key(number))
            .collect();
        let text = answer.and_then(|answer| answer.text.as_deref());
        if text.is_some() {
            if let Some(slot) = prompt.free_text {
                desired.insert(slot);
            }
        }
        let toggles: Vec<u32> = prompt
            .checked
            .symmetric_difference(&desired)
            .copied()
            .filter(|number| {
                prompt.choices.contains_key(number) || prompt.free_text == Some(*number)
            })
            .collect();
        // Each option key must arrive as its own input event: Claude Code
        // treats digits bundled into one write as a single keypress, so the
        // toggles are spaced like a user tapping keys one at a time.
        let now = Instant::now();
        if let Some(number) = toggles.first() {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(number.to_string().into_bytes())) {
                warn!(
                    pane_id = ?pane_id,
                    error = %err,
                    "failed to send multi-select toggles"
                );
                return;
            }
        }
        let mut last_toggle_at = now;
        let mut deferred: Vec<DeferredPaneWrite> = toggles
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, number)| {
                last_toggle_at = now + TOGGLE_KEY_DELAY * index as u32;
                DeferredPaneWrite::Bytes {
                    at: last_toggle_at,
                    ws_idx,
                    pane_id,
                    bytes: Bytes::from(number.to_string().into_bytes()),
                }
            })
            .collect();
        deferred.push(DeferredPaneWrite::MultiSelectSubmit {
            at: last_toggle_at + FREE_TEXT_OPTION_DELAY,
            ws_idx,
            pane_id,
            decision_id: decision.decision_id.clone(),
            text: text.map(str::to_string),
            attempts_left: MULTISELECT_SUBMIT_RETRIES,
        });
        self.deferred_pane_writes.extend(deferred);
    }

    /// Begin a preview scan for `prompt`: capture the cursor's resting spot
    /// and the already-visible preview, then step the cursor toward every
    /// other option so each preview box can be read before the decision is
    /// created.
    fn start_preview_scan(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        sig: String,
    ) {
        let mut visited = std::collections::BTreeSet::new();
        if let Some(crate::screen_prompt::Focused::Option(number)) = prompt.focused {
            visited.insert(number);
        }
        let remaining: std::collections::VecDeque<u32> = prompt
            .choices
            .keys()
            .copied()
            .filter(|number| !visited.contains(number))
            .collect();
        let scan = PreviewScan {
            prompt: prompt.clone(),
            signature: sig,
            previews: prompt.previews.clone(),
            visited,
            restore: prompt.focused,
            remaining,
            restoring: false,
            // Worst case: visit each option once, then walk all the way back.
            steps_left: (prompt.choices.len() as u32) * 3 + 4,
            mismatches: 0,
            arrows_sent: false,
            scan_key: preview_scan_key(prompt),
        };
        debug!(
            pane_id = ?pane_id,
            options = ?scan.remaining,
            "preview scan started"
        );
        self.preview_scans.insert(pane_id, scan);
        self.deferred_pane_writes
            .push(DeferredPaneWrite::PreviewScanStep {
                at: Instant::now() + PREVIEW_SCAN_STEP_DELAY,
                ws_idx,
                pane_id,
            });
    }

    /// One preview-scan step: re-read the pane, harvest whatever preview box
    /// is up, then either send one arrow toward the next target (visit phase)
    /// or back toward the resting cursor position (restore phase). The scan
    /// ends by creating the decision from the enriched prompt; only Up/Down
    /// arrow keys are ever sent — never a key that could answer the prompt.
    fn preview_scan_step(&mut self, ws_idx: usize, pane_id: PaneId) {
        let Some(scan) = self.preview_scans.get_mut(&pane_id) else {
            return;
        };
        scan.steps_left = scan.steps_left.saturating_sub(1);

        // A person's input reached the pane mid-walk: stop immediately. No
        // further arrows and no restore keys — herdr never moves a cursor
        // the operator is actively using.
        if self.pane_user_input_recent(ws_idx, pane_id) {
            self.mark_preview_scan_done(pane_id);
            warn!(
                pane_id = ?pane_id,
                "preview scan aborted: operator input arrived"
            );
            self.finish_preview_scan(ws_idx, pane_id);
            return;
        }

        let Some(screen) = self
            .lookup_runtime_sender(ws_idx, pane_id)
            .map(|runtime| runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS))
        else {
            // The pane is gone; there is nothing to restore and no decision
            // to create.
            self.preview_scans.remove(&pane_id);
            return;
        };
        let live = parse_screen_prompt(&screen);
        let signature = self
            .preview_scans
            .get(&pane_id)
            .map(|scan| scan.signature.clone());
        let Some(signature) = signature else {
            return;
        };

        // Freshness: a different or unparsable screen ends the walk. The
        // resting cursor is only chased while the watched prompt is still
        // up — arrows on a different screen could move some other widget.
        if live
            .as_ref()
            .is_none_or(|prompt| prompt.signature() != signature)
        {
            let abort = {
                let scan = self.preview_scans.get_mut(&pane_id).expect("scan checked");
                scan.mismatches += 1;
                scan.mismatches >= PREVIEW_SCAN_MISMATCH_LIMIT
            };
            if abort {
                warn!(
                    pane_id = ?pane_id,
                    "preview scan aborted: prompt changed or torn"
                );
                self.mark_preview_scan_done(pane_id);
                self.finish_preview_scan(ws_idx, pane_id);
                return;
            }
            // A torn redraw frame: no arrow this step, just look again.
            self.deferred_pane_writes
                .push(DeferredPaneWrite::PreviewScanStep {
                    at: Instant::now() + PREVIEW_SCAN_STEP_DELAY,
                    ws_idx,
                    pane_id,
                });
            return;
        }
        if let Some(scan) = self.preview_scans.get_mut(&pane_id) {
            scan.mismatches = 0;
        }
        let Some(live) = live else {
            return;
        };
        let focused = live.focused;
        if let Some(scan) = self.preview_scans.get_mut(&pane_id) {
            for (number, preview) in &live.previews {
                scan.previews.insert(*number, preview.clone());
            }
            if let Some(crate::screen_prompt::Focused::Option(number)) = focused {
                scan.visited.insert(number);
            }
            // The opening frame may itself be torn; until the first arrow
            // leaves the cursor untouched, keep the true resting position.
            if !scan.arrows_sent && focused.is_some() {
                scan.restore = focused;
            }
        }

        // Pick the next arrow target: the first unvisited option, or the
        // resting position once every option was visited.
        let (target, exhausted) = {
            let scan = self.preview_scans.get_mut(&pane_id).expect("scan checked");
            if !scan.restoring {
                if scan.remaining.front().copied()
                    == focused.and_then(|f| match f {
                        crate::screen_prompt::Focused::Option(n) => Some(n),
                        crate::screen_prompt::Focused::Advance => None,
                    })
                {
                    scan.remaining.pop_front();
                }
                if scan.remaining.is_empty() {
                    scan.restoring = true;
                }
            }
            let target = if scan.restoring {
                scan.restore
            } else {
                scan.remaining
                    .front()
                    .copied()
                    .map(crate::screen_prompt::Focused::Option)
            };
            (target, scan.steps_left == 0)
        };
        if target.is_none() || target == focused || exhausted {
            self.mark_preview_scan_done(pane_id);
            self.finish_preview_scan(ws_idx, pane_id);
            return;
        }
        let arrow = match (focused, target) {
            (
                Some(crate::screen_prompt::Focused::Option(from)),
                Some(crate::screen_prompt::Focused::Option(to)),
            ) if to != from => Some(if to > from {
                crossterm::event::KeyCode::Down
            } else {
                crossterm::event::KeyCode::Up
            }),
            (
                Some(crate::screen_prompt::Focused::Option(_)),
                Some(crate::screen_prompt::Focused::Advance),
            ) => Some(crossterm::event::KeyCode::Down),
            (
                Some(crate::screen_prompt::Focused::Advance),
                Some(crate::screen_prompt::Focused::Option(_)),
            ) => Some(crossterm::event::KeyCode::Up),
            _ => None,
        };
        if let Some(code) = arrow {
            let sent = self.lookup_runtime_sender(ws_idx, pane_id).map(|runtime| {
                runtime.try_send_bytes(Bytes::from(runtime.encode_terminal_key(
                    crate::input::TerminalKey::new(code, crossterm::event::KeyModifiers::NONE),
                )))
            });
            match sent {
                Some(Ok(())) => {
                    if let Some(scan) = self.preview_scans.get_mut(&pane_id) {
                        scan.arrows_sent = true;
                    }
                }
                Some(Err(err)) => {
                    warn!(pane_id = ?pane_id, error = %err, "preview scan arrow failed");
                    self.mark_preview_scan_done(pane_id);
                    self.finish_preview_scan(ws_idx, pane_id);
                    return;
                }
                None => {
                    self.preview_scans.remove(&pane_id);
                    return;
                }
            }
        } else if let Some(scan) = self.preview_scans.get_mut(&pane_id) {
            // Cannot compute a direction — stop visiting, try to restore.
            scan.restoring = true;
        }
        // The arrow only moves the cursor; the next step re-reads the screen
        // so previews are attributed from real state, never assumed.
        self.deferred_pane_writes
            .push(DeferredPaneWrite::PreviewScanStep {
                at: Instant::now() + PREVIEW_SCAN_STEP_DELAY,
                ws_idx,
                pane_id,
            });
    }

    /// End a preview scan: enrich the snapshot prompt with the previews read
    /// during the walk, mark the options actually visited as confirmed
    /// preview-less, and hand the enriched prompt to the normal decision
    /// path (unvisited options keep their `preview_unavailable` marker).
    fn finish_preview_scan(&mut self, ws_idx: usize, pane_id: PaneId) {
        let Some(scan) = self.preview_scans.remove(&pane_id) else {
            return;
        };
        let mut prompt = scan.prompt;
        prompt.previews = scan.previews;
        prompt.preview_none = scan.visited;
        self.create_or_adopt_watch(ws_idx, pane_id, &prompt, scan.signature);
    }

    /// Phase 2 of a multiSelect answer: re-parse so the arrow plan uses the
    /// real cursor position, move onto the free-text slot / advance row,
    /// type any text, and press Enter on `Next`/`Submit`.
    fn finish_multi_select_submit(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        decision_id: &str,
        text: Option<&str>,
        attempts_left: u8,
    ) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "multi-select submit dropped: pane gone");
            return;
        };
        let screen = runtime.detection_text_unwrapped_extra(SCREEN_PROMPT_EXTRA_ROWS);
        let prompt = parse_screen_prompt(&screen);
        // Same freshness rule as phase 1: the visible prompt must still be
        // the watched one before keys are sent. Toggle keys redraw the
        // prompt, so a torn or mid-redraw read is retried a few times before
        // giving up.
        let fresh = prompt.as_ref().is_some_and(|prompt| {
            self.screen_prompt_watches
                .get(&pane_id)
                .is_some_and(|watch| {
                    watch.decision_id == decision_id && watch.signature == prompt.signature()
                })
        });
        if !fresh {
            if attempts_left > 0 {
                self.deferred_pane_writes
                    .push(DeferredPaneWrite::MultiSelectSubmit {
                        at: Instant::now() + MULTISELECT_SUBMIT_RETRY_DELAY,
                        ws_idx,
                        pane_id,
                        decision_id: decision_id.to_string(),
                        text: text.map(str::to_string),
                        attempts_left: attempts_left - 1,
                    });
                return;
            }
            warn!(
                pane_id = ?pane_id,
                watch_sig = ?self.screen_prompt_watches.get(&pane_id).map(|w| w.signature.as_str()),
                live_sig = ?prompt.as_ref().map(ScreenPrompt::signature),
                "multi-select submit dropped: prompt changed mid-answer"
            );
            return;
        }
        let Some(prompt) = prompt else {
            return;
        };
        let Some(advance_after) = prompt.advance_after else {
            warn!(
                pane_id = ?pane_id,
                "multi-select submit dropped: no advance row on screen"
            );
            return;
        };
        // Selectable rows form one sequence: numbered items 1..=advance_after,
        // then the advance row, then any numbered items below it (e.g.
        // "Chat about this").
        let index_of = |focused: crate::screen_prompt::Focused| -> i64 {
            let after = advance_after as i64;
            match focused {
                crate::screen_prompt::Focused::Advance => after,
                crate::screen_prompt::Focused::Option(n) => {
                    let n = n as i64;
                    if n <= after {
                        n - 1
                    } else {
                        after + (n - after)
                    }
                }
            }
        };
        let encode = |code: crossterm::event::KeyCode| -> Bytes {
            Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            )))
        };
        let arrows = |from: i64, to: i64| -> Bytes {
            let code = if to >= from {
                crossterm::event::KeyCode::Down
            } else {
                crossterm::event::KeyCode::Up
            };
            let count = (to - from).unsigned_abs() as usize;
            Bytes::from(vec![encode(code); count].concat())
        };
        let focus = index_of(
            prompt
                .focused
                .unwrap_or(crate::screen_prompt::Focused::Option(1)),
        );
        let slot = prompt.free_text.or_else(|| {
            self.screen_prompt_watches
                .get(&pane_id)
                .filter(|watch| watch.decision_id == decision_id)
                .and_then(|watch| watch.free_text_slot)
        });
        let now = Instant::now();
        if let (Some(text), Some(slot)) = (text, slot) {
            // Focus the free-text slot first — digits in the text would
            // toggle options if the cursor sat on a checkbox row.
            let slot_index = index_of(crate::screen_prompt::Focused::Option(slot));
            let to_slot = arrows(focus, slot_index);
            // From the slot the advance row sits further down; Enter submits.
            let to_submit: Bytes = [
                arrows(slot_index, index_of(crate::screen_prompt::Focused::Advance)),
                encode(crossterm::event::KeyCode::Enter),
            ]
            .concat()
            .into();
            if let Err(err) = runtime.try_send_bytes(to_slot) {
                warn!(pane_id = ?pane_id, error = %err, "failed to focus free-text slot");
                return;
            }
            self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                at: now + FREE_TEXT_ENTER_DELAY,
                ws_idx,
                pane_id,
                bytes: Bytes::copy_from_slice(text.as_bytes()),
            });
            self.deferred_pane_writes.push(DeferredPaneWrite::Bytes {
                at: now + FREE_TEXT_OPTION_DELAY,
                ws_idx,
                pane_id,
                bytes: to_submit,
            });
        } else {
            let submit = [
                arrows(focus, index_of(crate::screen_prompt::Focused::Advance)),
                encode(crossterm::event::KeyCode::Enter),
            ]
            .concat();
            if let Err(err) = runtime.try_send_bytes(submit.into()) {
                warn!(pane_id = ?pane_id, error = %err, "failed to send multi-select submit");
            }
        }
    }

    /// Cancel every pending screen-prompt decision and drop all watches
    /// (feature toggled off via live config).
    pub(crate) fn cancel_all_screen_prompt_watches(&mut self) {
        let pane_ids: Vec<PaneId> = self.screen_prompt_watches.keys().copied().collect();
        for pane_id in pane_ids {
            self.release_watch(pane_id);
        }
        self.deferred_pane_writes.clear();
        self.preview_scans.clear();
        self.preview_scan_done.clear();
        self.screen_prompt_flows.clear();
        self.screen_prompt_auto_submits.clear();
        self.screen_prompt_scan_at = None;
    }

    /// Whether a workspace pane's terminal currently reports blocked.
    fn pane_is_blocked(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .is_some_and(|terminal| terminal.state == AgentState::Blocked)
    }
}

/// Stable identity for "this question" when deciding whether its preview
/// walk already ran: the question text and numbered option labels after
/// volatile-display normalisation. Descriptions, previews, checked state,
/// cursor position and free-text slots are deliberately excluded — a
/// redraw, a torn read, or a character typed into a free-text field must
/// not re-arm the one-time walk.
fn preview_scan_key(prompt: &ScreenPrompt) -> String {
    serde_json::to_string(&(
        normalize_volatile(&prompt.question),
        prompt
            .choices
            .iter()
            .map(|(number, label)| (*number, normalize_volatile(label)))
            .collect::<std::collections::BTreeMap<u32, String>>(),
    ))
    .unwrap_or_default()
}

/// The "send the collected answers" page: AskUserQuestion's final
/// `Submit answers` / `Cancel` screen (`mq.at_submit`), or — when the
/// multi-question tab row scrolled out of view — a confirm-only prompt
/// whose question mentions submitting and that offers a `Submit…` option
/// without a free-text slot. These pages never get a preview scan and are
/// never surfaced as decisions; see `handle_submit_confirmation`.
fn is_submit_confirmation(prompt: &ScreenPrompt) -> bool {
    if prompt.mq.as_ref().is_some_and(|mq| mq.at_submit) {
        return true;
    }
    prompt.free_text.is_none()
        && prompt.question.to_lowercase().contains("submit")
        && prompt
            .choices
            .values()
            .any(|label| label.trim_start().to_lowercase().starts_with("submit"))
}

fn option_role(label: &str) -> DecisionOptionRole {
    let lower = label.trim_start().to_lowercase();
    if lower.starts_with("yes")
        || lower == "accept"
        || lower.starts_with("approve")
        || lower.starts_with("allow")
        || lower.starts_with("proceed")
        || lower.starts_with("はい")
    {
        DecisionOptionRole::Approve
    } else if lower.starts_with("no")
        || lower == "decline"
        || lower.starts_with("deny")
        || lower.starts_with("reject")
        || lower.starts_with("cancel")
        || lower.starts_with("exit")
        || lower.starts_with("いいえ")
    {
        DecisionOptionRole::Reject
    } else {
        DecisionOptionRole::Other
    }
}

#[cfg(test)]
mod tests {

    const MCP_ACCEPT: &str =
        include_str!("../../tests/fixtures/screen_prompts/claude_mcp_accept.txt");
    const MCP_DECLINE: &str =
        include_str!("../../tests/fixtures/screen_prompts/claude_mcp_decline.txt");

    #[tokio::test]
    async fn mcp_server_change_cancels_old_approval_before_accepting_answers() {
        let _guard = crate::config::lock_test_config_env();
        for restart in [false, true] {
            for flood in [false, true] {
                for cut_title in [false, true] {
                    let (mut app, pane, _) = test_app("mcp-source", &crlf(MCP_ACCEPT), true);
                    app.scan_screen_prompts(Instant::now());
                    let old = pending_decisions().pop().unwrap();
                    if restart {
                        app.screen_prompt_watches.remove(&pane);
                    }
                    if flood {
                        for _ in 0..SCREEN_PROMPT_FLOOD_LIMIT {
                            app.record_pane_rebuild(pane);
                        }
                    }
                    let mut next = MCP_ACCEPT.replace("codex-cu", "different-server");
                    if cut_title {
                        next = next.replace("Allow Computer Use to use", "use");
                    }
                    let mut rx = replace_screen(&mut app, pane, &crlf(&next));
                    rescan_until_adopted(&mut app);
                    let current = pending_decisions().pop().unwrap();
                    assert_ne!(
                        old.decision_id, current.decision_id,
                        "restart={restart} flood={flood} cut={cut_title}"
                    );
                    assert!(all_decisions()
                        .iter()
                        .any(|d| d.decision_id == old.decision_id
                            && d.status == DecisionStatus::Cancelled));
                    let response = answer_response(&mut app, &old.decision_id, Some("1"), None);
                    assert!(response.contains("\"error\""));
                    app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(5));
                    assert!(rx.try_recv().is_err(), "no keys into the other MCP server");
                }
            }
        }
    }

    #[tokio::test]
    async fn mcp_answers_use_arrows_confirm_focus_then_enter_in_both_directions() {
        let _guard = crate::config::lock_test_config_env();
        for (screen, target, next, arrow) in [
            (MCP_ACCEPT, "2", MCP_DECLINE, b'B'),
            (MCP_DECLINE, "1", MCP_ACCEPT, b'A'),
        ] {
            let (mut app, pane, mut rx) = test_app("mcp-arrows", &crlf(screen), true);
            app.scan_screen_prompts(Instant::now());
            let decision = pending_decisions().pop().expect("MCP decision");
            assert_eq!(decision.kind, DecisionKind::ToolPermission);
            assert!(decision
                .origin
                .as_ref()
                .unwrap()
                .agent
                .as_deref()
                .unwrap()
                .contains("codex-cu"));
            assert_eq!(decision.options[0].role, DecisionOptionRole::Approve);
            assert_eq!(decision.options[1].role, DecisionOptionRole::Reject);
            assert!(!decision.allow_text);
            assert!(app.pane_has_visible_blocker(0, pane));
            assert!(app
                .guard_external_input(0, pane, true, Some(&format!("p{}", pane.raw())))
                .is_err());
            let refused = app
                .guard_external_input(0, pane, false, Some(&format!("p{}", pane.raw())))
                .unwrap_err();
            assert_eq!(refused.code, "pane_confirmation_pending");
            assert!(rx.try_recv().is_err(), "external refusal sends no keys");
            answer(&mut app, &decision.decision_id, Some(target), None);
            assert_eq!(drain_arrow(&mut rx), arrow);
            rx = replace_screen(&mut app, pane, &crlf(next));
            app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(5));
            assert_eq!(
                rx.try_recv().expect("Enter after focus check").as_ref(),
                b"\r"
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn mcp_confirmation_sends_nothing_on_change_disappearance_wrong_focus_or_human_input() {
        let _guard = crate::config::lock_test_config_env();
        for (next, human) in [
            (MCP_ACCEPT.to_string(), false),
            (MCP_DECLINE.replace("TextEdit", "Other"), false),
            (MCP_DECLINE.replace("codex-cu", "other-server"), false),
            (IDLE_SCREEN.to_string(), false),
            (MCP_DECLINE.to_string(), true),
        ] {
            let (mut app, pane, mut rx) = test_app("mcp-abort", &crlf(MCP_ACCEPT), true);
            app.scan_screen_prompts(Instant::now());
            let decision = pending_decisions().pop().unwrap();
            answer(&mut app, &decision.decision_id, Some("2"), None);
            assert_eq!(drain_arrow(&mut rx), b'B');
            rx = replace_screen(&mut app, pane, &crlf(&next));
            if human {
                app.lookup_runtime_sender(0, pane)
                    .unwrap()
                    .note_user_input();
            }
            app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(5));
            assert!(rx.try_recv().is_err());
            assert!(app.deferred_pane_writes.is_empty());
        }
    }

    #[tokio::test]
    async fn mcp_answer_sends_no_initial_keys_after_input_or_unreadable_or_changed_screen() {
        let _guard = crate::config::lock_test_config_env();
        for (screen, human) in [
            (MCP_ACCEPT.to_string(), true),
            (IDLE_SCREEN.to_string(), false),
            (MCP_ACCEPT.replace("TextEdit", "Other"), false),
        ] {
            let (mut app, pane, _) = test_app("mcp-initial-abort", &crlf(MCP_ACCEPT), true);
            app.scan_screen_prompts(Instant::now());
            let decision = pending_decisions().pop().unwrap();
            let mut rx = replace_screen(&mut app, pane, &crlf(&screen));
            if human {
                app.lookup_runtime_sender(0, pane)
                    .unwrap()
                    .note_user_input();
            }
            let answer = crate::api::schema::DecisionAnswer {
                option_id: Some("2".into()),
                option_ids: None,
                text: None,
                responder: None,
                answered_unix_ms: 0,
            };
            app.deliver_screen_prompt_answer(0, pane, &decision, Some(&answer));
            app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(5));
            assert!(rx.try_recv().is_err());
            assert!(app.deferred_pane_writes.is_empty());
        }
    }

    #[tokio::test]
    async fn mcp_already_focused_answer_is_rechecked_before_enter() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _, mut rx) = test_app("mcp-same", &crlf(MCP_ACCEPT), true);
        app.scan_screen_prompts(Instant::now());
        let id = pending_decisions().pop().unwrap().decision_id;
        answer(&mut app, &id, Some("1"), None);
        assert!(rx.try_recv().is_err());
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(5));
        assert_eq!(rx.try_recv().unwrap().as_ref(), b"\r");
    }
    use super::*;
    use crate::api::schema::DecisionAnswerParams;
    use crate::workspace::Workspace;

    const CC_QUESTION: &str = "\
グラスで見えるか確認します

❯ 1. りんご
     1つ目の答え
  2. みかん
     選ばない
  3. Type something.
───────────────────────────────────────────────────────────────
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    const CC_PERMISSION: &str = "\
Do you want to proceed?

❯ 1. Yes
  2. No, and tell Claude what to do differently (esc)

Esc to cancel
";

    // Claude Code 2.1.301 permission prompt captured from a live pane: the
    // footer offers a per-option note on Tab.
    fn cc_amend_permission(options: &str, footer: &str) -> String {
        format!(
            "\
 Bash command
 Delete *x.png files in ./probe
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 D=./probe; rm -f $D/*x.png
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Glob patterns are not allowed in write operations.

 Do you want to proceed?
{options}

 {footer}
"
        )
    }

    const AMEND_ON_YES: &str = "\
 ❯ 1. Yes
   2. Yes, and always allow access to probe from this project
   3. No";
    const AMEND_ON_ALLOW: &str = "\
   1. Yes
 ❯ 2. Yes, and always allow access to probe from this project
   3. No";
    const AMEND_ON_NO: &str = "\
   1. Yes
   2. Yes, and always allow access to probe from this project
 ❯ 3. No";
    const AMEND_NO_OPEN: &str = "\
   1. Yes
   2. Yes, and always allow access to probe from this project
 ❯ 3. No, and tell Claude what to do differently";
    const AMEND_NO_TYPED: &str = "\
   1. Yes
   2. Yes, and always allow access to probe from this project
 ❯ 3. No, 代わりに ls だけして";
    const AMEND_HINT: &str = "Esc to cancel · Tab to amend";

    const CODEX_PERMISSION: &str = "\
  Would you like to run the following command?

› 1. Yes, proceed (y)
  2. No, and tell Codex what to do differently (esc)

Press enter to confirm or esc to cancel
";

    const PERM_LS: &str = "\
Bash(ls -la)

Do you want to proceed?

❯ 1. Yes
  2. No (esc)

Esc to cancel
";

    // Identical options to PERM_LS; only the asked command differs.
    const PERM_RM: &str = "\
Bash(rm -rf /tmp/x)

Do you want to proceed?

❯ 1. Yes
  2. No (esc)

Esc to cancel
";

    const IDLE_SCREEN: &str = "compiling half the world\nthis may take a while\n";

    fn test_app(
        tag: &str,
        screen: &str,
        blocked: bool,
    ) -> (App, PaneId, tokio::sync::mpsc::Receiver<Bytes>) {
        let dir =
            std::env::temp_dir().join(format!("herdr-screen-prompt-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::env::set_var(crate::dispatch::HERDR_DB_PATH_ENV_VAR, dir.join("herdr.db"));
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, receiver) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                100,
                30,
                0,
                screen.as_bytes(),
                16,
            );
        workspace.tabs[0].runtimes.insert(pane_id, runtime);
        app.state.workspaces.push(workspace);
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        if blocked {
            let terminal = app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal");
            terminal.state = AgentState::Blocked;
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
        }
        (app, pane_id, receiver)
    }

    fn replace_screen(
        app: &mut App,
        pane_id: PaneId,
        screen: &str,
    ) -> tokio::sync::mpsc::Receiver<Bytes> {
        let (runtime, receiver) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                100,
                30,
                0,
                screen.as_bytes(),
                16,
            );
        app.state.workspaces[0].tabs[0]
            .runtimes
            .insert(pane_id, runtime);
        receiver
    }

    fn pending_decisions() -> Vec<Decision> {
        DecisionStore::open_active()
            .expect("store")
            .list(Some(DecisionStatus::Pending))
            .expect("list")
    }

    fn all_decisions() -> Vec<Decision> {
        DecisionStore::open_active()
            .expect("store")
            .list(None)
            .expect("list")
    }

    fn answer_response(
        app: &mut App,
        decision_id: &str,
        option_id: Option<&str>,
        text: Option<&str>,
    ) -> String {
        app.handle_decision_answer(
            "test".to_string(),
            DecisionAnswerParams {
                decision_id: decision_id.to_string(),
                option_id: option_id.map(str::to_string),
                option_ids: None,
                text: text.map(str::to_string),
                responder: None,
            },
        )
    }

    fn answer(app: &mut App, decision_id: &str, option_id: Option<&str>, text: Option<&str>) {
        let response = answer_response(app, decision_id, option_id, text);
        assert!(
            response.contains("\"decision\""),
            "answer failed: {response}"
        );
    }

    #[tokio::test]
    async fn blocked_pane_prompt_creates_agent_prompt_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("create", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        let decision = &decisions[0];
        assert_eq!(decision.kind, DecisionKind::AgentPrompt);
        assert!(decision.title.contains("グラスで見えるか確認"));
        assert_eq!(
            decision
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert_eq!(
            decision.options[0].description.as_deref(),
            Some("1つ目の答え")
        );
        assert_eq!(decision.options[1].description.as_deref(), Some("選ばない"));
        assert!(decision.allow_text);
        let origin = decision.origin.as_ref().expect("origin");
        let public_id = origin.pane_id.as_deref().expect("pane_id");
        assert_eq!(
            app.parse_pane_id(public_id),
            Some((0, pane_id)),
            "origin pane_id resolves back to the test pane"
        );
        assert!(origin.agent.is_some());
        assert_eq!(origin.reason.as_deref(), Some(SCREEN_PROMPT_REASON));

        // Rescanning the same screen is a no-op (dedup by signature).
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);
        assert_eq!(app.screen_prompt_watches.len(), 1);
        assert!(app.screen_prompt_watches.contains_key(&pane_id));
    }

    #[tokio::test]
    async fn claude_narrow_detected_question_creates_multiselect_decision() {
        let _guard = crate::config::lock_test_config_env();
        let screen = include_str!("../detect/fixtures/claude-narrow-multi-question.txt");
        let (mut app, pane_id, _rx) = test_app("narrow-question", &crlf(screen), false);
        let result = crate::detect::manifest::explain(crate::detect::Agent::Claude, screen);
        assert_eq!(result.state, AgentState::Blocked);
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.state = result.state;
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        app.scan_screen_prompts(Instant::now());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        let decision = &decisions[0];
        assert_eq!(decision.kind, DecisionKind::AgentPrompt);
        assert_eq!(decision.title, "Which sections should the report include?");
        assert!(decision.multi_select);
        assert!(decision.allow_text);
        assert!(decision
            .question_label
            .as_deref()
            .unwrap()
            .contains("Sections"));
        assert_eq!(
            decision
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["Summary", "Evidence"]
        );
        assert_eq!(
            decision.options[0].description.as_deref(),
            Some("A concise overview.")
        );
    }

    #[tokio::test]
    async fn unblocked_pane_creates_no_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, _rx) = test_app("idle", CC_QUESTION, false);
        app.scan_screen_prompts(Instant::now());
        assert!(pending_decisions().is_empty());
        assert!(app.screen_prompt_watches.is_empty());
    }

    #[tokio::test]
    async fn disabled_feature_creates_and_scans_nothing() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, _rx) = test_app("disabled", CC_QUESTION, true);
        app.state.decisions_screen_prompts = false;
        app.screen_prompt_scan_at = None;
        app.scan_screen_prompts(Instant::now());
        assert!(pending_decisions().is_empty());
        assert!(app.screen_prompt_scan_at.is_none());
    }

    #[tokio::test]
    async fn permission_prompt_creates_tool_permission_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, _rx) = test_app("perm", CC_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].kind, DecisionKind::ToolPermission);
        assert!(!decisions[0].allow_text);
        assert_eq!(decisions[0].options.len(), 2);
        assert_eq!(decisions[0].options[0].role, DecisionOptionRole::Approve);
    }

    #[tokio::test]
    async fn amend_permission_allows_a_note() {
        let _guard = crate::config::lock_test_config_env();
        let screen = cc_amend_permission(AMEND_ON_YES, AMEND_HINT);
        let (mut app, _pane_id, _rx) = test_app("amend", &crlf(&screen), true);
        app.scan_screen_prompts(Instant::now());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].kind, DecisionKind::ToolPermission);
        assert!(decisions[0].allow_text);
        assert_eq!(decisions[0].options.len(), 3);
        assert_eq!(decisions[0].options[2].role, DecisionOptionRole::Reject);
    }

    #[tokio::test]
    async fn note_alone_rides_on_no_through_tab_amend() {
        let _guard = crate::config::lock_test_config_env();
        let screen = cc_amend_permission(AMEND_ON_YES, AMEND_HINT);
        let (mut app, pane_id, mut rx) = test_app("amendno", &crlf(&screen), true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        let later = || Instant::now() + Duration::from_secs(60);

        answer(&mut app, &decision_id, None, Some("代わりに\nls だけして"));
        // Cursor on Yes, answer is No: the first arrow goes out at once.
        let down = rx.try_recv().expect("arrow");
        assert!(down.as_ref().starts_with(b"\x1b"), "{down:?}");
        assert!(rx.try_recv().is_err(), "no digit: a digit would select");

        // The cursor has not moved yet: another arrow.
        app.flush_deferred_pane_writes(later());
        assert_eq!(rx.try_recv().expect("arrow again"), down);

        // Passing "always allow", which takes no note: the footer loses its
        // hint there, and the walk keeps going.
        rx = replace_screen(
            &mut app,
            pane_id,
            &crlf(&cc_amend_permission(AMEND_ON_ALLOW, "Esc to cancel")),
        );
        app.flush_deferred_pane_writes(later());
        assert_eq!(rx.try_recv().expect("arrow past allow"), down);

        // Cursor on No: Tab opens the note.
        rx = replace_screen(
            &mut app,
            pane_id,
            &crlf(&cc_amend_permission(AMEND_ON_NO, AMEND_HINT)),
        );
        app.flush_deferred_pane_writes(later());
        assert_eq!(rx.try_recv().expect("tab").as_ref(), b"\t");

        // The note field has not opened yet: nothing is typed.
        app.flush_deferred_pane_writes(later());
        assert!(
            rx.try_recv().is_err(),
            "typing must wait for the note field"
        );

        // Note field open (footer lost its Tab hint): the note, one line.
        rx = replace_screen(
            &mut app,
            pane_id,
            &crlf(&cc_amend_permission(AMEND_NO_OPEN, "Esc to cancel")),
        );
        app.flush_deferred_pane_writes(later());
        assert_eq!(
            rx.try_recv().expect("note").as_ref(),
            "代わりに ls だけして".as_bytes()
        );

        rx = replace_screen(
            &mut app,
            pane_id,
            &crlf(&cc_amend_permission(AMEND_NO_TYPED, "Esc to cancel")),
        );
        app.flush_deferred_pane_writes(later());
        assert_eq!(rx.try_recv().expect("enter").as_ref(), b"\r");
        assert!(app.deferred_pane_writes.is_empty());
    }

    #[tokio::test]
    async fn option_with_note_tabs_on_the_focused_option() {
        let _guard = crate::config::lock_test_config_env();
        let screen = cc_amend_permission(AMEND_ON_YES, AMEND_HINT);
        let (mut app, _pane_id, mut rx) = test_app("amendyes", &crlf(&screen), true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("1"), Some("結果を一言で"));
        // Already on Yes: Tab, never the digit (which would answer at once
        // and drop the note).
        assert_eq!(rx.try_recv().expect("tab").as_ref(), b"\t");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn note_answer_stops_when_the_prompt_moves_on() {
        let _guard = crate::config::lock_test_config_env();
        let screen = cc_amend_permission(AMEND_ON_NO, AMEND_HINT);
        let (mut app, pane_id, mut rx) = test_app("amendgone", &crlf(&screen), true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("3"), Some("やめて"));
        assert_eq!(rx.try_recv().expect("tab").as_ref(), b"\t");

        // A different prompt replaced it: nothing more is typed.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PERMISSION));
        for _ in 0..20 {
            app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        }
        assert!(rx.try_recv().is_err());
        assert!(app.deferred_pane_writes.is_empty());
    }

    #[tokio::test]
    async fn prompt_disappearance_cancels_after_two_reads() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("gone", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);

        // A single torn/blank read must not cancel.
        let _ = replace_screen(&mut app, pane_id, &crlf(IDLE_SCREEN));
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);
        assert!(app.screen_prompt_watches.contains_key(&pane_id));

        // Two consecutive prompt-less reads cancel the decision.
        app.scan_screen_prompts(Instant::now());
        assert!(pending_decisions().is_empty());
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].status, DecisionStatus::Cancelled);
        assert!(!app.screen_prompt_watches.contains_key(&pane_id));
    }

    #[tokio::test]
    async fn prompt_change_replaces_decision_after_two_reads() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("change", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].decision_id.clone();

        let _ = replace_screen(&mut app, pane_id, CC_PERMISSION);
        app.scan_screen_prompts(Instant::now());
        // First mismatched read: old decision still pending.
        assert_eq!(pending_decisions()[0].decision_id, first);
        app.scan_screen_prompts(Instant::now());

        let decisions = all_decisions();
        assert_eq!(decisions.len(), 2);
        let (old, new) = (&decisions[0], &decisions[1]);
        assert_eq!(old.decision_id, first);
        assert_eq!(old.status, DecisionStatus::Cancelled);
        assert_eq!(new.status, DecisionStatus::Pending);
        assert_eq!(new.kind, DecisionKind::ToolPermission);
    }

    #[tokio::test]
    async fn same_options_different_description_replaces_decision() {
        // Identical labels; only option 1's description row differs. The
        // description is part of the signature, so this is a new prompt.
        const CC_QUESTION_ALT_DESC: &str = "\
グラスで見えるか確認します

❯ 1. りんご
     別の説明文に変わった
  2. みかん
     選ばない
  3. Type something.
───────────────────────────────────────────────────────────────
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("desc-change", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].clone();
        assert_eq!(first.options[0].description.as_deref(), Some("1つ目の答え"));

        let _ = replace_screen(&mut app, pane_id, CC_QUESTION_ALT_DESC);
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());

        let decisions = all_decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].status, DecisionStatus::Cancelled);
        assert_eq!(decisions[1].status, DecisionStatus::Pending);
        assert_eq!(
            decisions[1].options[0].description.as_deref(),
            Some("別の説明文に変わった")
        );
        assert_eq!(
            app.screen_prompt_watches[&pane_id].decision_id,
            decisions[1].decision_id
        );
    }

    const CC_TALL_QUESTION: &str = "\
レビューで残った点が3つあります。
① 鍵の更新の書き方が足りない。
② 古い節番号が残っている。どうしますか？

❯ 1. 直す
     資料に書き足す
  2. 直さない
     このままにする
  3. Type something.
───────────────────────────────────────────────────────────────
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    /// The same question after a live handoff: the agent repainted only
    /// the rows that fit, so the question starts mid-way.
    const CC_TALL_QUESTION_CUT_TOP: &str = "\
② 古い節番号が残っている。どうしますか？

❯ 1. 直す
     資料に書き足す
  2. 直さない
     このままにする
  3. Type something.
───────────────────────────────────────────────────────────────
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    #[tokio::test]
    async fn handoff_cut_top_question_keeps_the_full_title_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("cuttop", CC_TALL_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].clone();
        assert!(
            first.title.starts_with("レビューで残った点"),
            "{}",
            first.title
        );

        // A live handoff drops the watches; the new server reads the
        // repainted lower part of the same question.
        app.screen_prompt_watches.clear();
        let _ = replace_screen(&mut app, pane_id, CC_TALL_QUESTION_CUT_TOP);
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());

        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1, "no replacement decision");
        assert_eq!(decisions[0].decision_id, first.decision_id);
        assert_eq!(decisions[0].status, DecisionStatus::Pending);
        assert_eq!(decisions[0].title, first.title);
        assert_eq!(
            app.screen_prompt_watches[&pane_id].decision_id,
            first.decision_id
        );
    }

    #[tokio::test]
    async fn watched_question_redrawn_with_top_cut_keeps_the_full_title() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("cuttop-watch", CC_TALL_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].clone();

        let _ = replace_screen(&mut app, pane_id, CC_TALL_QUESTION_CUT_TOP);
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());

        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decision_id, first.decision_id);
        assert_eq!(decisions[0].title, first.title);
    }

    #[test]
    fn cut_top_title_match_requires_a_strict_suffix() {
        assert!(title_is_cut_top_of("② 古い節番号", "① 鍵\n② 古い節番号"));
        assert!(title_is_cut_top_of("番号が残る", "古い節\n番号が残る"));
        assert!(!title_is_cut_top_of("② 古い節番号", "② 古い節番号"));
        assert!(!title_is_cut_top_of("", "① 鍵"));
        assert!(!title_is_cut_top_of("別の質問", "① 鍵 ② 古い節番号"));
    }

    #[tokio::test]
    async fn same_options_different_question_creates_new_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("sameopts", PERM_LS, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].clone();

        // Same options, different asked command: a different prompt must not
        // be deduplicated into the first decision's watch.
        let _ = replace_screen(&mut app, pane_id, PERM_RM);
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());

        let decisions = all_decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].decision_id, first.decision_id);
        assert_eq!(decisions[0].status, DecisionStatus::Cancelled);
        assert_eq!(decisions[1].status, DecisionStatus::Pending);
        assert_ne!(decisions[1].title, first.title);
        assert!(decisions[1].title.contains("rm -rf"));
        assert_eq!(
            app.screen_prompt_watches[&pane_id].decision_id,
            decisions[1].decision_id
        );
    }

    #[tokio::test]
    async fn stale_answer_after_prompt_change_is_rejected() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("stale", CC_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();

        // The pane moved on to a different prompt before the answer arrived.
        let mut rx = replace_screen(&mut app, pane_id, PERM_LS);
        let response = answer_response(&mut app, &decision_id, Some("1"), None);
        assert!(response.contains("decision_stale"), "{response}");
        assert!(
            rx.try_recv().is_err(),
            "no keys may be sent for a stale screen prompt"
        );
        assert_eq!(all_decisions()[0].status, DecisionStatus::Cancelled);
    }

    #[tokio::test]
    async fn stale_answer_after_prompt_disappears_is_rejected() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("gone-stale", CC_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();

        // The prompt left the screen; one mismatching scan already ran.
        let mut rx = replace_screen(&mut app, pane_id, &crlf(IDLE_SCREEN));
        app.scan_screen_prompts(Instant::now());
        let response = answer_response(&mut app, &decision_id, Some("1"), None);
        assert!(response.contains("decision_stale"), "{response}");
        assert!(rx.try_recv().is_err());
        assert_eq!(all_decisions()[0].status, DecisionStatus::Cancelled);
    }

    #[tokio::test]
    async fn torn_read_still_allows_answer() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("torn", CC_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();

        // A torn read (no parseable prompt) while the last scan still
        // matched is not stale — the same one-read tolerance scans use.
        let mut rx = replace_screen(&mut app, pane_id, &crlf(IDLE_SCREEN));
        let response = answer_response(&mut app, &decision_id, Some("1"), None);
        assert!(response.contains("\"decision\""), "{response}");
        assert_eq!(rx.try_recv().expect("bytes").as_ref(), b"1");
    }

    #[tokio::test]
    async fn option_answer_sends_number_key() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("num", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("2"), None);
        assert_eq!(rx.try_recv().expect("bytes").as_ref(), b"2");
    }

    #[tokio::test]
    async fn option_answer_uses_advertised_shortcut() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("shortcut", CODEX_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].kind, DecisionKind::ToolPermission);
        let decision_id = decisions[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("1"), None);
        // Codex advertises `(y)` for option 1 — send the shortcut, not "1".
        assert_eq!(rx.try_recv().expect("bytes").as_ref(), b"y");
    }

    #[tokio::test]
    async fn esc_shortcut_answer_sends_escape() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("esc", CODEX_PERMISSION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("2"), None);
        assert_eq!(rx.try_recv().expect("bytes").as_ref(), &[0x1b]);
    }

    #[tokio::test]
    async fn codex_question_countdown_keeps_one_agent_prompt_and_delivers_notes() {
        let _guard = crate::config::lock_test_config_env();
        let screen = include_str!("../../tests/fixtures/screen_prompt/codex-question.txt");
        let timed = |time: &str| {
            screen.replace(
                "Question 1/1 (1 unanswered)",
                &format!("Question 1/1 (1 unanswered) · auto-resolves in {time}"),
            )
        };
        let (mut app, pane_id, _rx) = test_app("codex-question", screen, true);
        app.scan_screen_prompts(Instant::now());
        let original = pending_decisions();
        assert_eq!(original.len(), 1);
        assert_eq!(original[0].kind, DecisionKind::AgentPrompt);
        assert_eq!(original[0].title, "検証用コーヒーはどちらにしますか？");
        assert_eq!(
            original[0].question_label.as_deref(),
            Some("Question 1/1 (1 unanswered)")
        );
        assert!(original[0].allow_text);
        assert!(!original[0]
            .body
            .as_deref()
            .unwrap_or_default()
            .contains("auto-resolves in"));
        for time in ["1m 0s", "59s", "58s", "1s"] {
            let _rx = replace_screen(&mut app, pane_id, &timed(time));
            app.scan_screen_prompts(Instant::now());
            let pending = pending_decisions();
            assert_eq!(pending[0].decision_id, original[0].decision_id);
            assert!(pending[0]
                .body
                .as_deref()
                .expect("notice")
                .contains("auto-resolves in …"));
            assert_eq!(all_decisions().len(), 1);
        }
        let mut rx = replace_screen(&mut app, pane_id, screen);
        answer(
            &mut app,
            &original[0].decision_id,
            None,
            Some("codexq-note-verified"),
        );
        assert_eq!(
            rx.try_recv().expect("arrows and Tab").as_ref(),
            b"\x1b[B\x1b[B\t"
        );
        rx = replace_screen(
            &mut app,
            pane_id,
            include_str!("../../tests/fixtures/screen_prompt/codex-notes.txt"),
        );
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(
            rx.try_recv().expect("note").as_ref(),
            b"codexq-note-verified"
        );
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(rx.try_recv().expect("Enter").as_ref(), b"\r");
    }

    #[tokio::test]
    async fn codex_open_notes_answer_does_not_toggle_tab_and_checks_submit() {
        let _guard = crate::config::lock_test_config_env();
        let notes = include_str!("../../tests/fixtures/screen_prompt/codex-notes.txt");
        let (mut app, pane_id, mut rx) = test_app("codex-open-notes", notes, true);
        app.scan_screen_prompts(Instant::now());
        let id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &id, None, Some("open-note"));
        assert_eq!(
            rx.try_recv().expect("text without Tab").as_ref(),
            b"open-note"
        );
        assert!(rx.try_recv().is_err());
        let mut idle = replace_screen(&mut app, pane_id, IDLE_SCREEN);
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert!(
            idle.try_recv().is_err(),
            "Enter must not leak after the question disappears"
        );
    }

    #[tokio::test]
    async fn codex_torn_answer_read_waits_for_same_question_and_open_notes() {
        let _guard = crate::config::lock_test_config_env();
        let screen = include_str!("../../tests/fixtures/screen_prompt/codex-question.txt");
        let notes = include_str!("../../tests/fixtures/screen_prompt/codex-notes.txt");
        let (mut app, pane_id, _rx) = test_app("codex-torn-note", screen, true);
        app.scan_screen_prompts(Instant::now());
        let id = pending_decisions()[0].decision_id.clone();
        let mut torn = replace_screen(&mut app, pane_id, "");
        answer(&mut app, &id, None, Some("recovered-note"));
        assert!(
            torn.try_recv().is_err(),
            "no numeric fallback on a torn read"
        );
        let mut rx = replace_screen(&mut app, pane_id, screen);
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(
            rx.try_recv().expect("open editor").as_ref(),
            b"\x1b[B\x1b[B\t"
        );
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert!(
            rx.try_recv().is_err(),
            "do not type until notes editor is confirmed open"
        );
        rx = replace_screen(&mut app, pane_id, notes);
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(rx.try_recv().expect("text").as_ref(), b"recovered-note");
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(rx.try_recv().expect("Enter").as_ref(), b"\r");
    }

    #[tokio::test]
    async fn free_text_answer_types_slot_text_and_enter() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("freetext", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, None, Some("ぶどう"));
        // The "Type something" slot number is sent immediately...
        assert_eq!(rx.try_recv().expect("slot").as_ref(), b"3");
        assert!(rx.try_recv().is_err());
        // ...then the text and Enter arrive as deferred writes.
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(rx.try_recv().expect("text").as_ref(), "ぶどう".as_bytes());
        assert_eq!(rx.try_recv().expect("enter").as_ref(), b"\r");
    }

    fn queue_review_mail(app: &mut App, pane: PaneId) {
        let terminal = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
        app.state.terminals.get_mut(&terminal).unwrap().state = AgentState::Working;
        let target = app.agent_info(0, pane).unwrap().global_pane_id;
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "review-redraw-mail".into(),
            method: crate::api::schema::Method::MsgSend(crate::api::schema::MsgSendParams {
                room: "review-redraw".into(),
                project: "/repo".into(),
                from_agent: "sender".into(),
                to: target,
                body: "must stay queued".into(),
                reply_to: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        assert!(app.input_guard_waiting.contains_key(&pane));
        let mail = crate::msg::MsgStore::open_active()
            .unwrap()
            .history("review-redraw", Some("/repo"), 10)
            .unwrap();
        assert_eq!(mail.len(), 1);
        assert!(mail[0].delivered_at.is_none());
    }

    #[tokio::test]
    async fn review_real_free_text_answer_holds_mail_through_deferred_redraw() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane, mut rx) = test_app("review-free-text", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision = pending_decisions().pop().unwrap();
        answer(&mut app, &decision.decision_id, None, Some("ぶどう"));
        assert_eq!(rx.try_recv().unwrap().as_ref(), b"3");
        rx = replace_screen(&mut app, pane, "");
        queue_review_mail(&mut app, pane);
        let now = Instant::now();
        app.input_guard_tasks(now + Duration::from_secs(1));
        app.input_guard_tasks(now + Duration::from_secs(2));
        assert!(app.input_guard_waiting.contains_key(&pane));
        assert!(
            rx.try_recv().is_err(),
            "no mailbox body or Enter during deferred redraw"
        );
        app.flush_deferred_pane_writes(now + Duration::from_secs(3));
        assert_eq!(rx.try_recv().unwrap().as_ref(), "ぶどう".as_bytes());
        assert_eq!(rx.try_recv().unwrap().as_ref(), b"\r");
        assert!(rx.try_recv().is_err());
        rx = replace_screen(&mut app, pane, CC_PERMISSION);
        app.input_guard_tasks(now + Duration::from_secs(4));
        assert!(
            app.input_guard_waiting.contains_key(&pane),
            "next question resets clear reads"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn review_real_preview_scan_holds_mail_through_intermediate_frame() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane, mut rx) =
            test_app("review-preview-frame", &crlf(CC_PREVIEW_OPTION_1), true);
        app.scan_screen_prompts(Instant::now());
        assert!(app.preview_scans.contains_key(&pane));
        let now = Instant::now();
        app.flush_deferred_pane_writes(now + Duration::from_secs(1));
        assert_eq!(drain_arrow(&mut rx), b'B');
        rx = replace_screen(&mut app, pane, "partial redraw");
        queue_review_mail(&mut app, pane);
        app.input_guard_tasks(now + Duration::from_secs(2));
        app.input_guard_tasks(now + Duration::from_secs(3));
        assert!(app.input_guard_waiting.contains_key(&pane));
        assert!(rx.try_recv().is_err());
        rx = replace_screen(&mut app, pane, &crlf(CC_PREVIEW_OPTION_2));
        app.input_guard_tasks(now + Duration::from_secs(4));
        assert!(app.input_guard_waiting.contains_key(&pane));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancelled_decision_is_not_reoffered_while_prompt_stays() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, _rx) = test_app("cancel", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        let response = app.handle_decision_cancel(
            "test".to_string(),
            crate::api::schema::DecisionCancelParams {
                decision_id: decision_id.clone(),
            },
        );
        assert!(
            response.contains("\"decision\""),
            "cancel failed: {response}"
        );

        // The prompt is still on screen; rescanning must not recreate it.
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].status, DecisionStatus::Cancelled);
    }

    #[tokio::test]
    async fn feature_disable_cancels_pending_screen_prompt_decisions() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, _rx) = test_app("toggle", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);

        let config: crate::config::Config =
            toml::from_str("[decisions]\nscreen_prompts = false\n").expect("config");
        let report = app.apply_live_config(&config, &[], &[], false);
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert!(pending_decisions().is_empty());
        assert_eq!(all_decisions()[0].status, DecisionStatus::Cancelled);
        assert!(app.screen_prompt_watches.is_empty());
    }

    #[test]
    fn config_default_enables_screen_prompts() {
        let config: crate::config::Config = toml::from_str("").expect("config");
        assert!(config.decisions.screen_prompts);
        let config: crate::config::Config =
            toml::from_str("[decisions]\nscreen_prompts = false\n").expect("config");
        assert!(!config.decisions.screen_prompts);
    }

    // A preview question with three options; the preview box only ever shows
    // the focused option's preview, mirroring Claude Code.
    const CC_PREVIEW_OPTION_1: &str = "\
←  ☐ 方式  ✔ Submit  →

どの方式にする？

❯ 1. 方式A                        ┌──────────────────────────────────────────┐
  2. 方式B                        │ 図A: input → A → out                     │
  3. 方式C                        └──────────────────────────────────────────┘

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    const CC_PREVIEW_OPTION_2: &str = "\
←  ☐ 方式  ✔ Submit  →

どの方式にする？

  1. 方式A                        ┌──────────────────────────────────────────┐
❯ 2. 方式B                        │ 図B: input → B → out                     │
  3. 方式C                        └──────────────────────────────────────────┘

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    // Option 3 draws no box at all — visiting it confirms the absence.
    const CC_PREVIEW_OPTION_3: &str = "\
←  ☐ 方式  ✔ Submit  →

どの方式にする？

  1. 方式A
  2. 方式B
❯ 3. 方式C

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    /// Fixtures are written to the pane raw: without `\r` every `\n` keeps
    /// the column and stair-steps the lines, which misaligns the preview
    /// box's `┌│└` borders. Feed the terminal what a real TUI emits.
    fn crlf(screen: &str) -> String {
        screen.replace('\n', "\r\n")
    }

    /// Read one deferred scan step's arrow write; returns the direction as
    /// 'A' (Up) or 'B' (Down) from the last byte of the escape sequence.
    fn drain_arrow(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> u8 {
        let bytes = rx.try_recv().expect("an arrow write was queued");
        *bytes.last().expect("non-empty arrow bytes")
    }

    #[tokio::test]
    async fn preview_scan_suppresses_decision_creation_until_finish() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvsuppress", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        assert!(pending_decisions().is_empty());
        assert_eq!(app.preview_scans.len(), 1);

        // A reconcile tick while the scan is in flight must not create a
        // decision from a partial parse — the scan's finish does that.
        now += Duration::from_millis(300);
        app.scan_screen_prompts(now);
        assert!(
            pending_decisions().is_empty(),
            "reconcile must not create a decision while a preview scan runs"
        );

        // Walk to completion: Down, Down, Up, Up restores the cursor.
        for screen in [
            CC_PREVIEW_OPTION_2,
            CC_PREVIEW_OPTION_3,
            CC_PREVIEW_OPTION_2,
            CC_PREVIEW_OPTION_1,
        ] {
            now += Duration::from_secs(1);
            app.flush_deferred_pane_writes(now);
            drain_arrow(&mut rx);
            rx = replace_screen(&mut app, pane_id, &crlf(screen));
        }
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        let pending = pending_decisions();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].options[0].preview.is_some());
        assert!(pending[0].options[1].preview.is_some());
        assert!(pending[0].options.iter().all(|o| !o.preview_unavailable));
        let _ = rx;
    }

    #[tokio::test]
    async fn preview_scan_walks_every_option_and_restores_cursor() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvscan", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        // No decision yet — the scan runs first.
        assert!(pending_decisions().is_empty());
        assert_eq!(app.preview_scans.len(), 1);

        // Step 1: still focused on 1, sends Down toward option 2. Steps are
        // scheduled off `Instant::now()` inside the app, so jump well past
        // the delay to stay due regardless of clock granularity.
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');

        // Cursor now on 2: capture its preview, send another Down. Each
        // replace is a fresh pane runtime — keep its receiver.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_2));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');

        // Cursor on 3 (no box): all visited, walk back toward 1 — Up.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_3));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'A');

        // Passing 2 on the way back: still one more Up.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_2));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'A');

        // Back on 1: the scan ends and creates the decision.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_1));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(app.preview_scans.is_empty());
        assert!(rx.try_recv().is_err(), "no more keys after restore");

        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        let options = &decisions[0].options;
        assert_eq!(
            options[0].preview.as_deref().map(str::trim),
            Some("図A: input → A → out")
        );
        assert_eq!(
            options[1].preview.as_deref().map(str::trim),
            Some("図B: input → B → out")
        );
        // Option 3 was visited and drew no box: confirmed absent, not
        // "preview_unavailable".
        assert!(options[2].preview.is_none());
        assert!(!options[2].preview_unavailable);
        assert!(options.iter().all(|o| !o.preview_unavailable));
    }

    #[tokio::test]
    async fn real_table_preview_scan_restores_nonfirst_cursor_without_recreating_decision() {
        let _guard = crate::config::lock_test_config_env();
        let original = include_str!("../../tests/fixtures/screen_prompts/claude_preview_table.txt");
        let frame = |number: u32| {
            original
                .replace("❯ 2. 案B", "  2. 案B")
                .replace(&format!("  {number}. 案"), &format!("❯ {number}. 案"))
                .replace("[店長] --依頼-->", &format!("[選択{number}] --依頼-->"))
        };
        let (mut app, pane_id, _) = test_app("real-table-preview", IDLE_SCREEN, true);
        let install = |app: &mut App, text: &str| {
            let (runtime, rx) =
                crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                    240,
                    100,
                    0,
                    crlf(text).as_bytes(),
                    16,
                );
            app.state.workspaces[0].tabs[0]
                .runtimes
                .insert(pane_id, runtime);
            rx
        };
        let mut rx = install(&mut app, &frame(2));
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        assert!(pending_decisions().is_empty());
        for (number, key) in [(1, b'A'), (2, b'B'), (3, b'B'), (2, b'A')] {
            now += Duration::from_secs(1);
            app.flush_deferred_pane_writes(now);
            assert_eq!(drain_arrow(&mut rx), key);
            assert!(rx.try_recv().is_err());
            rx = install(&mut app, &frame(number));
        }
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(
            rx.try_recv().is_err(),
            "no keys after original cursor restored"
        );
        assert!(app.preview_scans.is_empty());
        let pending = pending_decisions();
        assert_eq!(pending.len(), 1);
        let id = pending[0].decision_id.clone();
        for (index, option) in pending[0].options.iter().enumerate() {
            assert!(option
                .preview
                .as_deref()
                .expect("visited preview")
                .contains(&format!("選択{}", index + 1)));
            assert!(!option.preview_unavailable);
        }
        for _ in 0..3 {
            now += Duration::from_secs(1);
            app.scan_screen_prompts(now);
        }
        assert_eq!(all_decisions().len(), 1, "one creation and no cancellation");
        assert_eq!(pending_decisions()[0].decision_id, id);
        assert!(rx.try_recv().is_err());
    }

    /// Walk a preview scan over the three-option fixture so the decision
    /// exists, leaving the cursor back on option 1.
    fn finish_three_option_preview_scan(
        app: &mut App,
        pane_id: PaneId,
        rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    ) -> Instant {
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        for screen in [
            CC_PREVIEW_OPTION_1,
            CC_PREVIEW_OPTION_2,
            CC_PREVIEW_OPTION_3,
            CC_PREVIEW_OPTION_2,
            CC_PREVIEW_OPTION_1,
        ] {
            *rx = replace_screen(app, pane_id, &crlf(screen));
            now += Duration::from_secs(1);
            app.flush_deferred_pane_writes(now);
            while rx.try_recv().is_ok() {}
        }
        assert!(app.preview_scans.is_empty());
        now
    }

    #[tokio::test]
    async fn preview_answer_presses_enter_once_cursor_reaches_option() {
        // Claude Code's preview layout moves the cursor on a digit and only
        // selects on Enter; without Enter the question sat answered but
        // unselected.
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvanswer", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = finish_three_option_preview_scan(&mut app, pane_id, &mut rx);
        let decision_id = pending_decisions()[0].decision_id.clone();

        answer(&mut app, &decision_id, Some("3"), None);
        assert_eq!(rx.try_recv().expect("digit").as_ref(), b"3");

        // The cursor has not redrawn yet: no Enter, the pass retries.
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(rx.try_recv().is_err(), "Enter must wait for the cursor");

        // Cursor on option 3: Enter confirms it.
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_3));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(rx.try_recv().expect("enter").as_ref(), b"\r");
        assert!(app.deferred_pane_writes.is_empty());
    }

    #[tokio::test]
    async fn preview_answer_sends_no_enter_after_prompt_moves_on() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvmoved", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = finish_three_option_preview_scan(&mut app, pane_id, &mut rx);
        let decision_id = pending_decisions()[0].decision_id.clone();

        answer(&mut app, &decision_id, Some("2"), None);
        assert_eq!(rx.try_recv().expect("digit").as_ref(), b"2");

        // A different prompt is on screen: the confirm pass ends silently.
        rx = replace_screen(&mut app, pane_id, PERM_LS);
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(rx.try_recv().is_err(), "no Enter into a different prompt");
        assert!(app.deferred_pane_writes.is_empty());
    }

    #[tokio::test]
    async fn option_with_text_types_both_into_the_free_text_slot() {
        // The dialog sends the chosen option together with typed text; a
        // plain question has no notes field, so both go through the
        // free-text slot instead of the text being dropped.
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("opttext", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("1"), Some("理由も読んで"));
        assert_eq!(rx.try_recv().expect("slot").as_ref(), b"3");
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert_eq!(
            rx.try_recv().expect("text").as_ref(),
            "「りんご」を選択。理由も読んで".as_bytes()
        );
        assert_eq!(rx.try_recv().expect("enter").as_ref(), b"\r");
    }

    #[tokio::test]
    async fn preview_option_with_text_adds_a_note_before_enter() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvnote", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = finish_three_option_preview_scan(&mut app, pane_id, &mut rx);
        let decision_id = pending_decisions()[0].decision_id.clone();
        // Preview questions offer no free-text slot, so the API refuses text
        // on them; deliver directly to exercise the notes path.
        let decision = all_decisions()
            .into_iter()
            .find(|decision| decision.decision_id == decision_id)
            .expect("decision");
        let note = crate::api::schema::DecisionAnswer {
            option_id: Some("2".to_string()),
            option_ids: None,
            text: Some("メモです".to_string()),
            responder: None,
            answered_unix_ms: 0,
        };
        app.deliver_screen_prompt_answer(0, pane_id, &decision, Some(&note));
        assert_eq!(rx.try_recv().expect("digit").as_ref(), b"2");
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_2));
        now += Duration::from_secs(5);
        app.flush_deferred_pane_writes(now);
        assert_eq!(rx.try_recv().expect("n").as_ref(), b"n");
        assert_eq!(rx.try_recv().expect("note").as_ref(), "メモです".as_bytes());
        assert_eq!(rx.try_recv().expect("enter").as_ref(), b"\r");
    }

    #[tokio::test]
    async fn plain_question_answer_sends_only_the_digit() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, _pane_id, mut rx) = test_app("plaindigit", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, Some("2"), None);
        assert_eq!(rx.try_recv().expect("digit").as_ref(), b"2");
        assert!(app.deferred_pane_writes.is_empty());
    }

    #[tokio::test]
    async fn preview_scan_abort_marks_unvisited_options_unavailable() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvabort", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        assert_eq!(app.preview_scans.len(), 1);

        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');

        // The prompt vanishes mid-walk: mismatched reads are tolerated for a
        // couple of torn frames, then the scan aborts and creates the
        // decision with the previews read so far.
        replace_screen(&mut app, pane_id, &crlf(IDLE_SCREEN));
        for _ in 0..PREVIEW_SCAN_MISMATCH_LIMIT {
            now += Duration::from_secs(1);
            app.flush_deferred_pane_writes(now);
        }
        assert!(app.preview_scans.is_empty());

        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        let options = &decisions[0].options;
        assert!(options[0].preview.is_some());
        assert!(options[1].preview.is_none());
        assert!(options[1].preview_unavailable);
        assert!(options[2].preview_unavailable);

        // The same signature is never scanned again.
        assert!(app.preview_scan_done.contains_key(&pane_id));
    }

    // ── Operator input versus screen-prompt keys ───────────────────────

    /// Every operator/remote/API input path marks the pane runtime before
    /// pushing bytes; tests go through the same marker.
    fn note_operator_input(app: &App, pane_id: PaneId) {
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("pane runtime")
            .note_user_input();
    }

    fn note_operator_input_ago(app: &App, pane_id: PaneId, ago: Duration) {
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("pane runtime")
            .note_user_input_at(Instant::now() - ago);
    }

    // Two-question AskUserQuestion flow (tab row `☐/☒`, `✔ Submit` marker),
    // modelled on the verbatim captures in src/screen_prompt.rs tests.
    const MQ_Q1: &str = "\
────────────────────────────────────────────────────────────
←  ☐ 果物  ☐ 動物  ✔ Submit  →

複数質問の確認（1つ目）。「りんご」を選んでください

❯ 1. りんご
     1つ目の答え
  2. みかん
     選ばない
  3. Type something.

Enter to select · Tab/Arrow keys to navigate · Esc to cancel
";

    const MQ_Q2: &str = "\
────────────────────────────────────────────────────────────
←  ☒ 果物  ☐ 動物  ✔ Submit  →

│ 複数質問の確認（2つ目）。「いぬ」を選んでください

❯ 1. ねこ
     選ばない
  2. いぬ
     2つ目の答え
  3. Type something.

Enter to select · Tab/Arrow keys to navigate · Esc to cancel
";

    const MQ_SUBMIT: &str = "\
────────────────────────────────────────────────────────────
←  ☒ 果物  ☒ 動物  ✔ Submit  →

Review your answers

 ● 複数質問の確認（1つ目）。「りんご」を選んでください
   → りんご
 ● 複数質問の確認（2つ目）。「いぬ」を選んでください
   → いぬ

Ready to submit your answers?

❯ 1. Submit answers
  2. Cancel
";

    /// Scan until no watch sits on `pane_id` (a replaced screen needs the
    /// two tolerated mismatches before the old watch releases).
    fn rescan_until_adopted(app: &mut App) {
        for _ in 0..4 {
            app.scan_screen_prompts(Instant::now());
        }
    }

    #[tokio::test]
    async fn preview_scan_never_starts_while_operator_types() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvquiet", &crlf(CC_PREVIEW_OPTION_1), true);
        note_operator_input(&app, pane_id);
        app.scan_screen_prompts(Instant::now());

        // Operator input freezes the pane: no walk starts, no keys leave
        // herdr, and no decision is created from a mid-edit frame.
        assert!(app.preview_scans.is_empty());
        assert!(rx.try_recv().is_err(), "no keys while operator types");
        assert!(pending_decisions().is_empty());

        // Once the input settles the decision is created — but the
        // question seen under operator input stays walked, so the quiet
        // window ending must not retroactively arm a walk, not even when
        // the prompt redraws under a new signature.
        note_operator_input_ago(&app, pane_id, PANE_USER_INPUT_QUIET * 3);
        let mut rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_1_WOBBLE));
        rescan_until_adopted(&mut app);
        assert!(app.preview_scans.is_empty());
        assert!(rx.try_recv().is_err());
        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].options[0].preview.is_some());
        assert!(decisions[0].options[1].preview_unavailable);
        assert!(decisions[0].options[2].preview_unavailable);
    }

    /// A CC_QUESTION frame whose free-text slot holds a typed draft —
    /// the layout the churn report produced per keystroke.
    fn cc_question_typing(draft: &str) -> String {
        CC_QUESTION
            .replacen("❯ 1. りんご", "  1. りんご", 1)
            .replacen("  3. Type something.", &format!("❯ 3. {draft}"), 1)
    }

    #[tokio::test]
    async fn typing_in_free_text_slot_does_not_churn_the_decision() {
        // The incident: each typed character redrew the slot as a new
        // option, the signature flipped, and the pending decision was
        // cancelled and recreated roughly once a second.
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("typedraft", &crlf(CC_QUESTION), true);
        rescan_until_adopted(&mut app);
        let first = pending_decisions()[0].decision_id.clone();

        // While the operator types into the slot the watch freezes —
        // and even after the input settles the draft is still the same
        // free-text question, so nothing rebuilds.
        for draft in ["そ", "それは", "それは君が判断できるでしょ"] {
            let _rx = replace_screen(&mut app, pane_id, &crlf(&cc_question_typing(draft)));
            note_operator_input(&app, pane_id);
            for _ in 0..3 {
                app.scan_screen_prompts(Instant::now());
            }
            let decisions = all_decisions();
            assert_eq!(decisions.len(), 1, "no churn while typing {draft:?}");
            assert_eq!(decisions[0].decision_id, first);
            assert_eq!(decisions[0].status, DecisionStatus::Pending);
        }

        note_operator_input_ago(&app, pane_id, PANE_USER_INPUT_QUIET * 2);
        rescan_until_adopted(&mut app);
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decision_id, first);
        assert_eq!(decisions[0].status, DecisionStatus::Pending);
        assert!(decisions[0].allow_text);
        assert!(!decisions[0]
            .options
            .iter()
            .any(|option| option.label.contains("それは")));
    }

    #[tokio::test]
    async fn operator_input_freezes_watch_until_input_settles() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("freeze", &crlf(CC_QUESTION), true);
        rescan_until_adopted(&mut app);
        let first = pending_decisions()[0].decision_id.clone();

        // While the operator types, reads that do not parse — a torn
        // frame, the prompt scrolling past — cannot cancel or recreate
        // the decision.
        let _rx = replace_screen(&mut app, pane_id, IDLE_SCREEN);
        note_operator_input(&app, pane_id);
        for _ in 0..6 {
            app.scan_screen_prompts(Instant::now());
        }
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decision_id, first);
        assert_eq!(decisions[0].status, DecisionStatus::Pending);
        assert!(app.screen_prompt_watches.contains_key(&pane_id));

        // The prompt is truly gone and the input has settled: the usual
        // two-mismatch cancel applies, exactly once.
        note_operator_input_ago(&app, pane_id, PANE_USER_INPUT_QUIET * 2);
        rescan_until_adopted(&mut app);
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].status, DecisionStatus::Cancelled);
        assert!(!app.screen_prompt_watches.contains_key(&pane_id));
    }

    #[tokio::test]
    async fn new_or_changed_prompt_waits_for_input_to_settle() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("settle", &crlf(CC_QUESTION), true);
        rescan_until_adopted(&mut app);
        assert_eq!(pending_decisions().len(), 1);

        // The agent advances to a different prompt while the operator is
        // still typing: nothing cancels, nothing is created mid-edit.
        let _rx = replace_screen(&mut app, pane_id, &crlf(CC_PERMISSION));
        note_operator_input(&app, pane_id);
        for _ in 0..6 {
            app.scan_screen_prompts(Instant::now());
        }
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].status, DecisionStatus::Pending);

        // Once the input settles the change is reflected exactly once:
        // the old decision is cancelled and the new prompt gets one.
        note_operator_input_ago(&app, pane_id, PANE_USER_INPUT_QUIET * 2);
        rescan_until_adopted(&mut app);
        let decisions = all_decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(
            decisions
                .iter()
                .filter(|decision| decision.status == DecisionStatus::Cancelled)
                .count(),
            1
        );
        let pending = pending_decisions();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, DecisionKind::ToolPermission);
    }

    #[tokio::test]
    async fn preview_scan_aborts_when_operator_types_mid_walk() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvabort-in", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = Instant::now();
        app.scan_screen_prompts(now);
        assert_eq!(app.preview_scans.len(), 1);

        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');

        // The operator starts typing mid-walk: stop right there. No more
        // arrows — including none of the restore arrows a normal finish
        // would send.
        note_operator_input(&app, pane_id);
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(app.preview_scans.is_empty());
        assert!(rx.try_recv().is_err(), "no restore arrows after abort");

        let decisions = pending_decisions();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].options[0].preview.is_some());
        assert!(decisions[0].options[1].preview_unavailable);
    }

    #[tokio::test]
    async fn signature_wobble_does_not_restart_preview_scan() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("pvwobble", &crlf(CC_PREVIEW_OPTION_1), true);
        let mut now = Instant::now();
        app.scan_screen_prompts(now);

        // Drive the walk to completion (cursor moves are simulated by
        // swapping the fixture the "pane" shows).
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_2));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'B');
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_3));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'A');
        rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_2));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert_eq!(drain_arrow(&mut rx), b'A');
        let _rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_1));
        now += Duration::from_secs(1);
        app.flush_deferred_pane_writes(now);
        assert!(app.preview_scans.is_empty());
        assert_eq!(pending_decisions().len(), 1);

        // The same question redraws differently (a description line
        // appears while the operator types): signature changes, question
        // key stays — the walk must not run a second time.
        let mut rx = replace_screen(&mut app, pane_id, &crlf(CC_PREVIEW_OPTION_1_WOBBLE));
        rescan_until_adopted(&mut app);
        assert!(app.preview_scans.is_empty(), "no re-scan for same question");
        assert!(rx.try_recv().is_err(), "no arrows after wobble");
    }

    // Same question/options as CC_PREVIEW_OPTION_1 but a description line
    // appeared under option 1 — the prompt signature differs.
    const CC_PREVIEW_OPTION_1_WOBBLE: &str = "\
←  ☐ 方式  ✔ Submit  →

どの方式にする？

❯ 1. 方式A
     補足説明が増えた
  2. 方式B                        ┌──────────────────────────────────────────┐
  3. 方式C                        │ 図A: input → A → out                     │
                                  └──────────────────────────────────────────┘

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    #[tokio::test]
    async fn submit_confirmation_screen_is_silent_without_herdr_answers() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("submit-idle", MQ_SUBMIT, true);
        rescan_until_adopted(&mut app);

        // A confirmation-only screen never becomes a decision, never
        // starts a preview walk, and never receives keys.
        assert!(all_decisions().is_empty());
        assert!(app.preview_scans.is_empty());
        assert!(!app.screen_prompt_watches.contains_key(&pane_id));
        assert!(rx.try_recv().is_err(), "no keys on confirmation screen");
    }

    #[tokio::test]
    async fn herdr_answered_flow_auto_submits_final_confirmation() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("autosubmit", MQ_Q1, true);
        rescan_until_adopted(&mut app);
        assert_eq!(pending_decisions().len(), 1);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("1"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q1 answer").as_ref(), b"1");

        rx = replace_screen(&mut app, pane_id, MQ_Q2);
        rescan_until_adopted(&mut app);
        assert_eq!(pending_decisions().len(), 1);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("2"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q2 answer").as_ref(), b"2");

        // Both questions answered through herdr: the confirmation screen
        // gets exactly one key — Enter on the already-focused Submit row.
        rx = replace_screen(&mut app, pane_id, MQ_SUBMIT);
        rescan_until_adopted(&mut app);
        assert!(pending_decisions().is_empty());
        assert!(app.preview_scans.is_empty());
        assert_eq!(
            rx.try_recv().expect("auto submit").as_ref(),
            b"\r",
            "focused Submit answers gets a bare Enter"
        );
        assert!(rx.try_recv().is_err(), "no extra keys after submit");

        // It fires once: rescanning the unchanged confirmation is inert.
        rescan_until_adopted(&mut app);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn partially_answered_flow_does_not_auto_submit() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("submit-part", MQ_Q1, true);
        rescan_until_adopted(&mut app);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("1"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q1 answer").as_ref(), b"1");

        // Question 2 was answered by the operator in the terminal — the
        // flow is incomplete, so the confirmation stays untouched.
        let mut rx = replace_screen(&mut app, pane_id, MQ_SUBMIT);
        rescan_until_adopted(&mut app);
        assert!(pending_decisions().is_empty());
        assert!(rx.try_recv().is_err(), "no submit on incomplete flow");
    }

    #[tokio::test]
    async fn operator_takeover_blocks_auto_submit() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("submit-block", MQ_Q1, true);
        rescan_until_adopted(&mut app);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("1"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q1 answer").as_ref(), b"1");

        rx = replace_screen(&mut app, pane_id, MQ_Q2);
        rescan_until_adopted(&mut app);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("2"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q2 answer").as_ref(), b"2");

        // The flow is complete but the operator typed into the pane
        // afterwards — their keys win, herdr's Enter must not land.
        rx = replace_screen(&mut app, pane_id, MQ_SUBMIT);
        note_operator_input(&app, pane_id);
        rescan_until_adopted(&mut app);
        assert!(rx.try_recv().is_err(), "no submit while operator types");
    }

    #[tokio::test]
    async fn failed_auto_submit_is_not_marked_done() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("autosubmit-fail", MQ_Q1, true);
        rescan_until_adopted(&mut app);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("1"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q1 answer").as_ref(), b"1");

        rx = replace_screen(&mut app, pane_id, MQ_Q2);
        rescan_until_adopted(&mut app);
        answer(
            &mut app,
            &pending_decisions()[0].decision_id.clone(),
            Some("2"),
            None,
        );
        assert_eq!(rx.try_recv().expect("q2 answer").as_ref(), b"2");

        // The pane's writer is gone: the submit send fails, gets logged,
        // and the confirmation is not marked submitted (a later scan may
        // retry once the writer recovers).
        let rx = replace_screen(&mut app, pane_id, MQ_SUBMIT);
        drop(rx);
        rescan_until_adopted(&mut app);
        assert!(
            !app.screen_prompt_auto_submits.contains_key(&pane_id),
            "failed submit is not recorded as sent"
        );
    }

    #[tokio::test]
    async fn answer_is_refused_while_operator_is_typing() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("gate", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();

        note_operator_input(&app, pane_id);
        let response = answer_response(&mut app, &decision_id, Some("1"), None);
        assert!(response.contains("pane_user_active"), "{response}");
        assert_eq!(pending_decisions().len(), 1, "answer stays pending");
        assert!(rx.try_recv().is_err(), "no keys while operator types");
    }

    #[tokio::test]
    async fn deferred_answer_text_drops_when_operator_types() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, mut rx) = test_app("drop-deferred", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let decision_id = pending_decisions()[0].decision_id.clone();
        answer(&mut app, &decision_id, None, Some("ぶどう"));
        assert_eq!(rx.try_recv().expect("slot").as_ref(), b"3");

        // The operator grabbed the pane before the deferred text/Enter
        // flushed — herdr must not spray the queued bytes in.
        note_operator_input(&app, pane_id);
        app.flush_deferred_pane_writes(Instant::now() + Duration::from_secs(60));
        assert!(rx.try_recv().is_err(), "deferred bytes dropped");
    }

    // Real Claude Code 2.1.296 AFK permission dialog (auto mode, dangerous
    // `rm` on a variable path) — the `in M:SS` countdown ticks once per
    // second and used to recreate the decision on every tick.
    fn afk_permission(countdown: &str) -> String {
        format!(
            "\
⏺ Bash(B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done)
  ⎿  Waiting…

──────────────────────────────────────────────────────
 Bash command
 Delete frames directories for two bench runs
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Dangerous rm operation on possibly-empty variable path: $B/$r/frames in `/bin/rm -rf $B/$r/frames` (rewrite it as \"${{B:?}}\"/\"${{r:?}}\"/frames or use a literal path)
 ⚠ Claude Code will automatically deny this request
 in {countdown}, to avoid blocking progress on an unattended
 session


 Do you want to proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel · Tab to amend
"
        )
    }

    #[tokio::test]
    async fn afk_countdown_keeps_single_pending_decision() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("afk", &afk_permission("1:48"), true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions();
        assert_eq!(first.len(), 1);
        let decision_id = first[0].decision_id.clone();
        // The deadline survives in body, digit-free; the title carries the
        // full command, its description, and the warning.
        assert!(first[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("automatically deny this request in M:SS")));
        assert!(first[0]
            .title
            .contains("B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C"));
        assert!(first[0].title.contains("/bin/rm -rf $B/$r/frames; done"));
        assert!(first[0].title.contains("Dangerous rm operation"));
        assert!(!first[0].title.contains("automatically deny"));
        assert!(!first[0].title.contains('╌'));

        // Every redraw of the same prompt (countdown ticking) keeps the same
        // pending decision — no cancel, no recreate.
        for countdown in ["1:12", "0:30", "0:03"] {
            replace_screen(&mut app, pane_id, &afk_permission(countdown));
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
        }
        let all = all_decisions();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].decision_id, decision_id);
        assert_eq!(all[0].status, DecisionStatus::Pending);
        assert!(app.screen_prompt_watches.contains_key(&pane_id));
    }

    #[tokio::test]
    async fn watch_loss_adopts_pending_decision_without_recreate() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("adopt", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        let first = pending_decisions()[0].clone();

        // Watch lost (e.g., server restart): the still-pending decision for
        // the same prompt is adopted — same id, still pending, no cancel.
        app.screen_prompt_watches.remove(&pane_id);
        app.scan_screen_prompts(Instant::now());

        let all = all_decisions();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].decision_id, first.decision_id);
        assert_eq!(all[0].status, DecisionStatus::Pending);
        assert!(app.screen_prompt_watches.contains_key(&pane_id));
    }

    #[tokio::test]
    async fn rapid_rebuilds_trip_flood_brake_into_in_place_updates() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("flood", PERM_LS, true);
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);

        // Different questions on identical options normally cancel+create —
        // keep bouncing between them until the flood brake engages.
        for _ in 0..SCREEN_PROMPT_FLOOD_LIMIT {
            replace_screen(&mut app, pane_id, PERM_RM);
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
            replace_screen(&mut app, pane_id, PERM_LS);
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
            app.scan_screen_prompts(Instant::now());
        }
        let before = all_decisions().len();
        let pending_id = pending_decisions()[0].decision_id.clone();

        // Once churning, the same-options rebuild updates the record instead
        // of cancelling it — one decision, one watcher notification.
        replace_screen(&mut app, pane_id, PERM_RM);
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());
        app.scan_screen_prompts(Instant::now());
        let all = all_decisions();
        assert_eq!(all.len(), before);
        assert_eq!(pending_decisions().len(), 1);
        assert_eq!(pending_decisions()[0].decision_id, pending_id);
        assert!(pending_decisions()[0].title.contains("rm -rf"));
    }
}
