use bytes::Bytes;

use super::{
    api::AGENT_SEND_SUBMIT_DELAY,
    api_helpers::{encode_api_keys, encode_api_text},
    App,
};
use crate::api::schema::{
    AgentInfo, AgentStatus, ErrorBody, MsgHistoryParams, MsgInboxParams, MsgSendParams,
    ResponseResult,
};

impl App {
    pub(super) fn handle_msg_send(
        &mut self,
        params: MsgSendParams,
    ) -> Result<ResponseResult, ErrorBody> {
        let room = normalize_room(&params.room)?;
        let from_agent = normalize_agent(&params.from_agent, "from_agent")?;
        let body = normalize_body(&params.body)?;
        let recipients = if params.to == "*" {
            self.broadcast_recipients(&room, &from_agent)?
        } else {
            vec![self.resolve_msg_recipient(&params.to)?]
        };

        let mut store = open_msg_store()?;
        let mut messages = Vec::with_capacity(recipients.len());
        for recipient in &recipients {
            messages.push(
                store
                    .insert_message_with_reply(
                        &room,
                        &params.project,
                        &from_agent,
                        recipient,
                        &body,
                        params.reply_to,
                    )
                    .map_err(msg_store_error)?,
            );
        }
        drop(store);

        let mut nudged = Vec::new();
        for recipient in recipients {
            if self.flush_msg_nudge_for(&room, &recipient)? {
                nudged.push(recipient);
            }
        }

        Ok(ResponseResult::MsgSend { messages, nudged })
    }

    pub(super) fn handle_msg_inbox(
        &mut self,
        params: MsgInboxParams,
    ) -> Result<ResponseResult, ErrorBody> {
        let room = normalize_room(&params.room)?;
        let to_agent = normalize_agent(&params.to_agent, "to_agent")?;
        let recipients = self.mailbox_recipient_aliases(&to_agent);
        let mut store = open_msg_store()?;
        let messages = store
            .unread_for_recipients(&room, &recipients)
            .map_err(msg_store_error)?;
        Ok(ResponseResult::MsgInbox { messages })
    }

    pub(super) fn handle_msg_history(
        &self,
        params: MsgHistoryParams,
    ) -> Result<ResponseResult, ErrorBody> {
        let room = normalize_room(&params.room)?;
        let store = open_msg_store()?;
        let messages = store
            .history(&room, params.project.as_deref(), params.limit.min(1000))
            .map_err(msg_store_error)?;
        Ok(ResponseResult::MsgHistory { messages })
    }

    pub(super) fn handle_msg_rooms(&self) -> Result<ResponseResult, ErrorBody> {
        let store = open_msg_store()?;
        let rooms = store.rooms().map_err(msg_store_error)?;
        Ok(ResponseResult::MsgRooms { rooms })
    }

