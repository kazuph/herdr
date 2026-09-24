use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentPromptParams, AgentRenameParams, AgentRestoreActionInfo, AgentRestoreActionStatus,
    AgentRestoreParams, AgentSendParams, AgentStartParams, AgentTarget, PaneReadResult, ReadFormat,
    ReadSource, ResponseResult,
};
use crate::app::{
    api::AGENT_SEND_SUBMIT_DELAY,
    api_helpers::{encode_api_keys, encode_api_text},
    App,
};

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

// Codex's Windows input reader does not surface bracketed paste. It detects the prompt as a
// "paste burst" and, while that burst is buffered, rewrites a following Enter into a newline
// instead of submitting. The burst only flushes after an idle timeout, so any size-based delay is
// a timing guess that fails when ConPTY delivery lags it. Codex flushes a buffered burst
// synchronously when it receives a non-character key, so appending one after the paste gives the
// submission a deterministic paste boundary regardless of prompt size or delivery speed.
#[cfg(windows)]
fn append_codex_paste_boundary(runtime: &crate::terminal::TerminalRuntime, text: &mut Vec<u8>) {
    let keys = match crate::app::api_helpers::encode_api_keys(runtime, &["right".to_string()]) {
        Ok(keys) => keys,
        Err(key) => {
            tracing::warn!(key = %key, "failed to encode Codex paste boundary key");
            return;
        }
    };
    if let Some(key) = keys.into_iter().find(|bytes| !bytes.is_empty()) {
        text.extend_from_slice(&key);
    }
}

impl App {
    pub(crate) fn handle_deferred_agent_api_request(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let crate::api::schema::Method::AgentPrompt(params) = request.method else {
            return false;
        };
        match self.queue_agent_prompt(request.id, params) {
            Ok((id, agent, completion)) => {
                std::thread::spawn(move || {
                    let response = match completion.recv() {
                        Ok(Ok(())) => encode_success(id, ResponseResult::AgentPrompted { agent }),
                        Ok(Err(err)) if err.kind() == std::io::ErrorKind::TimedOut => {
                            encode_error(id, "timeout", err.to_string())
                        }
                        Ok(Err(err)) => encode_error(id, "agent_prompt_failed", err.to_string()),
                        Err(_) => encode_error(id, "agent_prompt_failed", "pty actor closed"),
                    };
                    let _ = respond_to.send(response);
                });
            }
            Err(response) => {
                let _ = respond_to.send(response);
            }
        }
        true
    }

