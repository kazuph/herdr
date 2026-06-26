use std::collections::HashMap;
use std::path::PathBuf;

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

const CLAUDE_WORKING_HOLD: std::time::Duration = std::time::Duration::from_millis(1200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub custom_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub custom_status: Option<String>,
}

/// Pure state for a server-owned terminal.
///
/// During the migration this is still one-to-one with a pane-backed PTY, but
/// pane/view state no longer owns terminal identity, cwd, labels, or agent
/// metadata.
pub struct TerminalState {
    pub id: TerminalId,
    pub cwd: PathBuf,
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    pub hook_authority: Option<HookAuthority>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    pub agent_task_title: Option<String>,
    pub pane_title: Option<String>,
    hook_report_sequences: HashMap<String, u64>,
    pub state: AgentState,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    /// Resumable session id reported by an agent integration hook.
    pub agent_session_id: Option<String>,
    /// Agent that owned the observed session id, kept after live detection clears.
    pub agent_session_agent: Option<Agent>,
    /// Agent metadata restored from the session snapshot, consumed by
    /// `[agent_restore]` to relaunch the agent in this pane.
    pub pending_restore: Option<PendingAgentRestore>,
}

/// What ran in a pane before the server restarted, as captured in the
/// session snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAgentRestore {
    pub agent: String,
    pub session_id: Option<String>,
}

impl TerminalState {
    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            hook_authority: None,
            manual_label: None,
            agent_name: None,
            agent_task_title: None,
            pane_title: None,
            hook_report_sequences: HashMap::new(),
            state: AgentState::Unknown,
            revision: 0,
            launch_argv: None,
            agent_session_id: None,
            agent_session_agent: None,
            pending_restore: None,
        }
    }

    pub fn with_launch_argv(mut self, argv: Vec<String>) -> Self {
        self.launch_argv = Some(argv);
        self
    }

    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_detected_agent = self.detected_agent;
        self.detected_agent = agent;
        self.fallback_state = fallback_state;
        if self.hook_authority_conflicts_with_detected_agent(agent)
            || (previous_detected_agent.is_some()
                && agent != previous_detected_agent
                && self.hook_authority.as_ref().is_some_and(|authority| {
                    crate::detect::parse_agent_label(&authority.agent_label)
                        == previous_detected_agent
                }))
        {
            self.hook_authority = None;
        }
        self.recompute_effective_state(previous_agent_label, previous_known_agent, previous_state)
    }

    #[cfg(test)]
    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_with_custom_status(source, agent_label, state, message, None, seq)
    }

    pub fn set_hook_authority_with_custom_status(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        if !self.accept_hook_report(&source, seq) {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            custom_status,
        });
        self.recompute_effective_state(previous_agent_label, previous_known_agent, previous_state)
    }

    fn hook_authority_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn known_agent_label_conflicts_with_detected_agent(&self, agent_label: &str) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label)
            .is_some_and(|hook_agent| hook_agent != detected_agent)
    }

    fn accept_hook_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        let Some(seq) = seq else {
            return !self.hook_report_sequences.contains_key(source);
        };

        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|last_seq| seq <= *last_seq)
        {
            return false;
        }

        self.hook_report_sequences.insert(source.to_string(), seq);
        true
    }

    pub fn clear_hook_authority(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        let sequence_source = source.map(str::to_string).or_else(|| {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.source.clone())
        });
        if let Some(source) = sequence_source.as_deref() {
            if !self.accept_hook_report(source, seq) {
                return None;
            }
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let should_clear = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        if !should_clear {
            return None;
        }
        self.hook_authority = None;
        self.recompute_effective_state(previous_agent_label, previous_known_agent, previous_state)
    }

    pub fn release_agent(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        if !self.accept_hook_report(source, seq) {
            return None;
        }

        let current_agent_label = self.effective_agent_label()?;
        if current_agent_label != agent_label {
            return None;
        }

        if self.hook_authority.as_ref().is_some_and(|authority| {
            authority.agent_label != agent_label || authority.source != source
        }) {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.hook_authority = None;
        self.recompute_effective_state(previous_agent_label, previous_known_agent, previous_state)
    }

    pub fn effective_agent_label(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .map(|authority| authority.agent_label.as_str())
            .or_else(|| self.detected_agent.map(crate::detect::agent_label))
    }

    pub fn effective_known_agent(&self) -> Option<Agent> {
        if let Some(authority) = &self.hook_authority {
            return crate::detect::parse_agent_label(&authority.agent_label);
        }
        self.detected_agent
    }

    pub fn effective_custom_status(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .and_then(|authority| authority.custom_status.as_deref())
    }

    pub fn set_manual_label(&mut self, label: String) {
        let label = label.trim().to_string();
        self.manual_label = (!label.is_empty()).then_some(label);
    }

    pub fn clear_manual_label(&mut self) {
        self.manual_label = None;
    }

    pub fn set_agent_name(&mut self, name: String) {
        let name = name.trim().to_string();
        self.agent_name = (!name.is_empty()).then_some(name);
    }

    pub fn clear_agent_name(&mut self) {
        self.agent_name = None;
    }

    pub fn set_agent_task_title(&mut self, title: Option<String>) -> bool {
        let title = title.and_then(|title| {
            let title = title.trim().to_string();
            (!title.is_empty()).then_some(title)
        });
        if self.agent_task_title == title {
            return false;
        }
        self.agent_task_title = title;
        true
    }

    pub fn set_pane_title(&mut self, title: Option<String>) -> bool {
        let title = title.and_then(|title| {
            let title = title.trim().to_string();
            (!title.is_empty()).then_some(title)
        });
        if self.pane_title == title {
            return false;
        }
        self.pane_title = title;
        true
    }

    pub fn is_agent_terminal(&self) -> bool {
        self.agent_name.is_some()
            || self.agent_task_title.is_some()
            || self.effective_agent_label().is_some()
    }

    fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        let state = self
            .hook_authority
            .as_ref()
            .map(|authority| authority.state)
            .unwrap_or(self.fallback_state);
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();

        let custom_status = self.effective_custom_status().map(str::to_string);

        if previous_agent_label == agent_label && previous_state == state {
            return None;
        }

        self.state = state;
        Some(EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            agent_label,
            known_agent,
            state,
            custom_status,
        })
    }
}