    pub(super) fn flush_msg_nudges_for_idle_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let Some(agent) = self.agent_info(ws_idx, pane_id) else {
            return;
        };
        if agent.agent_status != AgentStatus::Idle {
            return;
        }
        let Some(mailbox) = mailbox_agent_name(&agent) else {
            return;
        };
        if let Err(err) = self.flush_msg_nudges_for_agent(&mailbox) {
            tracing::warn!(
                mailbox = %mailbox,
                err = %err.message,
                "failed to flush herdr msg nudges"
            );
        }
    }

    pub(crate) fn flush_msg_nudges_for_all_idle_agents(&mut self) {
        let agents = self.collect_agent_infos();
        for agent in agents {
            if agent.agent_status != AgentStatus::Idle {
                continue;
            }
            let Some(mailbox) = mailbox_agent_name(&agent) else {
                continue;
            };
            if let Err(err) = self.flush_msg_nudges_for_agent(&mailbox) {
                tracing::warn!(
                    mailbox = %mailbox,
                    err = %err.message,
                    "failed to flush startup herdr msg nudges"
                );
            }
        }
    }

    fn flush_msg_nudges_for_agent(&mut self, to_agent: &str) -> Result<(), ErrorBody> {
        let store = open_msg_store()?;
        let nudges = store
            .pending_nudges_for_agent(to_agent)
            .map_err(msg_store_error)?;
        drop(store);

        for nudge in nudges {
            let _ = self.flush_pending_msg_nudge(nudge)?;
        }
        Ok(())
    }

    fn flush_msg_nudge_for(&mut self, room: &str, to_agent: &str) -> Result<bool, ErrorBody> {
        let store = open_msg_store()?;
        let Some(nudge) = store
            .pending_nudge_for(room, to_agent)
            .map_err(msg_store_error)?
        else {
            return Ok(false);
        };
        drop(store);
        self.flush_pending_msg_nudge(nudge)
    }

    fn flush_pending_msg_nudge(
        &mut self,
        nudge: crate::msg::PendingNudge,
    ) -> Result<bool, ErrorBody> {
        let Ok(resolved) = self.resolve_terminal_target(&nudge.to_agent) else {
            return Ok(false);
        };
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Ok(false);
        };
        if agent.agent_status == AgentStatus::Blocked {
            return Ok(false);
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return Ok(false);
        };

        let store = open_msg_store()?;
        let messages = store
            .pending_messages_for_agent(&nudge.room, &nudge.to_agent)
            .map_err(msg_store_error)?;
        drop(store);
        if messages.is_empty() {
            return Ok(false);
        }
        let total_bytes = messages
            .iter()
            .map(|message| message.body.len())
            .sum::<usize>();
        if messages.len() > crate::msg::FALLBACK_MAX_MESSAGES
            || total_bytes > crate::msg::FALLBACK_MAX_BYTES
        {
            let inbox_command = format!("herdr inbox --room {}", quote_shell_arg(&nudge.room));
            let message = format!(
                "\u{1f4ec} 未読{}件 (room={})。`{inbox_command}` で確認して",
                messages.len(),
                nudge.room
            );
            inject_text_and_enter(runtime, &message)?;
            return Ok(true);
        } else if nudge.room == crate::msg::JOBS_ROOM {
            let message = messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            inject_text_and_enter(runtime, &message)?;
        } else {
            let message = messages
                .iter()
                .map(push_message_text)
                .collect::<Vec<_>>()
                .join("\n");
            inject_text_and_enter(runtime, &message)?;
        }
        let store = open_msg_store()?;
        store
            .mark_delivered(&nudge.room, &nudge.to_agent)
            .map_err(msg_store_error)?;
        Ok(true)
    }

    fn resolve_msg_recipient(&self, target: &str) -> Result<String, ErrorBody> {
        let agent = self
            .agent_info_for_target(target)
            .map_err(|err| self.agent_target_error_body(err))?;
        mailbox_agent_name(&agent).ok_or_else(|| ErrorBody {
            code: "agent_not_found".into(),
            message: format!("agent target {target} has no reported agent identity"),
        })
    }

    fn mailbox_recipient_aliases(&self, to_agent: &str) -> Vec<String> {
        let Ok(resolved) = self.resolve_terminal_target(to_agent) else {
            return vec![to_agent.to_string()];
        };
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return vec![to_agent.to_string()];
        };

        let mut aliases = vec![agent.global_pane_id, agent.short_pane_id];
        if let Some(name) = agent.name {
            let name_resolves_to_pane =
                self.resolve_terminal_target(&name)
                    .is_ok_and(|name_target| {
                        name_target.ws_idx == resolved.ws_idx
                            && name_target.pane_id == resolved.pane_id
                    });
            if name_resolves_to_pane {
                aliases.push(name);
            }
        }
        if let Some(public_pane_id) = self.public_pane_id(resolved.ws_idx, resolved.pane_id) {
            aliases.push(public_pane_id);
        }
        aliases.sort();
        aliases.dedup();
        aliases
    }

    fn broadcast_recipients(&self, room: &str, from_agent: &str) -> Result<Vec<String>, ErrorBody> {
        let store = open_msg_store()?;
        let mut recipients = store.participants(room).map_err(msg_store_error)?;
        recipients.extend(
            self.collect_agent_infos()
                .into_iter()
                .filter_map(|agent| mailbox_agent_name(&agent)),
        );
        recipients.sort();
        recipients.dedup();
        recipients.retain(|agent| agent != from_agent);
        if recipients.is_empty() {
            return Err(ErrorBody {
                code: "msg_no_recipients".into(),
                message: format!("room {room} has no broadcast recipients"),
            });
        }
        Ok(recipients)
    }
}

