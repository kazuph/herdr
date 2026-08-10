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
    pub(crate) fn handle_mailbox_write_finished(
        &mut self,
        message_id: i64,
        result: Result<(), String>,
    ) {
        if !self.regular_mail_in_flight.remove(&message_id) {
            return;
        }
        if let Err(err) = result {
            tracing::warn!(message_id, err, "mailbox PTY write failed");
            return;
        }
        match open_msg_store().and_then(|store| {
            store
                .mark_message_delivered(message_id)
                .map_err(msg_store_error)
        }) {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                message_id,
                "mailbox write completed for a non-pending message"
            ),
            Err(err) => {
                tracing::warn!(message_id, err = %err.message, "failed to record mailbox write completion")
            }
        }
    }

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
            vec![self.resolve_msg_recipient(&room, &params.to)?]
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
            let delivered = if room == crate::msg::JOBS_ROOM {
                self.flush_job_msg_nudge_for(&room, &recipient)?
            } else {
                self.flush_regular_message_for_recipient(&recipient)?
            };
            if delivered {
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
        let recipients = self.mailbox_recipient_aliases(&room, &to_agent);
        let mut store = open_msg_store()?;
        let messages = store
            .unread_for_recipients(&room, &recipients, &self.regular_mail_in_flight)
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

    pub(super) fn flush_msg_nudges_for_available_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let Some(agent) = self.agent_info(ws_idx, pane_id) else {
            return;
        };
        if !regular_mail_can_submit(agent.agent_status) {
            return;
        }
        self.flush_msg_nudges_for_available_agent(&agent);
    }

    pub(crate) fn flush_msg_nudges_for_all_available_agents(&mut self) {
        let agents = self.collect_agent_infos();
        for agent in agents {
            if !regular_mail_can_submit(agent.agent_status) {
                continue;
            }
            self.flush_msg_nudges_for_available_agent(&agent);
        }
    }

    fn flush_msg_nudges_for_available_agent(&mut self, agent: &AgentInfo) {
        if let Err(err) = self.flush_regular_messages_for_available_agent(agent) {
            tracing::warn!(
                recipient = %agent.global_pane_id,
                err = %err.message,
                "failed to flush herdr msg nudges"
            );
        }
        let Some(mailbox) = mailbox_agent_name(agent) else {
            return;
        };
        if let Err(err) = self.flush_job_msg_nudges_for_agent(&mailbox) {
            tracing::warn!(
                recipient = %mailbox,
                err = %err.message,
                "failed to flush herdr msg nudges"
            );
        }
    }

    fn flush_regular_messages_for_available_agent(
        &mut self,
        agent: &AgentInfo,
    ) -> Result<bool, ErrorBody> {
        let store = open_msg_store()?;
        let messages = store
            .pending_messages_for_agent_in_creation_order(&agent.global_pane_id)
            .map_err(msg_store_error)?;
        drop(store);
        if messages.is_empty() {
            return Ok(false);
        }
        let Ok(resolved) = self.resolve_terminal_target(&agent.global_pane_id) else {
            return Ok(false);
        };
        let mut in_flight = std::mem::take(&mut self.regular_mail_in_flight);
        let result = {
            let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id)
            else {
                self.regular_mail_in_flight = in_flight;
                return Ok(false);
            };
            deliver_regular_messages(&self.mailbox_write_tx, &mut in_flight, runtime, messages)
        };
        self.regular_mail_in_flight = in_flight;
        result
    }

    fn flush_regular_message_for_recipient(&mut self, to_agent: &str) -> Result<bool, ErrorBody> {
        let Ok(resolved) = self.resolve_terminal_target(to_agent) else {
            return Ok(false);
        };
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Ok(false);
        };
        if !regular_mail_can_submit(agent.agent_status) {
            return Ok(false);
        }
        self.flush_regular_messages_for_available_agent(&agent)
    }

    fn flush_job_msg_nudges_for_agent(&mut self, to_agent: &str) -> Result<(), ErrorBody> {
        let store = open_msg_store()?;
        let nudges = store
            .pending_nudges_for_agent(to_agent)
            .map_err(msg_store_error)?;
        drop(store);

        for nudge in nudges
            .into_iter()
            .filter(|nudge| nudge.room == crate::msg::JOBS_ROOM)
        {
            let _ = self.flush_pending_job_msg_nudge(nudge)?;
        }
        Ok(())
    }

    fn flush_job_msg_nudge_for(&mut self, room: &str, to_agent: &str) -> Result<bool, ErrorBody> {
        let store = open_msg_store()?;
        let Some(nudge) = store
            .pending_nudge_for(room, to_agent)
            .map_err(msg_store_error)?
        else {
            return Ok(false);
        };
        drop(store);
        self.flush_pending_job_msg_nudge(nudge)
    }

    fn flush_pending_job_msg_nudge(
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
        let message = messages
            .iter()
            .map(|message| message.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        inject_text_and_enter(runtime, &message)?;
        let store = open_msg_store()?;
        store
            .mark_delivered(&nudge.room, &nudge.to_agent)
            .map_err(msg_store_error)?;
        Ok(true)
    }

    fn resolve_msg_recipient(&self, room: &str, target: &str) -> Result<String, ErrorBody> {
        let agent = self
            .agent_info_for_target(target)
            .map_err(|err| self.agent_target_error_body(err))?;
        let mailbox = mailbox_agent_name(&agent).ok_or_else(|| ErrorBody {
            code: "agent_not_found".into(),
            message: format!("agent target {target} has no reported agent identity"),
        })?;
        if room == crate::msg::JOBS_ROOM {
            Ok(mailbox)
        } else {
            Ok(agent.global_pane_id)
        }
    }

    fn mailbox_recipient_aliases(&self, room: &str, to_agent: &str) -> Vec<String> {
        let Ok(resolved) = self.resolve_terminal_target(to_agent) else {
            return vec![to_agent.to_string()];
        };
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return vec![to_agent.to_string()];
        };

        let mut aliases = vec![agent.global_pane_id, agent.short_pane_id];
        if room == crate::msg::JOBS_ROOM {
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
        }
        if let Some(public_pane_id) = self.public_pane_id(resolved.ws_idx, resolved.pane_id) {
            aliases.push(public_pane_id);
        }
        aliases.sort();
        aliases.dedup();
        aliases
    }

    fn broadcast_recipients(&self, room: &str, from_agent: &str) -> Result<Vec<String>, ErrorBody> {
        let mut recipients = if room == crate::msg::JOBS_ROOM {
            let store = open_msg_store()?;
            let mut recipients = store.participants(room).map_err(msg_store_error)?;
            recipients.extend(
                self.collect_agent_infos()
                    .into_iter()
                    .filter_map(|agent| mailbox_agent_name(&agent)),
            );
            recipients
        } else {
            let store = open_msg_store()?;
            let mut recipients = store.participants(room).map_err(msg_store_error)?;
            recipients.retain(|recipient| is_global_pane_id(recipient));
            recipients.extend(
                self.collect_agent_infos()
                    .into_iter()
                    .map(|agent| agent.global_pane_id),
            );
            recipients
        };
        let from_pane = self
            .agent_info_for_target(from_agent)
            .ok()
            .map(|agent| agent.global_pane_id);
        recipients.sort();
        recipients.dedup();
        recipients.retain(|agent| agent != from_agent && Some(agent) != from_pane.as_ref());
        if recipients.is_empty() {
            return Err(ErrorBody {
                code: "msg_no_recipients".into(),
                message: format!("room {room} has no broadcast recipients"),
            });
        }
        Ok(recipients)
    }
}