/// How long an agent activity fingerprint may stay frozen before the
/// `working` evidence is treated as fossilized scrollback rather than a live
/// spinner. A live spinner animates its frame glyph and elapsed-time counter
/// every second, so a fingerprint that does not change for this long cannot
/// be a running turn.
pub(crate) const CLAUDE_ACTIVITY_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(15);

/// Downgrade `working` to `idle` when the only working evidence is an agent
/// activity fingerprint that has been frozen for `CLAUDE_ACTIVITY_STALE_AFTER`.
///
/// Claude Code and Codex sometimes leave old spinner/status lines in
/// the transcript right above the prompt box, which is indistinguishable
/// from a live spinner by position or content alone. Tracking change over
/// time is the discriminator: real spinners tick, fossils do not. The state
/// self-heals — any new turn redraws the spinner, changes the fingerprint,
/// and working detection resumes immediately.
pub(crate) fn filter_stale_claude_working(
    agent: Option<Agent>,
    raw: AgentState,
    fingerprint: Option<String>,
    now: std::time::Instant,
    tracker: &mut Option<(String, std::time::Instant)>,
) -> AgentState {
    if !matches!(agent, Some(Agent::Claude | Agent::Codex)) || raw != AgentState::Working {
        *tracker = None;
        return raw;
    }
    let Some(fingerprint) = fingerprint else {
        // Working for a non-fingerprintable reason; nothing to expire.
        *tracker = None;
        return raw;
    };
    match tracker {
        Some((last, frozen_since)) if *last == fingerprint => {
            if now.duration_since(*frozen_since) >= CLAUDE_ACTIVITY_STALE_AFTER {
                AgentState::Idle
            } else {
                raw
            }
        }
        _ => {
            *tracker = Some((fingerprint, now));
            raw
        }
    }
}

