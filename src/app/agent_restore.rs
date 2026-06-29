//! `[agent_restore]` — relaunch agent CLIs in restored panes.
//!
//! Panes restored from a session snapshot carry the agent that was running
//! when the snapshot was captured (`TerminalState::pending_restore`). This
//! module turns those into resume commands typed into the pane shells,
//! either automatically after startup (config `enabled = true`) or on demand
//! through `herdr agent restore [--dry-run]`.

use tracing::info;

use crate::api::schema::AgentRestoreActionInfo;
use crate::layout::PaneId;

pub(crate) struct AgentRestorePlanEntry {
    pub ws_idx: usize,
    pub pane_id: PaneId,
    pub agent: String,
    pub outcome: AgentRestoreOutcome,
}

pub(crate) enum AgentRestoreOutcome {
    Launch(String),
    Skip(&'static str),
}

fn running_agent_count(state: &crate::app::state::AppState) -> usize {
    state
        .terminals
        .values()
        .filter(|terminal| terminal.effective_agent_label().is_some())
        .count()
}

fn restore_toast_context(actions: &[AgentRestoreActionInfo], running_agents: usize) -> String {
    let launched = actions
        .iter()
        .filter(|action| action.status == "launched")
        .count();
    let would_launch = actions
        .iter()
        .filter(|action| action.status == "would_launch")
        .count();
    let skipped = actions
        .iter()
        .filter(|action| action.status == "skipped")
        .count();
    let total = actions.len();

    if total == 0 {
        if running_agents == 0 {
            "no pending restore".to_string()
        } else {
            format!("no pending restore, {running_agents} already running")
        }
    } else if would_launch > 0 {
        format!("would launch {would_launch}, skipped {skipped}")
    } else {
        format!("launched {launched}, skipped {skipped}")
    }
}

/// Decide what to do for every pane that had an agent before the restart.
///
/// Skips panes with live agent evidence (`detected_agent`), so a relaunch is
/// never typed into a pane where the user already brought the agent back.
pub(crate) fn agent_restore_plan(
    state: &crate::app::state::AppState,
) -> Vec<AgentRestorePlanEntry> {
    let mut entries = Vec::new();
    for (ws_idx, workspace) in state.workspaces.iter().enumerate() {
        for tab in &workspace.tabs {
            for (pane_id, pane) in &tab.panes {
                let Some(terminal) = state.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let Some(pending) = terminal.pending_restore.as_ref() else {
                    continue;
                };
                let agent = pending.agent.clone();
                let outcome = if terminal.detected_agent.is_some() {
                    AgentRestoreOutcome::Skip("agent already running")
                } else if let Some(template) = crate::agent_sessions::restore_template(
                    &state.agent_restore_config.commands,
                    &agent,
                ) {
                    let session_id = pending
                        .session_id
                        .clone()
                        .filter(|id| crate::agent_sessions::is_safe_session_id(id));
                    match crate::agent_sessions::render_restore_command(
                        template,
                        session_id.as_deref(),
                    ) {
                        Some(command) => AgentRestoreOutcome::Launch(command),
                        None => AgentRestoreOutcome::Skip("no resumable session found"),
                    }
                } else {
                    AgentRestoreOutcome::Skip("no restore command configured")
                };
                entries.push(AgentRestorePlanEntry {
                    ws_idx,
                    pane_id: *pane_id,
                    agent,
                    outcome,
                });
            }
        }
    }
    entries.sort_by_key(|entry| (entry.ws_idx, entry.pane_id.raw()));
    entries
}

impl crate::app::App {
    pub(crate) fn run_pending_agent_restore_request(&mut self) -> bool {
        if !self.state.request_agent_restore {
            return false;
        }
        self.state.request_agent_restore = false;
        let previous_toast = self.state.toast.clone();
        let actions = self.execute_agent_restore(false);
        let context = restore_toast_context(&actions, running_agent_count(&self.state));
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "agent restore".into(),
            context,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
        true
    }