fn inject_text_and_enter(
    runtime: &crate::terminal::TerminalRuntime,
    message: &str,
) -> Result<(), ErrorBody> {
    let (text, enter) = encode_text_and_enter(runtime, message)?;
    runtime
        .try_send_bytes(Bytes::from(text))
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

fn inject_regular_message_and_enter(
    completion_tx: &tokio::sync::mpsc::UnboundedSender<crate::events::MailboxWriteCompletion>,
    runtime: &crate::terminal::TerminalRuntime,
    message: &str,
    message_id: i64,
) -> Result<(), ErrorBody> {
    let (mut input, enter) = encode_text_and_enter(runtime, message)?;
    input.extend(enter);
    let completion_tx = completion_tx.clone();
    runtime
        .try_write_mailbox_bytes(
            Bytes::from(input),
            Box::new(move |result| {
                let _ = completion_tx.send(crate::events::MailboxWriteCompletion {
                    message_id,
                    result: result.map_err(|err| err.to_string()),
                });
            }),
        )
        .map_err(|err| ErrorBody {
            code: "msg_nudge_failed".into(),
            message: err.to_string(),
        })?;
    Ok(())
}

fn encode_text_and_enter(
    runtime: &crate::terminal::TerminalRuntime,
    message: &str,
) -> Result<(Vec<u8>, Vec<u8>), ErrorBody> {
    let text = encode_api_text(runtime, message);
    let enter = match encode_api_keys(runtime, &["enter".to_string()]) {
        Ok(mut encoded_keys) => encoded_keys.pop().unwrap_or_default(),
        Err(key) => {
            return Err(ErrorBody {
                code: "invalid_key".into(),
                message: format!("unsupported key {key}"),
            });
        }
    };
    Ok((text, enter))
}

fn deliver_regular_messages(
    completion_tx: &tokio::sync::mpsc::UnboundedSender<crate::events::MailboxWriteCompletion>,
    in_flight: &mut std::collections::HashSet<i64>,
    runtime: &crate::terminal::TerminalRuntime,
    messages: Vec<crate::api::schema::MsgMessage>,
) -> Result<bool, ErrorBody> {
    let mut delivered = false;
    for message in messages {
        if in_flight.contains(&message.id) {
            continue;
        }
        in_flight.insert(message.id);
        if let Err(err) =
            inject_regular_message_and_enter(completion_tx, runtime, &message.body, message.id)
        {
            in_flight.remove(&message.id);
            return Err(err);
        }
        delivered = true;
    }
    Ok(delivered)
}

fn is_global_pane_id(value: &str) -> bool {
    value
        .strip_prefix('p')
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|raw| raw > 0 && value == format!("p{raw}"))
}