pub(crate) fn stabilize_agent_state(
    agent: Option<Agent>,
    previous: AgentState,
    raw: AgentState,
    now: std::time::Instant,
    last_claude_working_at: &mut Option<std::time::Instant>,
) -> AgentState {
    if agent != Some(Agent::Claude) {
        return raw;
    }

    match raw {
        AgentState::Working => {
            *last_claude_working_at = Some(now);
            AgentState::Working
        }
        AgentState::Blocked => AgentState::Blocked,
        AgentState::Idle if previous == AgentState::Working => {
            if last_claude_working_at
                .is_some_and(|last_working| now.duration_since(last_working) < CLAUDE_WORKING_HOLD)
            {
                AgentState::Working
            } else {
                AgentState::Idle
            }
        }
        _ => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    #[test]
    fn frozen_claude_activity_fingerprint_expires_to_idle() {
        let now = std::time::Instant::now();
        let mut tracker = None;
        let fossil = "✢ herdrで並行実装を監督… (13m 47s · thinking)".to_string();

        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fossil.clone()),
                now,
                &mut tracker,
            ),
            AgentState::Working,
            "first observation is trusted"
        );
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fossil.clone()),
                now + std::time::Duration::from_secs(5),
                &mut tracker,
            ),
            AgentState::Working,
            "still within the staleness window"
        );
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fossil.clone()),
                now + CLAUDE_ACTIVITY_STALE_AFTER,
                &mut tracker,
            ),
            AgentState::Idle,
            "a fingerprint frozen past the window is fossilized scrollback"
        );
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fossil),
                now + CLAUDE_ACTIVITY_STALE_AFTER + std::time::Duration::from_secs(60),
                &mut tracker,
            ),
            AgentState::Idle,
            "stays idle while the fossil remains unchanged"
        );
    }

    #[test]
    fn ticking_claude_activity_fingerprint_stays_working() {
        let now = std::time::Instant::now();
        let mut tracker = None;
        for second in 0..60u64 {
            // A live spinner updates its frame glyph and timer every second.
            let frame = if second % 2 == 0 { "✻" } else { "✽" };
            let fingerprint = format!("{frame} Cogitating… ({second}s · ↓ 1.2k tokens)");
            assert_eq!(
                filter_stale_claude_working(
                    Some(Agent::Claude),
                    AgentState::Working,
                    Some(fingerprint),
                    now + std::time::Duration::from_secs(second),
                    &mut tracker,
                ),
                AgentState::Working,
                "live spinner at {second}s must stay working"
            );
        }
    }

    #[test]
    fn new_turn_after_fossil_resumes_working_immediately() {
        let now = std::time::Instant::now();
        let mut tracker = None;
        let fossil = "✢ old task… (13m 47s · thinking)".to_string();

        filter_stale_claude_working(
            Some(Agent::Claude),
            AgentState::Working,
            Some(fossil.clone()),
            now,
            &mut tracker,
        );
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fossil.clone()),
                now + CLAUDE_ACTIVITY_STALE_AFTER,
                &mut tracker,
            ),
            AgentState::Idle,
        );

        // A new turn adds a fresh spinner below the fossil: fingerprint changes.
        let fresh = format!("{fossil}\n✻ Sprouting… (1s · ↑ 0.1k tokens)");
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Working,
                Some(fresh),
                now + CLAUDE_ACTIVITY_STALE_AFTER + std::time::Duration::from_secs(1),
                &mut tracker,
            ),
            AgentState::Working,
            "changed fingerprint resumes working immediately"
        );
    }

    #[test]
    fn stale_filter_resets_tracker_outside_claude_working() {
        let now = std::time::Instant::now();
        let mut tracker = None;
        let fossil = "✢ task… (1m 0s)".to_string();

        filter_stale_claude_working(
            Some(Agent::Claude),
            AgentState::Working,
            Some(fossil.clone()),
            now,
            &mut tracker,
        );
        // An idle observation in between resets the freeze clock.
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Claude),
                AgentState::Idle,
                None,
                now + std::time::Duration::from_secs(5),
                &mut tracker,
            ),
            AgentState::Idle,
        );
        assert!(tracker.is_none());
        // Agents without fossil filters pass through untouched.
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Gemini),
                AgentState::Working,
                Some(fossil),
                now + std::time::Duration::from_secs(6),
                &mut tracker,
            ),
            AgentState::Working,
        );
        assert!(tracker.is_none());
    }

    #[test]
    fn frozen_codex_working_header_expires_to_idle() {
        let now = std::time::Instant::now();
        let mut tracker = None;
        let fossil = "• Working (37s • esc to interrupt)".to_string();

        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Codex),
                AgentState::Working,
                Some(fossil.clone()),
                now,
                &mut tracker,
            ),
            AgentState::Working,
            "first Codex working observation is trusted"
        );
        assert_eq!(
            filter_stale_claude_working(
                Some(Agent::Codex),
                AgentState::Working,
                Some(fossil),
                now + CLAUDE_ACTIVITY_STALE_AFTER,
                &mut tracker,
            ),
            AgentState::Idle,
            "a frozen Codex working header is fossilized scrollback"
        );
    }

    #[test]
    fn claude_working_is_sticky_for_short_gap() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Idle,
            AgentState::Working,
            now,
            &mut last_working,
        );
        assert_eq!(working, AgentState::Working);

        let still_working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + std::time::Duration::from_millis(400),
            &mut last_working,
        );
        assert_eq!(still_working, AgentState::Working);
    }

    #[test]
    fn claude_transitions_to_idle_after_hold_expires() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + CLAUDE_WORKING_HOLD + std::time::Duration::from_millis(1),
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn non_claude_states_are_unchanged() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let state = stabilize_agent_state(
            Some(Agent::Codex),
            AgentState::Working,
            AgentState::Idle,
            now,
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_can_override_with_unknown_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn known_hook_authority_does_not_override_different_detected_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);
        let change = terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        assert!(change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clears_conflicting_known_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_survives_unrelated_detected_agent_clear() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clear_clears_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn detected_agent_change_clears_previous_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::OpenCode));
        assert_eq!(terminal.effective_agent_label(), Some("opencode"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn release_agent_clears_identity_immediately() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.release_agent("herdr:pi", "pi", None);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn stale_hook_report_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn unsequenced_hook_report_is_ignored_after_source_uses_sequence() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_release_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.release_agent("herdr:pi", "pi", Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn stale_clear_all_sequence_is_checked_against_current_authority_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.clear_hook_authority(None, Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn same_sequence_from_different_sources_is_independent() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.set_hook_authority(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().source,
            "custom:pi"
        );
    }
}