    /// Execute (or, with `dry_run`, just report) the agent restore plan.
    pub(crate) fn execute_agent_restore(&mut self, dry_run: bool) -> Vec<AgentRestoreActionInfo> {
        let entries = agent_restore_plan(&self.state);
        let mut actions = Vec::with_capacity(entries.len());
        for entry in entries {
            let pane_id = self
                .public_pane_id(entry.ws_idx, entry.pane_id)
                .unwrap_or_else(|| format!("{}-{}", entry.ws_idx + 1, entry.pane_id.raw()));
            let action = match entry.outcome {
                AgentRestoreOutcome::Skip(reason) => {
                    if reason == "agent already running" {
                        self.clear_pending_restore(entry.ws_idx, entry.pane_id);
                    }
                    AgentRestoreActionInfo {
                        pane_id,
                        agent: entry.agent,
                        status: "skipped".into(),
                        command: None,
                        reason: Some(reason.into()),
                    }
                }
                AgentRestoreOutcome::Launch(command) if dry_run => AgentRestoreActionInfo {
                    pane_id,
                    agent: entry.agent,
                    status: "would_launch".into(),
                    command: Some(command),
                    reason: None,
                },
                AgentRestoreOutcome::Launch(command) => {
                    match self.type_command_into_pane(entry.ws_idx, entry.pane_id, &command) {
                        Ok(()) => {
                            self.clear_pending_restore(entry.ws_idx, entry.pane_id);
                            info!(
                                pane = %pane_id,
                                agent = %entry.agent,
                                command = %command,
                                "agent restore launched"
                            );
                            AgentRestoreActionInfo {
                                pane_id,
                                agent: entry.agent,
                                status: "launched".into(),
                                command: Some(command),
                                reason: None,
                            }
                        }
                        Err(reason) => AgentRestoreActionInfo {
                            pane_id,
                            agent: entry.agent,
                            status: "skipped".into(),
                            command: Some(command),
                            reason: Some(reason),
                        },
                    }
                }
            };
            actions.push(action);
        }
        actions
    }

    /// Run the scheduled startup restore once its delay elapses.
    pub(crate) fn maybe_run_scheduled_agent_restore(&mut self, now: std::time::Instant) -> bool {
        if self.agent_restore_due.is_none_or(|due| now < due) {
            return false;
        }
        self.agent_restore_due = None;
        let actions = self.execute_agent_restore(false);
        for action in &actions {
            info!(
                pane = %action.pane_id,
                agent = %action.agent,
                status = %action.status,
                reason = action.reason.as_deref().unwrap_or(""),
                "startup agent restore"
            );
        }
        actions.iter().any(|action| action.status == "launched")
    }

    fn clear_pending_restore(&mut self, ws_idx: usize, pane_id: PaneId) {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_restore = None;
        }
    }

