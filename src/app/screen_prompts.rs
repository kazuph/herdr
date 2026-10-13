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
use crate::screen_prompt::{parse_screen_prompt, OptionShortcut, ScreenPrompt};
use crate::terminal::TerminalRuntime;

/// Decision origin reason marking decisions created from pane screens.
pub(crate) const SCREEN_PROMPT_REASON: &str = "screen_prompt";
/// How often blocked panes are re-scanned while the feature is active.
const SCREEN_PROMPT_SCAN_INTERVAL: Duration = Duration::from_millis(400);
/// Consecutive reads that no longer show the watched prompt before the watch
/// is released and the pending decision cancelled.
const SCREEN_PROMPT_MISMATCH_LIMIT: u8 = 2;
/// Delay between typing the free-text option number and the answer text, and
/// again before Enter (CC redraws the option row when a number is typed).
const FREE_TEXT_OPTION_DELAY: Duration = Duration::from_millis(500);
const FREE_TEXT_ENTER_DELAY: Duration = Duration::from_millis(300);

/// A screen prompt a pane is currently showing, tracked against the decision
/// it created so redraws, tab moves and disappearances reconcile it.
#[derive(Debug, Clone)]
pub(crate) struct ScreenPromptWatch {
    pub signature: String,
    pub decision_id: String,
    /// Option number of the free-text slot when the prompt offered one; kept
    /// so a text answer still finds the slot when the live read is torn.
    pub free_text_slot: Option<u32>,
    /// The decision was resolved (answered, cancelled, expired); the watch
    /// stays only to observe the prompt leaving the screen so the same prompt
    /// is not re-offered.
    pub resolved: bool,
    /// Consecutive scans whose parse did not match `signature`.
    pub mismatches: u8,
}

/// Bytes to write to a pane once a deadline passes. Typed text and Enter for
/// free-text answers need delays so the agent TUI can redraw between keys.
#[derive(Debug, Clone)]
pub(crate) struct DeferredPaneWrite {
    pub at: Instant,
    pub ws_idx: usize,
    pub pane_id: PaneId,
    pub bytes: Bytes,
}