    fn queue_agent_prompt(
        &mut self,
        id: String,
        params: AgentPromptParams,
    ) -> Result<
        (
            String,
            crate::api::schema::AgentInfo,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        ),
        String,
    > {
        if params.text.is_empty() {
            return Err(encode_error(
                id,
                "empty_agent_prompt",
                "agent prompt must not be empty",
            ));
        }
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return Err(encode_error_body(id, self.agent_target_error_body(err))),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return Err(agent_not_found(id, &params.target));
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if terminal.state == crate::detect::AgentState::Blocked {
            return Err(encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and requires interactive input",
                    params.target
                ),
            ));
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return Err(agent_not_ready(id, &params.target));
        };
        if terminal.managed_agent_launch_pending() || terminal.pending_agent_resume_plan.is_some() {
            return Err(agent_not_ready(id, &params.target));
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return Err(encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            ));
        }
        #[cfg(windows)]
        let submit_deadline = params
            .wait
            .as_ref()
            .and_then(|wait| wait.submission_deadline);
        #[cfg(not(windows))]
        let submit_deadline = None;
        if expected_agent == crate::detect::Agent::GithubCopilot {
            // Copilot ignores synthetic Enter after focus loss until it receives focus gained.
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => {
                    return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
                }
            };
            if let Err(err) = runtime.try_send_bytes(Bytes::from(focus)) {
                return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
            }
        }
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        #[cfg(windows)]
        let text = if expected_agent == crate::detect::Agent::Codex {
            let mut text = text;
            append_codex_paste_boundary(runtime, &mut text);
            text
        } else {
            text
        };
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        let completion = runtime
            .queue_user_input_submission(
                Bytes::from(text),
                Bytes::from(enter),
                AGENT_PROMPT_SUBMIT_DELAY,
                submit_deadline,
            )
            .map_err(|err| encode_error(id.clone(), "agent_prompt_failed", err.to_string()))?;
        Ok((id, agent, completion))
    }

    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        self.reconcile_managed_agent_target(&target.target);
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
                    let command = crate::app::agent_resume::shell_command_from_argv(
                        &plan.argv,
                        crate::pane::PaneShellConfig::new(
                            &self.state.default_shell,
                            self.state.shell_mode,
                        ),
                    );
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
                                .unwrap_or_else(|| format!("p{}", pane_id.raw())),
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

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(all(test, unix))]
#[path = "../../../tests/support/prompt_probe.rs"]
mod prompt_probe;

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

    #[cfg(unix)]
    mod prompt {
        use super::*;
        use std::{fs, path::PathBuf, time::Instant};

        struct Probe {
            app: App,
            base: PathBuf,
            pane: crate::layout::PaneId,
            terminal: crate::terminal::TerminalId,
        }

        impl Drop for Probe {
            fn drop(&mut self) {
                self.app.terminal_runtimes.remove(&self.terminal);
                let _ = fs::remove_dir_all(&self.base);
            }
        }

        impl Probe {
            fn new(agent: Agent, bracketed: bool) -> Self {
                let mut app = app_with_agent();
                let pane = app.state.workspaces[0].tabs[0].root_pane;
                let terminal = app.state.workspaces[0].tabs[0].panes[&pane]
                    .attached_terminal_id
                    .clone();
                let base = PathBuf::from(format!(
                    "/tmp/hpr-{}-{}",
                    std::process::id(),
                    crate::terminal::TerminalId::alloc()
                ));
                fs::create_dir_all(&base).unwrap();
                let binary = prompt_probe::compile(&base);
                let executable = base.join(crate::detect::agent_label(agent));
                std::os::unix::fs::symlink(binary, &executable).unwrap();
                let argv = vec![
                    executable.to_string_lossy().into_owned(),
                    base.join("input.jsonl").to_string_lossy().into_owned(),
                    "-".into(),
                    "-".into(),
                    crate::detect::agent_label(agent).into(),
                    if bracketed { "bracketed" } else { "raw" }.into(),
                    "-".into(),
                ];
                let env = crate::pane::PaneLaunchEnv::from_extra(vec![
                    (
                        "XDG_CONFIG_HOME".into(),
                        base.join("config").to_string_lossy().into_owned(),
                    ),
                    (
                        "XDG_RUNTIME_DIR".into(),
                        base.join("run").to_string_lossy().into_owned(),
                    ),
                    (
                        "HERDR_SOCKET_PATH".into(),
                        base.join("s").to_string_lossy().into_owned(),
                    ),
                ])
                .without_pane_identity();
                let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
                    pane,
                    24,
                    80,
                    base.clone(),
                    &argv,
                    &env,
                    crate::pane::AgentDetection::Enabled,
                    0,
                    Default::default(),
                    app.event_tx.clone(),
                    app.render_notify.clone(),
                    app.render_dirty.clone(),
                )
                .unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !runtime.recent_text(24).contains("PROMPT_AGENT_READY") {
                    assert!(
                        Instant::now() < deadline,
                        "real PTY probe did not start: {}",
                        runtime.recent_text(24)
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(crate::app::agents::runtime_hosts_agent(&runtime, agent));
                app.terminal_runtimes.insert(terminal.clone(), runtime);
                let state = app.state.terminals.get_mut(&terminal).unwrap();
                state.set_agent_name("reviewer".into());
                state.set_detected_state(Some(agent), AgentState::Idle);
                Self {
                    app,
                    base,
                    pane,
                    terminal,
                }
            }

            fn submit(&mut self, target: &str, text: &str) -> std::sync::mpsc::Receiver<String> {
                let (respond_to, receiver) = std::sync::mpsc::channel();
                assert!(self.app.handle_deferred_agent_api_request(
                    crate::api::schema::Request {
                        id: "prompt".into(),
                        method: crate::api::schema::Method::AgentPrompt(AgentPromptParams {
                            target: target.into(),
                            text: text.into(),
                            wait: None,
                        }),
                    },
                    respond_to
                ));
                receiver
            }

            fn input(&self) -> Vec<u8> {
                fs::read_to_string(self.base.join("input.jsonl"))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .filter(|row| row["kind"] == "bytes")
                    .flat_map(|row| {
                        row["hex"]
                            .as_str()
                            .unwrap()
                            .as_bytes()
                            .chunks_exact(2)
                            .map(|hex| {
                                u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap()
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect()
            }

            fn wait_input(&self, expected: &[u8]) {
                let deadline = Instant::now() + Duration::from_secs(5);
                while self.input().len() < expected.len() {
                    assert!(
                        Instant::now() < deadline,
                        "input was not received: {:?}",
                        self.input()
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(self.input(), expected);
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_and_legacy_send_keep_both_submissions_separate() {
            for prompt_first in [false, true] {
                let mut probe = Probe::new(Agent::Pi, true);
                let prompt = prompt_first.then(|| probe.submit("reviewer", "new"));
                let response: SuccessResponse = serde_json::from_str(&probe.app.handle_agent_send(
                    "legacy".into(),
                    AgentSendParams {
                        target: "reviewer".into(),
                        text: "old\n\n".into(),
                    },
                ))
                .unwrap();
                assert!(matches!(response.result, ResponseResult::Ok {}));
                let prompt = prompt.unwrap_or_else(|| probe.submit("reviewer", "new"));
                let response: SuccessResponse =
                    serde_json::from_str(&prompt.recv_timeout(Duration::from_secs(5)).unwrap())
                        .unwrap();
                assert!(matches!(
                    response.result,
                    ResponseResult::AgentPrompted { .. }
                ));
                let expected = if prompt_first {
                    b"\x1b[200~new\x1b[201~\r\x1b[200~old\x1b[201~\r".as_slice()
                } else {
                    b"\x1b[200~old\x1b[201~\r\x1b[200~new\x1b[201~\r".as_slice()
                };
                probe.wait_input(expected);
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_copilot_focus_and_body_do_not_cross_mailbox_input() {
            for mailbox_first in [false, true] {
                let mut probe = Probe::new(Agent::GithubCopilot, true);
                let (tx, rx) = std::sync::mpsc::channel();
                let mailbox = |probe: &Probe| {
                    probe
                        .app
                        .terminal_runtimes
                        .get(&probe.terminal)
                        .unwrap()
                        .try_write_mailbox_bytes(
                            bytes::Bytes::from_static(b"mail\r"),
                            Box::new(move |result| {
                                tx.send(result).unwrap();
                            }),
                        )
                        .unwrap();
                };
                let prompt = if mailbox_first {
                    mailbox(&probe);
                    probe.submit("reviewer", "prompt")
                } else {
                    let prompt = probe.submit("reviewer", "prompt");
                    mailbox(&probe);
                    prompt
                };
                let _: SuccessResponse =
                    serde_json::from_str(&prompt.recv_timeout(Duration::from_secs(5)).unwrap())
                        .unwrap();
                rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
                let expected = if mailbox_first {
                    b"mail\r\x1b[I\x1b[200~prompt\x1b[201~\r".as_slice()
                } else {
                    b"\x1b[I\x1b[200~prompt\x1b[201~\rmail\r".as_slice()
                };
                probe.wait_input(expected);
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_waits_for_wrapped_argv_launch_to_settle() {
            for prefix in [
                vec!["/bin/sh", "-lc", "exec \"$@\"", "sh"],
                vec!["/usr/bin/env"],
            ] {
                let mut probe = Probe::new(Agent::Pi, true);
                probe.app.terminal_runtimes.drain().for_each(drop);
                probe.app.state.workspaces.clear();
                probe.app.state.terminals.clear();
                probe.app.state.active = None;
                let mut argv: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
                argv.extend([
                    probe.base.join("pi").to_string_lossy().into_owned(),
                    probe
                        .base
                        .join("wrapped.jsonl")
                        .to_string_lossy()
                        .into_owned(),
                    "-".into(),
                    "-".into(),
                    "pi".into(),
                    "bracketed".into(),
                    "-".into(),
                ]);
                let params = serde_json::from_value(
                    serde_json::json!({"name":"wrapped", "cwd":probe.base, "argv":argv}),
                )
                .unwrap();
                let extra_env = vec![
                    (
                        "XDG_CONFIG_HOME".into(),
                        probe.base.join("config").to_string_lossy().into_owned(),
                    ),
                    (
                        "XDG_RUNTIME_DIR".into(),
                        probe.base.join("run").to_string_lossy().into_owned(),
                    ),
                    (
                        "HERDR_SOCKET_PATH".into(),
                        probe.base.join("s").to_string_lossy().into_owned(),
                    ),
                ];
                let (agent, _) = probe
                    .app
                    .start_agent(params, extra_env)
                    .unwrap_or_else(|err| {
                        panic!("{}", probe.app.agent_start_error_body(err).message)
                    });
                let resolved = probe.app.resolve_agent_target(&agent.pane_id).unwrap();
                probe.pane = resolved.pane_id;
                probe.terminal = probe.app.state.workspaces[resolved.ws_idx]
                    .terminal_id(probe.pane)
                    .cloned()
                    .unwrap();
                let runtime = &probe.app.terminal_runtimes.get(&probe.terminal).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !runtime.recent_text(24).contains("PROMPT_AGENT_READY") {
                    assert!(Instant::now() < deadline, "wrapped probe did not start");
                    std::thread::sleep(Duration::from_millis(10));
                }
                probe
                    .app
                    .state
                    .terminals
                    .get_mut(&probe.terminal)
                    .unwrap()
                    .set_detected_state(Some(Agent::Pi), AgentState::Idle);
                let response = probe
                    .submit("wrapped", "must wait")
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                let response: ErrorResponse = serde_json::from_str(&response)
                    .expect("wrapped launches must reject prompts during their settle interval");
                assert_eq!(response.error.code, "agent_not_ready");
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_sends_text_then_delays_enter() {
            for bracketed in [false, true] {
                let mut probe = Probe::new(Agent::OpenCode, bracketed);
                let body = "A != B\n\n";
                let target = probe.app.public_pane_id(0, probe.pane).unwrap();
                let began = Instant::now();
                let response = probe.submit(&target, body);
                assert!(response.try_recv().is_err());
                let response: SuccessResponse =
                    serde_json::from_str(&response.recv_timeout(Duration::from_secs(5)).unwrap())
                        .unwrap();
                assert!(matches!(
                    response.result,
                    ResponseResult::AgentPrompted { .. }
                ));
                assert!(began.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);
                let expected = if bracketed {
                    format!("\x1b[200~{body}\x1b[201~\r")
                } else {
                    format!("{body}\r")
                };
                probe.wait_input(expected.as_bytes());
                let rejected: ErrorResponse =
                    serde_json::from_str(&probe.submit("opencode", "wrong target").recv().unwrap())
                        .unwrap();
                assert_eq!(rejected.error.code, "agent_not_found");
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_rejects_blocked_agent_without_writing() {
            let mut probe = Probe::new(Agent::GithubCopilot, true);
            probe
                .app
                .state
                .terminals
                .get_mut(&probe.terminal)
                .unwrap()
                .set_detected_state(Some(Agent::GithubCopilot), AgentState::Blocked);
            let response: ErrorResponse =
                serde_json::from_str(&probe.submit("reviewer", "unrelated prompt").recv().unwrap())
                    .unwrap();
            assert_eq!(response.error.code, "agent_blocked");
            std::thread::sleep(AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100));
            assert!(probe.input().is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_focuses_copilot_before_submitting() {
            let mut probe = Probe::new(Agent::GithubCopilot, true);
            let response: SuccessResponse =
                serde_json::from_str(&probe.submit("reviewer", "A != B").recv().unwrap()).unwrap();
            assert!(matches!(
                response.result,
                ResponseResult::AgentPrompted { .. }
            ));
            probe.wait_input(b"\x1b[I\x1b[200~A != B\x1b[201~\r");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_rejects_agent_while_startup_is_pending() {
            let mut probe = Probe::new(Agent::OpenCode, true);
            probe
                .app
                .state
                .terminals
                .get_mut(&probe.terminal)
                .unwrap()
                .begin_managed_agent(
                    "reviewer".into(),
                    Agent::OpenCode,
                    Instant::now(),
                    crate::app::agents::AGENT_START_SETTLE_DELAY,
                    Duration::from_secs(30),
                );
            let response: ErrorResponse =
                serde_json::from_str(&probe.submit("reviewer", "A != B").recv().unwrap()).unwrap();
            assert_eq!(response.error.code, "agent_not_ready");
            assert!(probe.input().is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn agent_prompt_rejects_empty_or_unknown_agent_without_writing() {
            let mut probe = Probe::new(Agent::Pi, false);
            let response: ErrorResponse =
                serde_json::from_str(&probe.submit("reviewer", "").recv().unwrap()).unwrap();
            assert_eq!(response.error.code, "empty_agent_prompt");
            probe
                .app
                .state
                .terminals
                .get_mut(&probe.terminal)
                .unwrap()
                .set_detected_state(None, AgentState::Unknown);
            let response: ErrorResponse =
                serde_json::from_str(&probe.submit("reviewer", "text").recv().unwrap()).unwrap();
            assert_eq!(response.error.code, "agent_not_ready");
            assert!(probe.input().is_empty());
        }
    }
}