    /// Type `command` + Enter into the pane shell, using the same
    /// bracketed-paste-aware encoding as `pane.send_input`.
    fn type_command_into_pane(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        command: &str,
    ) -> Result<(), String> {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err("pane runtime not found".into());
        };
        let text = super::api_helpers::encode_api_text(runtime, command);
        runtime
            .try_send_bytes(bytes::Bytes::from(text))
            .map_err(|err| err.to_string())?;
        std::thread::sleep(super::api::AGENT_SEND_SUBMIT_DELAY);
        let enter = super::api_helpers::encode_api_keys(runtime, &["enter".to_string()])
            .map_err(|key| format!("unsupported key {key}"))?;
        for bytes in enter {
            runtime
                .try_send_bytes(bytes::Bytes::from(bytes))
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::PendingAgentRestore;

    fn terminal_id_for(
        state: &crate::app::state::AppState,
        ws_idx: usize,
    ) -> crate::terminal::TerminalId {
        let ws = &state.workspaces[ws_idx];
        let pane_id = ws.tabs[0].root_pane;
        ws.tabs[0].panes[&pane_id].attached_terminal_id.clone()
    }

    #[test]
    fn plan_builds_commands_and_skips_live_or_unconfigured_agents() {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = vec![
            crate::workspace::Workspace::test_new("claude-ws"),
            crate::workspace::Workspace::test_new("codex-ws"),
            crate::workspace::Workspace::test_new("pi-ws"),
            crate::workspace::Workspace::test_new("lost-ws"),
        ];
        state.ensure_test_terminals();

        let claude_tid = terminal_id_for(&state, 0);
        let claude = state.terminals.get_mut(&claude_tid).unwrap();
        claude.pending_restore = Some(PendingAgentRestore {
            agent: "claude".into(),
            session_id: Some("11111111-2222-3333-4444-555555555555".into()),
        });

        let codex_tid = terminal_id_for(&state, 1);
        let codex = state.terminals.get_mut(&codex_tid).unwrap();
        codex.pending_restore = Some(PendingAgentRestore {
            agent: "codex".into(),
            session_id: Some("019e8d78-4a5e-78d2-968c-02240ac6e9e9".into()),
        });
        codex.detected_agent = Some(crate::detect::Agent::Codex);

        let pi_tid = terminal_id_for(&state, 2);
        let pi = state.terminals.get_mut(&pi_tid).unwrap();
        pi.pending_restore = Some(PendingAgentRestore {
            agent: "pi".into(),
            session_id: None,
        });

        // claude pane without a recorded session id is not guessed from cwd:
        // multiple panes can share the same cwd but point at different sessions.
        let lost_tid = terminal_id_for(&state, 3);
        let lost = state.terminals.get_mut(&lost_tid).unwrap();
        lost.pending_restore = Some(PendingAgentRestore {
            agent: "claude".into(),
            session_id: None,
        });

        state
            .agent_restore_config
            .commands
            .insert("pi".into(), "pi".into());

        let entries = agent_restore_plan(&state);
        assert_eq!(entries.len(), 4);

        match &entries[0].outcome {
            AgentRestoreOutcome::Launch(command) => assert_eq!(
                command,
                "claude --resume 11111111-2222-3333-4444-555555555555"
            ),
            AgentRestoreOutcome::Skip(reason) => panic!("claude skipped: {reason}"),
        }
        match &entries[1].outcome {
            AgentRestoreOutcome::Skip(reason) => assert_eq!(*reason, "agent already running"),
            AgentRestoreOutcome::Launch(command) => panic!("codex launched: {command}"),
        }
        match &entries[2].outcome {
            AgentRestoreOutcome::Skip(reason) => assert_eq!(*reason, "no resumable session found"),
            AgentRestoreOutcome::Launch(command) => panic!("pi launched: {command}"),
        }
        match &entries[3].outcome {
            AgentRestoreOutcome::Skip(reason) => assert_eq!(*reason, "no resumable session found"),
            AgentRestoreOutcome::Launch(command) => panic!("lost claude launched: {command}"),
        }
    }

    #[test]
    fn plan_rejects_unsafe_persisted_session_ids() {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("ws")];
        state.ensure_test_terminals();
        let tid = terminal_id_for(&state, 0);
        let terminal = state.terminals.get_mut(&tid).unwrap();
        terminal.pending_restore = Some(PendingAgentRestore {
            agent: "claude".into(),
            session_id: Some("evil; rm -rf /".into()),
        });

        let entries = agent_restore_plan(&state);
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].outcome,
            AgentRestoreOutcome::Skip("no resumable session found")
        ));
    }

    #[test]
    fn plan_does_not_guess_shared_cwd_sessions() {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = vec![
            crate::workspace::Workspace::test_new("same-cwd-a"),
            crate::workspace::Workspace::test_new("same-cwd-b"),
            crate::workspace::Workspace::test_new("same-cwd-c"),
        ];
        state.ensure_test_terminals();
        let shared_cwd = std::env::temp_dir().join(format!(
            "herdr-agent-restore-shared-cwd-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&shared_cwd).unwrap();

        for ws_idx in 0..3 {
            let tid = terminal_id_for(&state, ws_idx);
            let terminal = state.terminals.get_mut(&tid).unwrap();
            terminal.cwd = shared_cwd.clone();
            terminal.pending_restore = Some(PendingAgentRestore {
                agent: "codex".into(),
                session_id: None,
            });
        }

        let entries = agent_restore_plan(&state);
        assert_eq!(entries.len(), 3);
        assert!(
            entries.iter().all(|entry| matches!(
                entry.outcome,
                AgentRestoreOutcome::Skip("no resumable session found")
            )),
            "restore must not infer one latest cwd session for multiple panes"
        );

        let _ = std::fs::remove_dir_all(&shared_cwd);
    }

    #[test]
    fn pane_session_lifecycle_restores_same_id_after_snapshot_and_pane_died() {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("work")];
        state.ensure_test_terminals();
        let pane_id = state.workspaces[0].tabs[0].root_pane;

        state.handle_app_event(crate::events::AppEvent::AgentSessionObserved {
            pane_id,
            agent: crate::detect::Agent::Codex,
            session_id: "019ef3a2-749c-7b52-b324-2c20cb0b2379".into(),
        });

        let first_snapshot = crate::persist::capture(
            &state.workspaces,
            &state.terminals,
            &state.terminal_runtimes,
            state.active,
            state.selected,
            state.agent_panel_scope,
            state.sidebar_width,
            state.sidebar_section_split,
            &state.collapsed_workspace_sections,
            &state.agent_session_ledger,
        );
        let restore = first_snapshot.workspaces[0].tabs[0].panes[&pane_id.raw()]
            .agent_restore
            .clone()
            .expect("first snapshot restore metadata");
        assert_eq!(
            restore.session_id.as_deref(),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379")
        );

        let terminal_id = terminal_id_for(&state, 0);
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = None;
        terminal.pending_restore = Some(PendingAgentRestore {
            agent: restore.agent,
            session_id: restore.session_id,
        });

        let entries = agent_restore_plan(&state);
        assert_eq!(entries.len(), 1);
        match &entries[0].outcome {
            AgentRestoreOutcome::Launch(command) => {
                assert_eq!(command, "codex resume 019ef3a2-749c-7b52-b324-2c20cb0b2379");
            }
            AgentRestoreOutcome::Skip(reason) => panic!("codex skipped: {reason}"),
        }

        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.pending_restore = None;
        terminal.agent_session_id = None;
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.state = crate::detect::AgentState::Working;
        state.handle_app_event(crate::events::AppEvent::PaneDied { pane_id });

        let second_snapshot = crate::persist::capture(
            &state.workspaces,
            &state.terminals,
            &state.terminal_runtimes,
            state.active,
            state.selected,
            state.agent_panel_scope,
            state.sidebar_width,
            state.sidebar_section_split,
            &state.collapsed_workspace_sections,
            &state.agent_session_ledger,
        );
        let restore = second_snapshot.workspaces[0].tabs[0].panes[&pane_id.raw()]
            .agent_restore
            .clone()
            .expect("second snapshot restore metadata");
        assert_eq!(
            restore.session_id.as_deref(),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379")
        );

        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.pending_restore = Some(PendingAgentRestore {
            agent: restore.agent,
            session_id: restore.session_id,
        });
        let entries = agent_restore_plan(&state);
        assert_eq!(entries.len(), 1);
        match &entries[0].outcome {
            AgentRestoreOutcome::Launch(command) => {
                assert_eq!(command, "codex resume 019ef3a2-749c-7b52-b324-2c20cb0b2379");
            }
            AgentRestoreOutcome::Skip(reason) => panic!("codex skipped after PaneDied: {reason}"),
        }
    }

    #[test]
    fn empty_restore_toast_mentions_running_agents_when_present() {
        assert_eq!(restore_toast_context(&[], 0), "no pending restore");
        assert_eq!(
            restore_toast_context(&[], 3),
            "no pending restore, 3 already running"
        );
    }
}