fn push_message_text(message: &crate::api::schema::MsgMessage) -> String {
    format!(
        "\u{1f4e8} [#{} room={} from={}] {}\n返信例: `herdr send {} --reply-to {} '...'`",
        message.id, message.room, message.from_agent, message.body, message.from_agent, message.id
    )
}

fn inject_text_and_enter(
    runtime: &crate::terminal::TerminalRuntime,
    message: &str,
) -> Result<(), ErrorBody> {
    let text_bytes = encode_api_text(runtime, message);
    let enter = match encode_api_keys(runtime, &["enter".to_string()]) {
        Ok(mut encoded_keys) => encoded_keys.pop().unwrap_or_default(),
        Err(key) => {
            return Err(ErrorBody {
                code: "invalid_key".into(),
                message: format!("unsupported key {key}"),
            });
        }
    };
    runtime
        .try_send_bytes(Bytes::from(text_bytes))
        .map_err(|err| ErrorBody {
            code: "msg_nudge_failed".into(),
            message: err.to_string(),
        })?;
    std::thread::sleep(AGENT_SEND_SUBMIT_DELAY);
    runtime
        .try_send_bytes(Bytes::from(enter))
        .map_err(|err| ErrorBody {
            code: "msg_nudge_failed".into(),
            message: err.to_string(),
        })?;
    Ok(())
}

fn open_msg_store() -> Result<crate::msg::MsgStore, ErrorBody> {
    crate::msg::MsgStore::open_active().map_err(msg_store_error)
}

fn msg_store_error(err: rusqlite::Error) -> ErrorBody {
    ErrorBody {
        code: "msg_store_error".into(),
        message: err.to_string(),
    }
}

fn normalize_room(room: &str) -> Result<String, ErrorBody> {
    let room = room.trim();
    if room.is_empty() {
        return Err(ErrorBody {
            code: "invalid_msg_room".into(),
            message: "room must not be empty".into(),
        });
    }
    Ok(room.to_string())
}

fn quote_shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn normalize_agent(value: &str, field: &str) -> Result<String, ErrorBody> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ErrorBody {
            code: "invalid_msg_agent".into(),
            message: format!("{field} must not be empty"),
        });
    }
    Ok(value.to_string())
}

fn normalize_body(body: &str) -> Result<String, ErrorBody> {
    if body.trim().is_empty() {
        return Err(ErrorBody {
            code: "invalid_msg_body".into(),
            message: "message body must not be empty".into(),
        });
    }
    Ok(body.to_string())
}