fn regular_mail_can_submit(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Idle | AgentStatus::Working | AgentStatus::Done
    )
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
            EmptyParams, ErrorResponse, Method, MsgMessage, PaneAgentState, PaneListParams,
            PaneReportAgentParams, Request, SuccessResponse,
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
        with_msg_api_harness_with_channel_capacity(names, 16, test)
    }

    fn with_msg_api_harness_with_channel_capacity<T>(
        names: &[&str],
        channel_capacity: usize,
        test: impl FnOnce(&mut MsgApiHarness) -> T,
    ) -> T {
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

        let mut harness = MsgApiHarness::new(names, db_path.clone(), channel_capacity);
        let result = test(&mut harness);

        std::env::remove_var(crate::msg::MSG_DB_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    impl MsgApiHarness {
        fn new(names: &[&str], db_path: PathBuf, channel_capacity: usize) -> Self {
            Self::new_with_root_pane_id(names, db_path, channel_capacity, None)
        }

        fn new_with_root_pane_id(
            names: &[&str],
            db_path: PathBuf,
            channel_capacity: usize,
            root_pane_id: Option<PaneId>,
        ) -> Self {
            assert!(!names.is_empty());
            let mut app = test_app();
            let mut ws = root_pane_id.map_or_else(
                || Workspace::test_new("msg-api"),
                |pane_id| Workspace::test_new_with_root_pane_id("msg-api", pane_id),
            );
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

                let (runtime, rx) =
                    TerminalRuntime::test_with_channel_capacity(80, 24, channel_capacity);
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
            self.drain_events();
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
            self.drain_events();
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

        #[cfg(unix)]
        fn replace_runtime_with_socket_pty(
            &mut self,
            name: &str,
        ) -> std::os::unix::net::UnixStream {
            let pane_id = self.agents.get(name).expect("test agent").pane_id;
            let (runtime, peer) = TerminalRuntime::test_with_socket_pty_actor(80, 24);
            self.app.state.insert_test_runtime(pane_id, runtime);
            peer
        }

        fn received_texts(&mut self, name: &str) -> Vec<String> {
            let rx = &mut self.agents.get_mut(name).unwrap().rx;
            let mut texts = Vec::new();
            while let Ok(bytes) = rx.try_recv() {
                texts.push(String::from_utf8_lossy(&bytes).into_owned());
            }
            texts
        }

        fn drain_events(&mut self) {
            while let Ok(completion) = self.app.mailbox_write_rx.try_recv() {
                self.app
                    .handle_mailbox_write_finished(completion.message_id, completion.result);
            }
            while let Ok(event) = self.app.event_rx.try_recv() {
                self.app.handle_internal_event(event);
            }
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
    fn broadcast_submits_immediately_to_working_recipients() {
        with_msg_api_harness(&["alpha", "beta", "gamma"], |harness| {
            let nudged = harness.send("alpha", "*", "broadcast", "hello everyone");
            let expected_recipients = [
                harness.global_pane_id("beta"),
                harness.global_pane_id("gamma"),
            ];

            let messages = harness.history("broadcast");
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.to_agent.as_str())
                    .collect::<Vec<_>>(),
                expected_recipients
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            );
            assert!(messages.iter().all(|message| message.from_agent == "alpha"));
            assert_eq!(nudged, expected_recipients);
            assert_eq!(
                harness.received_texts("beta"),
                vec!["hello everyone\r".to_string()]
            );
            assert_eq!(
                harness.received_texts("gamma"),
                vec!["hello everyone\r".to_string()]
            );
            assert!(messages.iter().all(|message| message.read_at.is_some()));
        });

        with_msg_api_harness(&["alpha"], |harness| {
            let code = harness.send_error_code("alpha", "*", "broadcast-empty");

            assert_eq!(code, "msg_no_recipients");
            assert!(harness.history("broadcast-empty").is_empty());
        });
    }

    #[test]
    fn regular_broadcast_keeps_offline_global_pane_participants_only() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let room = "offline-broadcast";
            let offline_pane_id = "p999999";
            harness.insert_queued_message(room, offline_pane_id, "existing stable recipient");
            harness.insert_queued_message(room, "legacy-beta", "legacy recipient");

            harness.send("alpha", "*", room, "broadcast");

            let recipients = harness
                .history(room)
                .into_iter()
                .filter(|message| message.body == "broadcast")
                .map(|message| message.to_agent)
                .collect::<Vec<_>>();
            let mut expected = vec![harness.global_pane_id("beta"), offline_pane_id.to_string()];
            expected.sort();
            assert_eq!(recipients, expected);
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
    fn global_pane_id_requires_a_positive_canonical_number() {
        assert!(is_global_pane_id("p42"));
        for value in ["p0", "p01", "p_", "beta"] {
            assert!(!is_global_pane_id(value), "{value} must not be canonical");
        }
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
    fn idle_direct_message_submits_its_exact_body_without_an_inbox_command() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let room = "idle direct's inbox";
            let body = "wake up\nwith every line intact\n`do not reinterpret`";

            let nudged = harness.send("alpha", "beta", room, body);

            assert_eq!(nudged, vec![harness.global_pane_id("beta")]);
            let texts = harness.received_texts("beta");
            assert_eq!(texts, vec![format!("{body}\r")]);
            assert!(!texts[0].contains("herdr msg inbox"));
            let queued = harness.history(room);
            assert_eq!(queued.len(), 1);
            assert!(queued[0].delivered_at.is_some());
            assert!(queued[0].read_at.is_some());

            harness.report_state("beta", PaneAgentState::Working);
            harness.report_state("beta", PaneAgentState::Idle);

            assert!(harness.received_texts("beta").is_empty());
            assert!(harness.inbox("beta", room).is_empty());
        });
    }

    #[test]
    fn regular_message_batch_submits_every_body_in_creation_order() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let room = "review ui's notifications";
            for index in 0..6 {
                harness.send("alpha", "beta", room, &format!("wake up {index}"));
            }
            assert!(harness.received_texts("beta").is_empty());
            let queued = harness.history(room);
            assert!(queued.iter().all(|message| message.delivered_at.is_none()));
            assert!(queued.iter().all(|message| message.read_at.is_none()));

            harness.report_state("beta", PaneAgentState::Idle);

            let texts = harness.received_texts("beta");
            assert_eq!(
                texts,
                (0..6)
                    .map(|index| format!("wake up {index}\r"))
                    .collect::<Vec<_>>()
            );
            let queued = harness.history(room);
            assert!(queued.iter().all(|message| message.delivered_at.is_some()));
            assert!(queued.iter().all(|message| message.read_at.is_some()));
            assert!(harness.inbox("beta", room).is_empty());
        });
    }

    #[test]
    fn idle_send_selects_oldest_regular_message_across_rooms() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message("older-room", &pane_id, "older");

            harness.send("alpha", &pane_id, "newer-room", "newer");

            assert_eq!(
                harness.received_texts("beta"),
                vec!["older\r".to_string(), "newer\r".to_string()]
            );
            let older = harness.history("older-room");
            assert!(older[0].delivered_at.is_some());
            let newer = harness.history("newer-room");
            assert!(newer[0].delivered_at.is_some());
        });
    }

    #[test]
    fn agent_and_pane_list_do_not_flush_queued_regular_messages() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message("list-does-not-flush", &pane_id, "must stay queued");

            for _ in 0..2 {
                harness.app.handle_api_request(Request {
                    id: "agent-list".into(),
                    method: Method::AgentList(EmptyParams {}),
                });
                harness.app.handle_api_request(Request {
                    id: "pane-list".into(),
                    method: Method::PaneList(PaneListParams::default()),
                });
            }

            assert!(harness.received_texts("beta").is_empty());
            let messages = harness.history("list-does-not-flush");
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_none());
        });
    }

    #[test]
    fn presentation_only_idle_update_does_not_flush_regular_messages() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let room = "presentation-only";
            let pane_id = harness.agents.get("beta").unwrap().pane_id;
            let recipient = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &recipient, "must stay queued");

            harness
                .app
                .emit_pane_state_update(&crate::app::actions::PaneStateUpdate {
                    pane_id,
                    ws_idx: 0,
                    previous_agent_label: Some("beta".into()),
                    previous_known_agent: None,
                    previous_state: crate::detect::AgentState::Idle,
                    previous_seen: true,
                    previous_presentation: crate::terminal::EffectivePresentation {
                        title: None,
                        display_agent: None,
                        state_labels: HashMap::new(),
                    },
                    agent_label: Some("beta".into()),
                    known_agent: None,
                    state: crate::detect::AgentState::Idle,
                    seen: true,
                    presentation: crate::terminal::EffectivePresentation {
                        title: Some("presentation changed".into()),
                        display_agent: None,
                        state_labels: HashMap::new(),
                    },
                });

            assert!(harness.received_texts("beta").is_empty());
            let messages = harness.history(room);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_none());
        });
    }

    #[test]
    fn presentation_only_working_update_does_not_flush_regular_messages() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let room = "presentation-only-working";
            let pane_id = harness.agents.get("beta").unwrap().pane_id;
            let recipient = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &recipient, "must stay queued");

            harness
                .app
                .emit_pane_state_update(&crate::app::actions::PaneStateUpdate {
                    pane_id,
                    ws_idx: 0,
                    previous_agent_label: Some("beta".into()),
                    previous_known_agent: None,
                    previous_state: crate::detect::AgentState::Working,
                    previous_seen: true,
                    previous_presentation: crate::terminal::EffectivePresentation {
                        title: None,
                        display_agent: None,
                        state_labels: HashMap::new(),
                    },
                    agent_label: Some("beta".into()),
                    known_agent: None,
                    state: crate::detect::AgentState::Working,
                    seen: true,
                    presentation: crate::terminal::EffectivePresentation {
                        title: Some("presentation changed".into()),
                        display_agent: None,
                        state_labels: HashMap::new(),
                    },
                });

            assert!(harness.received_texts("beta").is_empty());
            let messages = harness.history(room);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_none());
        });
    }

    #[test]
    fn regular_message_delivery_submits_body_and_enter_atomically() {
        with_msg_api_harness_with_channel_capacity(&["alpha", "beta"], 1, |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let pane_id = harness.global_pane_id("beta");

            let nudged = harness.send("alpha", &pane_id, "atomic-delivery", "complete body");

            assert_eq!(nudged, vec![pane_id]);
            assert_eq!(
                harness.received_texts("beta"),
                vec!["complete body\r".to_string()]
            );
            let messages = harness.history("atomic-delivery");
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_some());
            assert!(messages[0].read_at.is_some());
        });
    }

    #[test]
    fn backpressured_regular_delivery_does_not_block_state_report_or_mark_second_message() {
        with_msg_api_harness_with_channel_capacity(&["alpha", "beta"], 1, |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message("backpressure", &pane_id, "first");
            harness.insert_queued_message("backpressure", &pane_id, "second");
            harness.report_state("beta", PaneAgentState::Idle);
            assert_eq!(harness.received_texts("beta"), vec!["first\r".to_string()]);
            let messages = harness.history("backpressure");
            assert!(messages[0].delivered_at.is_some());
            assert!(messages[1].delivered_at.is_none());
        });
    }

    #[test]
    fn full_shared_event_channel_does_not_block_mailbox_completion() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let pane_id = harness.global_pane_id("beta");
            while harness
                .app
                .event_tx
                .try_send(crate::events::AppEvent::PrefixInputSource { active: false })
                .is_ok()
            {}
            assert!(matches!(
                harness
                    .app
                    .event_tx
                    .try_send(crate::events::AppEvent::PrefixInputSource { active: false }),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_))
            ));

            let nudged = harness.send("alpha", "beta", "event-capacity", "body");

            assert_eq!(nudged, vec![pane_id]);
            let message = harness.history("event-capacity").pop().unwrap();
            assert!(message.delivered_at.is_some());
            assert!(message.read_at.is_some());
        });
    }

    #[test]
    fn in_flight_regular_message_is_not_resubmitted_on_repeat_flush() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message("in-flight-repeat", &pane_id, "hold");
            let message_id = harness.history("in-flight-repeat")[0].id;
            harness.app.regular_mail_in_flight.insert(message_id);
            harness.report_state("beta", PaneAgentState::Idle);
            harness.app.flush_msg_nudges_for_all_available_agents();
            assert!(harness.received_texts("beta").is_empty());
            assert!(harness.history("in-flight-repeat")[0]
                .delivered_at
                .is_none());
        });
    }

    #[test]
    fn inbox_excludes_in_flight_regular_message_without_marking_it_read() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message("in-flight-inbox", &pane_id, "hold");
            let message_id = harness.history("in-flight-inbox")[0].id;
            harness.app.regular_mail_in_flight.insert(message_id);
            assert!(harness.inbox("beta", "in-flight-inbox").is_empty());
            let message = harness.history("in-flight-inbox").pop().unwrap();
            assert!(message.delivered_at.is_none());
            assert!(message.read_at.is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn failed_pty_write_keeps_regular_message_queued() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let pane_id = harness.global_pane_id("beta");
            let peer = harness.replace_runtime_with_socket_pty("beta");

            let body = "x".repeat(4 * 1024 * 1024);
            let nudged = harness.send("alpha", "beta", "closed-pty", &body);
            assert_eq!(nudged, vec![pane_id]);
            drop(peer);
            let completion = tokio::runtime::Handle::current()
                .block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    harness.app.mailbox_write_rx.recv(),
                ))
                .expect("closed peer completion timeout")
                .expect("closed peer completes the in-flight write");
            harness
                .app
                .handle_mailbox_write_finished(completion.message_id, completion.result);

            let messages = harness.history("closed-pty");
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_none());
            assert!(messages[0].read_at.is_none());
        });
    }

    #[test]
    fn idle_transition_submits_regular_messages_in_global_creation_order() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            harness.send("alpha", "beta", "z-last-room", "first");
            harness.send("alpha", "beta", "a-first-room", "second");

            harness.report_state("beta", PaneAgentState::Idle);

            assert_eq!(
                harness.received_texts("beta"),
                vec!["first\r".to_string(), "second\r".to_string()]
            );
            let first = harness.history("z-last-room");
            assert_eq!(first.len(), 1);
            assert!(first[0].delivered_at.is_some());
            assert!(first[0].read_at.is_some());
            let second = harness.history("a-first-room");
            assert_eq!(second.len(), 1);
            assert!(second[0].delivered_at.is_some());
            assert!(second[0].read_at.is_some());
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
    fn herdr_job_messages_are_injected_while_recipient_is_working() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Working);
            let body = "[herdr run] exit=0 label=tests job=job-1 details: herdr log job-1";

            let nudged = harness.send("herdr-run", "beta", crate::msg::JOBS_ROOM, body);

            assert_eq!(nudged, vec!["beta"]);
            assert_eq!(
                harness.received_texts("beta"),
                vec![body.to_string(), "\r".to_string()]
            );
            assert!(harness.history(crate::msg::JOBS_ROOM)[0].read_at.is_some());
        });
    }

    #[test]
    fn startup_flush_delivers_queued_job_message_to_named_agent() {
        with_msg_api_harness(&["alpha", "renamed-caller"], |harness| {
            harness.report_state("renamed-caller", PaneAgentState::Blocked);
            let body = "[herdr run] exit=0 label=tests job=job-1 details: herdr log job-1";
            harness.insert_queued_message(crate::msg::JOBS_ROOM, "renamed-caller", body);

            let db_path = harness.db_path.clone();
            let mut restarted = MsgApiHarness::new(&["alpha", "renamed-caller"], db_path, 16);
            restarted.report_state("renamed-caller", PaneAgentState::Idle);
            restarted.app.flush_msg_nudges_for_all_available_agents();

            assert_eq!(
                restarted.received_texts("renamed-caller"),
                vec![body.to_string(), "\r".to_string()]
            );
            let messages = restarted.history(crate::msg::JOBS_ROOM);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].delivered_at.is_some());
            assert!(messages[0].read_at.is_some());
        });
    }

    #[test]
    fn working_send_submits_immediately_and_idle_does_not_duplicate() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Working);
            let pane_id = harness.global_pane_id("beta");
            let nudged = harness.send("alpha", &pane_id, "status-flush", "one");

            assert_eq!(nudged, vec![pane_id]);
            assert_eq!(harness.received_texts("beta"), vec!["one\r".to_string()]);
            let messages = harness.history("status-flush");
            assert_eq!(messages.len(), 1);
            assert!(messages
                .iter()
                .all(|message| message.delivered_at.is_some()));
            assert!(messages.iter().all(|message| message.read_at.is_some()));

            harness.report_state("beta", PaneAgentState::Idle);

            assert!(harness.received_texts("beta").is_empty());
            assert!(harness.inbox("beta", "status-flush").is_empty());
        });
    }

    #[test]
    fn working_recipient_receives_regular_message_as_immediate_steering() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Working);
            let pane_id = harness.global_pane_id("beta");

            let nudged = harness.send("alpha", &pane_id, "working-steering", "tell me now");

            assert_eq!(nudged, vec![pane_id]);
            assert_eq!(
                harness.received_texts("beta"),
                vec!["tell me now\r".to_string()]
            );
            assert!(harness.history("working-steering")[0].read_at.is_some());
        });
    }

    #[test]
    fn unknown_then_idle_submits_queued_regular_message() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Unknown);
            let pane_id = harness.global_pane_id("beta");
            let nudged = harness.send("alpha", &pane_id, "unknown-to-idle", "queued");
            assert!(nudged.is_empty());
            assert!(harness.received_texts("beta").is_empty());

            harness.report_state("beta", PaneAgentState::Idle);

            assert_eq!(harness.received_texts("beta"), vec!["queued\r".to_string()]);
            assert!(harness.history("unknown-to-idle")[0].read_at.is_some());
        });
    }

    #[test]
    fn blocked_message_submits_when_recipient_returns_to_working() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let pane_id = harness.global_pane_id("beta");

            let nudged = harness.send("alpha", &pane_id, "blocked-to-working", "continue now");
            assert!(nudged.is_empty());
            assert!(harness.received_texts("beta").is_empty());

            harness.report_state("beta", PaneAgentState::Working);

            assert_eq!(
                harness.received_texts("beta"),
                vec!["continue now\r".to_string()]
            );
            assert!(harness.history("blocked-to-working")[0].read_at.is_some());
        });
    }

    #[test]
    fn repeated_working_report_retries_regular_message_after_runtime_is_ready() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let room = "runtime-ready";
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &pane_id, "runtime is ready");

            harness.report_state("beta", PaneAgentState::Working);

            assert_eq!(
                harness.received_texts("beta"),
                vec!["runtime is ready\r".to_string()]
            );
            assert!(harness.history(room)[0].read_at.is_some());
        });
    }

    #[test]
    fn done_recipient_receives_regular_message_immediately() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Idle);
            let room = "done-immediate";
            let beta_internal_pane_id = harness.agents.get("beta").unwrap().pane_id;
            let beta_recipient = harness.global_pane_id("beta");
            harness.app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&beta_internal_pane_id)
                .unwrap()
                .seen = false;

            let nudged = harness.send("alpha", &beta_recipient, room, "new turn now");

            assert_eq!(nudged, vec![beta_recipient]);
            assert_eq!(
                harness.received_texts("beta"),
                vec!["new turn now\r".to_string()]
            );
            let messages = harness.history(room);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].read_at.is_some());
        });
    }

    #[test]
    fn blocked_message_submits_when_recipient_transitions_to_done() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            harness.report_state("beta", PaneAgentState::Blocked);
            let room = "blocked-to-done";
            let internal_pane_id = harness.agents.get("beta").unwrap().pane_id;
            let recipient = harness.global_pane_id("beta");
            let nudged = harness.send("alpha", &recipient, room, "new turn from done");
            assert!(nudged.is_empty());
            harness.app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&internal_pane_id)
                .unwrap()
                .seen = false;
            harness.app.state.outer_terminal_focus = Some(false);

            harness.report_state("beta", PaneAgentState::Idle);

            assert_eq!(
                harness
                    .app
                    .agent_info(0, internal_pane_id)
                    .unwrap()
                    .agent_status,
                AgentStatus::Done
            );
            assert_eq!(
                harness.received_texts("beta"),
                vec!["new turn from done\r".to_string()]
            );
            assert!(harness.history(room)[0].read_at.is_some());
        });
    }

    #[test]
    fn restarted_same_name_does_not_nudge_or_read_old_regular_mail() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let room = "restart-flush";
            let old_pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &old_pane_id, "old pane");
            harness.insert_queued_message(room, "beta", "old name");

            let db_path = harness.db_path.clone();
            let mut restarted = MsgApiHarness::new(&["alpha", "beta"], db_path, 16);
            assert_ne!(restarted.global_pane_id("beta"), old_pane_id);
            restarted.report_state("beta", PaneAgentState::Idle);
            restarted.app.flush_msg_nudges_for_all_available_agents();

            assert!(restarted.received_texts("beta").is_empty());
            assert!(restarted.inbox("beta", room).is_empty());
            let messages = restarted.history(room);
            assert_eq!(messages.len(), 2);
            assert!(messages
                .iter()
                .all(|message| message.delivered_at.is_none()));
            assert!(messages.iter().all(|message| message.read_at.is_none()));
        });
    }

    #[test]
    fn startup_flush_submits_current_working_regular_mail_once() {
        with_msg_api_harness(&["alpha", "beta"], |harness| {
            let room = "startup-flush";
            let pane_id = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &pane_id, "one");

            harness.app.flush_msg_nudges_for_all_available_agents();
            harness.drain_events();

            assert_eq!(harness.received_texts("beta"), vec!["one\r".to_string()]);
            let reopened = crate::msg::MsgStore::open_at(harness.db_path.clone()).unwrap();
            assert!(reopened
                .pending_nudges_for_agent(&pane_id)
                .unwrap()
                .is_empty());
            assert!(harness.history(room)[0].read_at.is_some());
            harness.app.flush_msg_nudges_for_all_available_agents();

            assert!(harness.received_texts("beta").is_empty());
        });
    }

    #[test]
    fn reopened_app_with_same_stable_pane_id_delivers_regular_mail_once() {
        with_msg_api_harness(&["beta"], |harness| {
            let room = "restart-same-stable-pane";
            let stable_pane_id = harness.agents.get("beta").unwrap().pane_id;
            let recipient = harness.global_pane_id("beta");
            harness.insert_queued_message(room, &recipient, "persisted");
            let db_path = harness.db_path.clone();

            let mut restarted = MsgApiHarness::new_with_root_pane_id(
                &["beta"],
                db_path.clone(),
                16,
                Some(stable_pane_id),
            );
            assert_eq!(restarted.global_pane_id("beta"), recipient);
            restarted.report_state("beta", PaneAgentState::Working);
            restarted.app.flush_msg_nudges_for_all_available_agents();
            assert_eq!(
                restarted.received_texts("beta"),
                vec!["persisted\r".to_string()]
            );
            assert!(restarted.history(room)[0].read_at.is_some());

            let mut reopened =
                MsgApiHarness::new_with_root_pane_id(&["beta"], db_path, 16, Some(stable_pane_id));
            assert_eq!(reopened.global_pane_id("beta"), recipient);
            reopened.report_state("beta", PaneAgentState::Idle);
            reopened.app.flush_msg_nudges_for_all_available_agents();
            assert!(reopened.received_texts("beta").is_empty());
        });
    }
}
