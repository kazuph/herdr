use bytes::Bytes;

use crate::api::schema::{
    AgentRenameParams, AgentRestoreActionInfo, AgentRestoreActionStatus, AgentRestoreParams,
    AgentSendParams, AgentStartParams, AgentTarget, PaneReadResult, ReadFormat, ReadSource,
    ResponseResult,
};
use crate::app::{
    api::AGENT_SEND_SUBMIT_DELAY,
    api_helpers::{encode_api_keys, encode_api_text},
    App,
};

use super::responses::{encode_error, encode_error_body, encode_success};

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let extra_env = match super::env::normalize_launch_env(params.env.clone()) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let (agent, argv) = match self.start_agent(params, extra_env) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };
        if let Some(model) = crate::detect::model_from_cmdline(&argv.join(" ")) {
            if let Some(actor_name) = agent.name.as_deref().or(agent.agent.as_deref()) {
                if let Ok(store) = crate::dispatch::DispatchStore::open_active() {
                    let _ = store.upsert_actor(
                        "agent",
                        actor_name,
                        agent.agent.as_deref(),
                        Some(&model),
                        None,
                        Some(&agent.pane_id),
                    );
                }
            }
        }

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_restore(
        &mut self,
        id: String,
        params: AgentRestoreParams,
    ) -> String {
        let mut actions = self.collect_agent_restore_actions(if params.dry_run {
            AgentRestoreActionStatus::WouldLaunch
        } else {
            AgentRestoreActionStatus::Launched
        });
        if !params.dry_run {
            let now = std::time::Instant::now();
            self.sync_pending_agent_resume_deadline(now);
            let _ = self.start_pending_agent_resumes(true);
            for (terminal_id, action) in &mut actions {
                if action.status == AgentRestoreActionStatus::Launched
                    && self
                        .state
                        .terminals
                        .get(terminal_id)
                        .is_some_and(|terminal| terminal.pending_agent_resume_plan.is_some())
                {
                    action.status = AgentRestoreActionStatus::Skipped;
                    action.reason = Some("failed to start resume shell".into());
                }
            }
        }
        encode_success(
            id,
            ResponseResult::AgentRestore {
                actions: actions.into_iter().map(|(_, action)| action).collect(),
            },
        )
    }

    fn collect_agent_restore_actions(
        &self,
        status: AgentRestoreActionStatus,
    ) -> Vec<(crate::terminal::TerminalId, AgentRestoreActionInfo)> {
        let mut actions = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.layout.pane_ids() {
                    let Some(pane) = tab.panes.get(&pane_id) else {
                        continue;
                    };
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    let Some(plan) = terminal.pending_agent_resume_plan.as_ref() else {
                        continue;
                    };
                    let command = crate::app::agent_resume::shell_command_from_argv(&plan.argv);
                    let (status, reason) = if command.is_none() {
                        (
                            AgentRestoreActionStatus::Skipped,
                            Some("no resumable session found".into()),
                        )
                    } else if self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some()
                    {
                        (
                            AgentRestoreActionStatus::Skipped,
                            Some("agent already running".into()),
                        )
                    } else {
                        (status.clone(), None)
                    };
                    actions.push((
                        pane.attached_terminal_id.clone(),
                        AgentRestoreActionInfo {
                            pane_id: self
                                .public_pane_id(ws_idx, pane_id)
                                .unwrap_or_else(|| format!("p_{}", pane_id.raw())),
                            agent: plan.agent.clone(),
                            status,
                            command,
                            reason,
                        },
                    ));
                }
            }
        }
        actions
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
        let text = match params.format {
            ReadFormat::Text => match params.source {
                ReadSource::Visible => pane.visible_text(),
                ReadSource::Recent => pane.recent_text(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
            ReadFormat::Ansi => match params.source {
                ReadSource::Visible => pane.visible_ansi(),
                ReadSource::Recent => pane.recent_ansi(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_ansi(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
        };

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_terminal_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        if terminal.full_lifecycle_hook_authority_active() {
            let explain = serde_json::json!({
                "agent": terminal.effective_agent_label().unwrap_or("unknown"),
                "state": crate::detect::manifest::agent_state_label(terminal.state),
                "manifest_source": null,
                "manifest_version": null,
                "cached_remote_version": null,
                "local_override_shadowing_remote": false,
                "remote_update_status": null,
                "remote_update_error": null,
                "matched_rule": null,
                "visible_idle": false,
                "visible_blocker": false,
                "visible_working": false,
                "screen_detection_skipped": true,
                "screen_detection_skip_reason": "full_lifecycle_hook_authority",
                "skip_state_update": false,
                "skipped_update_reason": null,
                "fallback_reason": null,
                "warning": null,
                "evaluated_rules": [],
            });
            return encode_success(id, ResponseResult::AgentExplain { explain });
        }
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let value = crate::detect::manifest::explain_to_json_value(&explain);

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send(&mut self, id: String, params: AgentSendParams) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        if self.agent_info(resolved.ws_idx, resolved.pane_id).is_none() {
            return agent_not_found(id, &params.target);
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        let text = params.text.trim_end_matches(&['\r', '\n'][..]);
        let text_bytes = encode_api_text(runtime, text);
        let enter = match encode_api_keys(runtime, &["enter".to_string()]) {
            Ok(mut encoded_keys) => encoded_keys.pop().unwrap_or_default(),
            Err(key) => {
                return encode_error(id, "invalid_key", format!("unsupported key {key}"));
            }
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(text_bytes)) {
            return encode_error(id, "agent_send_failed", err.to_string());
        }
        std::thread::sleep(AGENT_SEND_SUBMIT_DELAY);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(enter)) {
            return encode_error(id, "agent_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentStatus, ErrorResponse, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    fn app_with_pi_runtime(capacity: usize) -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, capacity);
        app.state.insert_test_runtime(pane_id, runtime);
        (app, rx)
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: "pi".into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[tokio::test]
    async fn agent_send_writes_text_then_submits_with_enter() {
        let (mut app, mut rx) = app_with_pi_runtime(2);

        let response = app.handle_agent_send(
            "req".into(),
            AgentSendParams {
                target: "pi".into(),
                text: "hello agent".into(),
            },
        );

        assert_eq!(
            AGENT_SEND_SUBMIT_DELAY,
            std::time::Duration::from_millis(500)
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"hello agent")
        );
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_send_normalizes_trailing_newlines_to_one_enter() {
        let (mut app, mut rx) = app_with_pi_runtime(2);

        let response = app.handle_agent_send(
            "req".into(),
            AgentSendParams {
                target: "pi".into(),
                text: "hello agent\r\n\n".into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"hello agent")
        );
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_send_rejects_a_normal_shell_target() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .to_string();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_send(
            "req".into(),
            AgentSendParams {
                target: terminal_id,
                text: "must not run".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert!(rx.try_recv().is_err());
    }
}