pub(crate) fn mailbox_agent_name(agent: &AgentInfo) -> Option<String> {
    agent
        .name
        .clone()
        .or_else(|| agent.agent.as_ref().map(|_| agent.global_pane_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{
            ErrorResponse, Method, MsgMessage, PaneAgentState, PaneReportAgentParams, Request,
            SuccessResponse,
        },
        config::Config,
        layout::PaneId,
        terminal::TerminalRuntime,
        workspace::Workspace,
    };
    use ratatui::layout::Direction;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    use tokio::sync::mpsc;

    static MSG_API_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestAgent {
        pane_id: PaneId,
        rx: mpsc::Receiver<bytes::Bytes>,
    }

    struct MsgApiHarness {
        app: App,
        db_path: PathBuf,
        agents: HashMap<String, TestAgent>,
    }

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn with_msg_api_harness<T>(names: &[&str], test: impl FnOnce(&mut MsgApiHarness) -> T) -> T {
        let _guard = crate::msg::msg_db_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _config_guard = crate::config::lock_test_config_env();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _runtime_guard = runtime.enter();
        let id = MSG_API_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("herdr-msg-api-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("msg.db");
        std::env::set_var(crate::msg::MSG_DB_PATH_ENV_VAR, &db_path);

        let mut harness = MsgApiHarness::new(names, db_path.clone());
        let result = test(&mut harness);

        std::env::remove_var(crate::msg::MSG_DB_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    impl MsgApiHarness {
        fn new(names: &[&str], db_path: PathBuf) -> Self {
            assert!(!names.is_empty());
            let mut app = test_app();
            let mut ws = Workspace::test_new("msg-api");
            let first = ws.tabs[0].root_pane;
            let mut panes = vec![first];
            for _ in 1..names.len() {
                panes.push(ws.test_split(Direction::Horizontal));
            }
            app.state.workspaces = vec![ws];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            let mut agents = HashMap::new();
            for (pane_id, name) in panes.into_iter().zip(names.iter().copied()) {
                let terminal_id = app.state.workspaces[0]
                    .pane_state(pane_id)
                    .unwrap()
                    .attached_terminal_id
                    .clone();
                let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
                terminal.set_agent_name(name.to_string());
                terminal.set_manual_label(name.to_string());

                let (runtime, rx) = TerminalRuntime::test_with_channel_capacity(80, 24, 16);
                app.state.insert_test_runtime(pane_id, runtime);
                agents.insert(name.to_string(), TestAgent { pane_id, rx });
            }

            let mut harness = Self {
                app,
                db_path,
                agents,
            };
            for name in names {
                harness.report_state(name, PaneAgentState::Working);
            }
            harness
        }

        fn report_state(&mut self, name: &str, state: PaneAgentState) {
            let pane_id = self.public_pane_id(name);
            let response = self.app.handle_api_request(Request {
                id: format!("report-{name}-{state:?}"),
                method: Method::PaneReportAgent(PaneReportAgentParams {
                    pane_id,
                    source: "test".into(),
                    agent: name.into(),
                    state,
                    message: None,
                    custom_status: None,
                    seq: None,
                    title: None,
                    agent_session_id: None,
                    session_id: None,
                    agent_session_path: None,
                    model: None,
                }),
            });
            success_result(&response);
        }

        fn send(&mut self, from_agent: &str, to: &str, room: &str, body: &str) -> Vec<String> {
            let response = self.app.handle_api_request(Request {
                id: format!("send-{from_agent}-{to}-{room}"),
                method: Method::MsgSend(MsgSendParams {
                    room: room.into(),
                    project: "/repo".into(),
                    from_agent: from_agent.into(),
                    to: to.into(),
                    body: body.into(),
                    reply_to: None,
                }),
            });
            let result = success_result(&response);
            match result.result {
                ResponseResult::MsgSend { messages, nudged } => {
                    assert_eq!(
                        messages
                            .iter()
                            .map(|message| message.body.as_str())
                            .collect::<Vec<_>>(),
                        vec![body; messages.len()]
                    );
                    nudged
                }
                other => panic!("expected msg.send result, got {other:?}"),
            }
        }

        fn send_error_code(&mut self, from_agent: &str, to: &str, room: &str) -> String {
            let response = self.app.handle_api_request(Request {
                id: format!("send-error-{from_agent}-{to}-{room}"),
                method: Method::MsgSend(MsgSendParams {
                    room: room.into(),
                    project: "/repo".into(),
                    from_agent: from_agent.into(),
                    to: to.into(),
                    body: "body".into(),
                    reply_to: None,
                }),
            });
            error_response(&response).error.code
        }

        fn history(&self, room: &str) -> Vec<MsgMessage> {
            let store = crate::msg::MsgStore::open_at(self.db_path.clone()).unwrap();
            store.history(room, Some("/repo"), 100).unwrap()
        }

        fn inbox(&mut self, to_agent: &str, room: &str) -> Vec<MsgMessage> {
            let response = self.app.handle_api_request(Request {
                id: format!("inbox-{to_agent}-{room}"),
                method: Method::MsgInbox(MsgInboxParams {
                    room: room.into(),
                    to_agent: to_agent.into(),
                }),
            });
            let result = success_result(&response);
            match result.result {
                ResponseResult::MsgInbox { messages } => messages,
                other => panic!("expected msg.inbox result, got {other:?}"),
            }
        }

        fn insert_queued_message(&self, room: &str, to_agent: &str, body: &str) {
            crate::msg::MsgStore::open_at(self.db_path.clone())
                .unwrap()
                .insert_messages(room, "/repo", "alpha", &[to_agent.to_string()], body)
                .unwrap();
        }

        fn received_texts(&mut self, name: &str) -> Vec<String> {
            let rx = &mut self.agents.get_mut(name).unwrap().rx;
            let mut texts = Vec::new();
            while let Ok(bytes) = rx.try_recv() {
                texts.push(String::from_utf8_lossy(&bytes).into_owned());
            }
            texts
        }

        fn public_pane_id(&self, name: &str) -> String {
            let agent = self.agents.get(name).unwrap();
            self.app.public_pane_id(0, agent.pane_id).unwrap()
        }

        fn global_pane_id(&self, name: &str) -> String {
            let agent = self.agents.get(name).unwrap();
            self.app
                .agent_info(0, agent.pane_id)
                .unwrap()
                .global_pane_id
        }
    }

    fn success_result(response: &str) -> SuccessResponse {
        serde_json::from_str(response).unwrap()
    }

    fn error_response(response: &str) -> ErrorResponse {
        serde_json::from_str(response).unwrap()
    }

    #[test]
    fn broadcast_expands_to_recipients_excludes_sender_and_errors_when_empty() {
        with_msg_api_harness(&["alpha", "beta", "gamma"], |harness| {
            let nudged = harness.send("alpha", "*", "broadcast", "hello everyone");

            let messages = harness.history("broadcast");
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.to_agent.as_str())
                    .collect::<Vec<_>>(),
                vec!["beta", "gamma"]
            );
            assert!(messages.iter().all(|message| message.from_agent == "alpha"));
            assert_eq!(nudged, vec!["beta", "gamma"]);
            assert!(!harness.received_texts("beta").is_empty());
            assert!(!harness.received_texts("gamma").is_empty());
        });

        with_msg_api_harness(&["alpha"], |harness| {
            let code = harness.send_error_code("alpha", "*", "broadcast-empty");

            assert_eq!(code, "msg_no_recipients");
            assert!(harness.history("broadcast-empty").is_empty());
        });
    }

    #[test]
    fn unresolved_direct_target_fails_closed_without_persisting_message() {
        with_msg_api_harness(&["alpha"], |harness| {
            let code = harness.send_error_code("alpha", "missing", "direct-missing");

            assert_eq!(code, "agent_not_found");
            assert!(harness.history("direct-missing").is_empty());
        });
    }

    #[test]
    fn inbox_reads_pane_id_and_agent_name_recipients_for_the_same_pane() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let room = "recipient-aliases";
            harness.insert_queued_message(room, &harness.global_pane_id("beta"), "pane id");
            harness.insert_queued_message(room, "codex", "other pane kind");

            let pane_id_messages = harness.inbox("beta", room);
            assert_eq!(
                pane_id_messages
                    .iter()
                    .map(|message| message.body.as_str())
                    .collect::<Vec<_>>(),
                vec!["pane id"]
            );
            assert!(harness
                .history(room)
                .iter()
                .find(|message| message.body == "pane id")
                .is_some_and(|message| message.delivered_at.is_some()));
            assert!(harness
                .history(room)
                .iter()
                .find(|message| message.body == "other pane kind")
                .is_some_and(|message| message.delivered_at.is_none()));

            harness.send("alpha", "beta", room, "agent name");
            let named_messages = harness.inbox("beta", room);
            assert_eq!(
                named_messages
                    .iter()
                    .map(|message| message.body.as_str())
                    .collect::<Vec<_>>(),
                vec!["agent name"]
            );
            assert!(harness
                .history(room)
                .iter()
                .find(|message| message.body == "agent name")
                .is_some_and(|message| message.delivered_at.is_some()));
        });
    }

    #[test]
    fn idle_and_working_recipients_receive_direct_push_but_blocked_waits() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);

            let nudged = harness.send("alpha", "beta", "idle-direct", "wake up");

            assert_eq!(nudged, vec!["beta"]);
            let texts = harness.received_texts("beta");
            assert_eq!(texts.len(), 2);
            assert!(texts[0].contains("📨 [#"));
            assert!(texts[0].contains("room=idle-direct"));
            assert!(texts[0].contains("返信例: `herdr send alpha --reply-to"));
            assert_eq!(texts[1], "\r");
            let delivered = harness.history("idle-direct");
            assert_eq!(delivered.len(), 1);
            assert!(delivered[0].delivered_at.is_some());

            harness.report_state("beta", PaneAgentState::Working);
            let nudged = harness.send("alpha", "beta", "busy-direct", "wait");

            assert_eq!(nudged, vec!["beta"]);
            assert!(!harness.received_texts("beta").is_empty());

            harness.report_state("beta", PaneAgentState::Blocked);
            let nudged = harness.send("alpha", "beta", "blocked-direct", "wait");

            assert!(nudged.is_empty());
            assert!(harness.received_texts("beta").is_empty());
            let queued = harness.history("blocked-direct");
            assert_eq!(queued.len(), 1);
            assert!(queued[0].delivered_at.is_none());
        });
    }

    #[test]
    fn six_messages_fallback_to_quoted_inbox_command() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            for index in 0..6 {
                harness.send(
                    "alpha",
                    "beta",
                    "review ui's notifications",
                    &format!("wake up {index}"),
                );
            }
            harness.report_state("beta", PaneAgentState::Idle);

            let texts = harness.received_texts("beta");
            assert_eq!(
                texts[0]
                    .split('`')
                    .nth(1)
                    .expect("fallback should contain an inbox command"),
                "herdr inbox --room 'review ui'\\''s notifications'"
            );
            let recipients = ["beta".to_string()];
            let unread = crate::msg::MsgStore::open_at(harness.db_path.clone())
                .unwrap()
                .unread_for_recipients("review ui's notifications", &recipients)
                .unwrap();
            assert_eq!(unread.len(), 6);
            let delivered = harness.history("review ui's notifications");
            assert!(delivered
                .iter()
                .all(|message| message.delivered_at.is_some()));
        });
    }

    #[test]
    fn herdr_job_messages_are_injected_directly_and_marked_read() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let body = "[herdr run] exit=0 label=tests job=job-1 details: herdr log job-1";

            let nudged = harness.send("herdr-run", "beta", crate::msg::JOBS_ROOM, body);

            assert_eq!(nudged, vec!["beta"]);
            let texts = harness.received_texts("beta");
            assert_eq!(texts, vec![body.to_string(), "\r".to_string()]);
            assert!(!texts[0].contains("herdr msg inbox"));
            let messages = harness.history(crate::msg::JOBS_ROOM);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_some());
            assert!(messages[0].read_at.is_some());
        });
    }

    #[test]
    fn blocked_then_idle_flushes_multiple_pending_messages_as_direct_push() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            harness.send("alpha", "beta", "status-flush", "one");
            harness.send("alpha", "beta", "status-flush", "two");
            harness.send("alpha", "beta", "status-flush", "three");
            assert!(harness.received_texts("beta").is_empty());

            harness.report_state("beta", PaneAgentState::Idle);

            let texts = harness.received_texts("beta");
            assert_eq!(texts.len(), 2);
            assert!(texts[0].contains("one"));
            assert!(texts[0].contains("two"));
            assert!(texts[0].contains("three"));
            assert!(texts[0].contains("room=status-flush"));
            assert_eq!(texts[1], "\r");
            let messages = harness.history("status-flush");
            assert_eq!(messages.len(), 3);
            assert!(messages
                .iter()
                .all(|message| message.delivered_at.is_some()));
        });
    }

    #[test]
    fn startup_flush_walk_delivers_pending_messages_after_server_restart() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            harness.send("alpha", "beta", "restart-flush", "one");
            harness.send("alpha", "beta", "restart-flush", "two");

            let db_path = harness.db_path.clone();
            let mut restarted = MsgApiHarness::new(&["alpha", "beta"], db_path);
            restarted.report_state("beta", PaneAgentState::Idle);
            restarted.app.flush_msg_nudges_for_all_idle_agents();

            let texts = restarted.received_texts("beta");
            assert_eq!(texts.len(), 2);
            assert!(texts[0].contains("one"));
            assert!(texts[0].contains("two"));
            assert!(texts[0].contains("room=restart-flush"));
            assert_eq!(texts[1], "\r");
            let messages = restarted.history("restart-flush");
            assert_eq!(messages.len(), 2);
            assert!(messages
                .iter()
                .all(|message| message.delivered_at.is_some()));
        });
    }
}