impl App {
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
            self.deferred_pane_writes.iter().map(|write| write.at).min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Deadline-driven work for the TUI and headless loops: flush due writes
    /// and run a scan when its scheduled instant passed.
    pub(crate) fn screen_prompt_tasks(&mut self, now: Instant) -> bool {
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
        match parse_screen_prompt(&runtime.detection_text_unwrapped()) {
            Some(prompt) => prompt.signature() == watch.signature,
            None => watch.mismatches == 0,
        }
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
                .partition(|write| write.at <= now);
        self.deferred_pane_writes = pending;
        for write in due {
            let Some(runtime) = self.lookup_runtime_sender(write.ws_idx, write.pane_id) else {
                warn!(
                    pane_id = ?write.pane_id,
                    "dropping deferred screen prompt write: pane gone"
                );
                continue;
            };
            if let Err(err) = runtime.try_send_bytes(write.bytes) {
                warn!(
                    pane_id = ?write.pane_id,
                    error = %err,
                    "deferred screen prompt write failed"
                );
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
                .map(TerminalRuntime::detection_text_unwrapped);
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

        if let Some(watch) = self.screen_prompt_watches.get_mut(&pane_id) {
            if current_sig.as_deref() == Some(watch.signature.as_str()) {
                watch.mismatches = 0;
                return false;
            }
            watch.mismatches += 1;
            if watch.mismatches < SCREEN_PROMPT_MISMATCH_LIMIT {
                return false;
            }
        }
        if self.screen_prompt_watches.contains_key(&pane_id) {
            // Two consecutive mismatched reads: the prompt is gone or changed.
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

    /// Reuse a still-pending decision for an identical prompt (server restart,
    /// watch lost on handoff) or create a fresh one.
    fn create_or_adopt_watch(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        prompt: &ScreenPrompt,
        sig: String,
    ) -> bool {
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
            })
            .collect();
        let allow_text = prompt.free_text.is_some();
        if options.is_empty() && !allow_text {
            return false;
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
        let body = prompt.progress().filter(|body| !body.is_empty());

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
                    .is_some_and(|origin| origin.pane_id == public_id)
        }) {
            if existing.kind == kind
                && existing.title == title
                && existing.body == body
                && existing.allow_text == allow_text
                && existing
                    .options
                    .iter()
                    .map(|option| {
                        (
                            option.id.clone(),
                            option.label.clone(),
                            option.description.clone(),
                        )
                    })
                    .eq(options.iter().map(|option| {
                        (
                            option.id.clone(),
                            option.label.clone(),
                            option.description.clone(),
                        )
                    }))
            {
                // Same prompt already has a pending decision — adopt it so a
                // restarted server does not re-offer.
                self.screen_prompt_watches.insert(
                    pane_id,
                    ScreenPromptWatch {
                        signature: sig,
                        decision_id: existing.decision_id.clone(),
                        free_text_slot: prompt.free_text,
                        resolved: false,
                        mismatches: 0,
                    },
                );
                return changed;
            }
            // A different prompt on this pane holds a pending decision; it is
            // stale, so cancel it before offering the new one.
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
            kind,
            title,
            body,
            options,
            allow_text,
            origin: Some(DecisionOrigin {
                pane_id: public_id,
                agent: pane.as_ref().and_then(|pane| pane.agent.clone()),
                cwd: pane.as_ref().and_then(|pane| pane.cwd.clone()),
                reason: Some(SCREEN_PROMPT_REASON.to_string()),
                machine: Some("local".to_string()),
                ..DecisionOrigin::default()
            }),
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
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            warn!(pane_id = ?pane_id, "screen prompt answer dropped: pane gone");
            return;
        };
        let screen = runtime.detection_text_unwrapped();
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

        // An explicit option wins over any accompanying text: free-text
        // answers arrive without an option id and take the typing path.
        let Some(number) = answer
            .and_then(|answer| answer.option_id.as_deref())
            .and_then(|option_id| option_id.parse::<u32>().ok())
        else {
            if let Some(text) = answer.and_then(|answer| answer.text.as_deref()) {
                self.deliver_free_text(ws_idx, pane_id, decision, prompt.as_ref(), text);
            } else {
                debug!(
                    pane_id = ?pane_id,
                    "screen prompt answer has no numeric option; nothing to send"
                );
            }
            return;
        };
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
        let bytes = match shortcut {
            Some(OptionShortcut::Escape) => {
                Bytes::from(runtime.encode_terminal_key(crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                )))
            }
            Some(OptionShortcut::Char(ch)) => Bytes::copy_from_slice(ch.to_string().as_bytes()),
            None => Bytes::copy_from_slice(number.to_string().as_bytes()),
        };
        if let Err(err) = runtime.try_send_bytes(bytes) {
            warn!(
                pane_id = ?pane_id,
                error = %err,
                "failed to send screen prompt answer"
            );
        }
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
                if let Err(err) =
                    runtime.try_send_bytes(Bytes::copy_from_slice(number.to_string().as_bytes()))
                {
                    warn!(
                        pane_id = ?pane_id,
                        error = %err,
                        "failed to select free-text option"
                    );
                    return;
                }
                let now = Instant::now();
                self.deferred_pane_writes.push(DeferredPaneWrite {
                    at: now + FREE_TEXT_OPTION_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: text_bytes,
                });
                self.deferred_pane_writes.push(DeferredPaneWrite {
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
                self.deferred_pane_writes.push(DeferredPaneWrite {
                    at: Instant::now() + FREE_TEXT_ENTER_DELAY,
                    ws_idx,
                    pane_id,
                    bytes: enter,
                });
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

fn option_role(label: &str) -> DecisionOptionRole {
    let lower = label.trim_start().to_lowercase();
    if lower.starts_with("yes")
        || lower.starts_with("approve")
        || lower.starts_with("allow")
        || lower.starts_with("proceed")
        || lower.starts_with("はい")
    {
        DecisionOptionRole::Approve
    } else if lower.starts_with("no")
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
    async fn prompt_disappearance_cancels_after_two_reads() {
        let _guard = crate::config::lock_test_config_env();
        let (mut app, pane_id, _rx) = test_app("gone", CC_QUESTION, true);
        app.scan_screen_prompts(Instant::now());
        assert_eq!(pending_decisions().len(), 1);

        // A single torn/blank read must not cancel.
        let _ = replace_screen(&mut app, pane_id, IDLE_SCREEN);
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
        let mut rx = replace_screen(&mut app, pane_id, IDLE_SCREEN);
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
        let mut rx = replace_screen(&mut app, pane_id, IDLE_SCREEN);
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
}
