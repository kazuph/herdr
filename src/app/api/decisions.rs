use std::time::{Duration, Instant};

use tracing::warn;

use super::responses;
use crate::api::schema::decisions::{
    Decision, DecisionAnswerParams, DecisionCancelParams, DecisionCreateParams, DecisionGetParams,
    DecisionListParams, DecisionStatus,
};
use crate::api::schema::{EventData, EventEnvelope, EventKind, ResponseResult};
use crate::app::App;
use crate::decision::{new_decision_id, unix_ms_now, AnswerOutcome, DecisionStore, ResolveOutcome};

const DECISION_DEADLINE_RETRY_INTERVAL: Duration = Duration::from_secs(30);

impl App {
    /// Fill in the workspace/tab/pane labels for a decision origin whose
    /// `pane_id` resolves to a local pane. Labels already supplied by the
    /// caller are kept; unresolvable panes are left untouched.
    pub(crate) fn enrich_decision_origin(&self, origin: &mut crate::api::schema::DecisionOrigin) {
        let Some(public_id) = origin.pane_id.clone() else {
            return;
        };
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&public_id) else {
            return;
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) else {
            return;
        };
        if origin.workspace_label.is_none() {
            origin.workspace_label =
                Some(ws.display_name_from(&self.state.terminals, &self.terminal_runtimes));
        }
        if origin.tab_label.is_none() {
            origin.tab_label = ws.tab_display_name(tab_idx);
        }
        if origin.pane_label.is_none() {
            origin.pane_label = self
                .pane_info(ws_idx, pane_id)
                .and_then(|pane| pane.label.or(pane.terminal_title));
        }
    }

    pub(crate) fn handle_decision_create(
        &mut self,
        id: String,
        params: DecisionCreateParams,
    ) -> String {
        if let Err(message) = validate_decision_create(&params) {
            return responses::encode_error(id, "invalid_request", message);
        }
        let now = unix_ms_now();
        let mut origin = params
            .origin
            .filter(|origin| *origin != crate::api::schema::DecisionOrigin::default());
        if let Some(origin) = origin.as_mut() {
            self.enrich_decision_origin(origin);
        }
        let decision = Decision {
            decision_id: new_decision_id(),
            kind: params.kind,
            title: params.title,
            body: params.body.filter(|body| !body.is_empty()),
            options: params.options,
            allow_text: params.allow_text,
            multi_select: params.multi_select,
            question_label: None,
            origin,
            created_unix_ms: now,
            expires_unix_ms: params.timeout_ms.map(|timeout| now.saturating_add(timeout)),
            status: DecisionStatus::Pending,
            answer: None,
        };
        let mut store = match DecisionStore::open_active() {
            Ok(store) => store,
            Err(err) => {
                return responses::encode_error(
                    id,
                    "decision_store_error",
                    format!("failed to open decision store: {err}"),
                );
            }
        };
        if let Err(err) = store.insert(&decision) {
            return responses::encode_error(id, "decision_store_error", err.to_string());
        }
        self.sync_decision_expiry_deadline(Instant::now());
        self.emit_event(EventEnvelope {
            event: EventKind::DecisionCreated,
            data: EventData::DecisionCreated {
                decision: decision.clone(),
            },
        });
        responses::encode_success(id, ResponseResult::Decision { decision })
    }

    pub(crate) fn handle_decision_get(&mut self, id: String, params: DecisionGetParams) -> String {
        match self.open_decision_store(&id) {
            Err(response) => response,
            Ok(store) => match store.get(&params.decision_id) {
                Ok(Some(decision)) => {
                    responses::encode_success(id, ResponseResult::Decision { decision })
                }
                Ok(None) => responses::encode_error(
                    id,
                    "decision_not_found",
                    format!("decision not found: {}", params.decision_id),
                ),
                Err(err) => responses::encode_error(id, "decision_store_error", err.to_string()),
            },
        }
    }

    pub(crate) fn handle_decision_list(
        &mut self,
        id: String,
        params: DecisionListParams,
    ) -> String {
        match self.open_decision_store(&id) {
            Err(response) => response,
            Ok(store) => match store.list(params.status) {
                Ok(decisions) => {
                    responses::encode_success(id, ResponseResult::DecisionList { decisions })
                }
                Err(err) => responses::encode_error(id, "decision_store_error", err.to_string()),
            },
        }
    }

    pub(crate) fn handle_decision_answer(
        &mut self,
        id: String,
        params: DecisionAnswerParams,
    ) -> String {
        let mut store = match self.open_decision_store(&id) {
            Err(response) => return response,
            Ok(store) => store,
        };
        // Screen-prompt decisions carry a freshness gate: answering a stale
        // one would type its keys into whatever prompt (or input line) the
        // pane shows now, so cancel the decision instead of committing the
        // answer.
        if let Ok(Some(decision)) = store.get(&params.decision_id) {
            if decision.status == DecisionStatus::Pending
                && Self::is_screen_prompt_decision(&decision)
                && !self.screen_prompt_answer_is_fresh(&decision)
            {
                if let Ok(ResolveOutcome::Resolved(cancelled)) = store.cancel(&params.decision_id) {
                    let cancelled = *cancelled;
                    self.screen_prompt_resolved(&cancelled);
                    self.emit_decision_resolved(cancelled);
                }
                return responses::encode_error(
                    id,
                    "decision_stale",
                    format!(
                        "screen prompt for decision {} changed or disappeared",
                        params.decision_id
                    ),
                );
            }
            // A person typing into the pane takes precedence over a queued
            // screen-prompt answer: refuse to commit so the answer stays
            // pending instead of keys silently landing (or getting lost)
            // under active operator input.
            if decision.status == DecisionStatus::Pending
                && Self::is_screen_prompt_decision(&decision)
                && self.screen_prompt_pane_has_recent_user_input(&decision)
            {
                return responses::encode_error(
                    id,
                    "pane_user_active",
                    format!(
                        "pane for decision {} received operator input moments ago; answer not committed",
                        params.decision_id
                    ),
                );
            }
        }
        match store.answer(
            &params.decision_id,
            params.option_id,
            params.option_ids,
            params.text,
            params.responder,
            unix_ms_now(),
        ) {
            Ok(AnswerOutcome::Answered(decision)) => {
                let decision = *decision;
                self.emit_decision_resolved(decision.clone());
                self.screen_prompt_answer(&decision);
                responses::encode_success(id, ResponseResult::Decision { decision })
            }
            Ok(AnswerOutcome::NotFound) => responses::encode_error(
                id,
                "decision_not_found",
                format!("decision not found: {}", params.decision_id),
            ),
            Ok(AnswerOutcome::AlreadyResolved) => responses::encode_error(
                id,
                "decision_already_resolved",
                format!("decision is already resolved: {}", params.decision_id),
            ),
            Ok(AnswerOutcome::UnknownOption) => responses::encode_error(
                id,
                "decision_unknown_option",
                format!("unknown option for decision {}", params.decision_id),
            ),
            Ok(AnswerOutcome::TextNotAllowed) => responses::encode_error(
                id,
                "decision_text_not_allowed",
                format!("decision {} does not allow free text", params.decision_id),
            ),
            Ok(AnswerOutcome::MissingAnswer) => responses::encode_error(
                id,
                "invalid_request",
                "decision answer requires option_id or text",
            ),
            Err(err) => responses::encode_error(id, "decision_store_error", err.to_string()),
        }
    }

    pub(crate) fn handle_decision_cancel(
        &mut self,
        id: String,
        params: DecisionCancelParams,
    ) -> String {
        let mut store = match self.open_decision_store(&id) {
            Err(response) => return response,
            Ok(store) => store,
        };
        match store.cancel(&params.decision_id) {
            Ok(ResolveOutcome::Resolved(decision)) => {
                let decision = *decision;
                self.emit_decision_resolved(decision.clone());
                self.screen_prompt_resolved(&decision);
                responses::encode_success(id, ResponseResult::Decision { decision })
            }
            Ok(ResolveOutcome::NotFound) => responses::encode_error(
                id,
                "decision_not_found",
                format!("decision not found: {}", params.decision_id),
            ),
            Ok(ResolveOutcome::AlreadyResolved) => responses::encode_error(
                id,
                "decision_already_resolved",
                format!("decision is already resolved: {}", params.decision_id),
            ),
            Err(err) => responses::encode_error(id, "decision_store_error", err.to_string()),
        }
    }

    /// Server-owned decision expiry. The deadline is an `Instant` cache of the
    /// earliest pending `expires_unix_ms`; when it passes the store marks due
    /// rows expired and each one emits `decision.resolved`, which wakes
    /// `decision.wait` pollers through the event hub.
    pub(crate) fn expire_due_decisions(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.decision_expiry_deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.expire_due_decisions_at(now)
    }

    fn expire_due_decisions_at(&mut self, now: Instant) -> bool {
        let expired = match DecisionStore::open_active() {
            Ok(mut store) => match store.expire_pending(unix_ms_now()) {
                Ok(expired) => expired,
                Err(err) => {
                    warn!(error = %err, "failed to expire pending decisions");
                    Vec::new()
                }
            },
            Err(err) => {
                warn!(error = %err, "failed to open decision store for expiry");
                Vec::new()
            }
        };
        let expired_any = !expired.is_empty();
        for decision in expired {
            self.screen_prompt_resolved(&decision);
            self.emit_decision_resolved(decision);
        }
        self.sync_decision_expiry_deadline(now);
        expired_any
    }

    fn sync_decision_expiry_deadline(&mut self, now: Instant) {
        match DecisionStore::open_active() {
            Ok(store) => match store.next_pending_expiry_unix_ms() {
                Ok(Some(expires_unix_ms)) => {
                    let delay = expires_unix_ms.saturating_sub(unix_ms_now());
                    self.decision_expiry_deadline = Some(now + Duration::from_millis(delay));
                }
                Ok(None) => {
                    self.decision_expiry_deadline = None;
                }
                Err(err) => {
                    warn!(error = %err, "failed to read decision expiry deadline");
                    self.decision_expiry_deadline = Some(now + DECISION_DEADLINE_RETRY_INTERVAL);
                }
            },
            Err(err) => {
                warn!(error = %err, "failed to open decision store for deadline sync");
                self.decision_expiry_deadline = Some(now + DECISION_DEADLINE_RETRY_INTERVAL);
            }
        }
    }

    pub(crate) fn emit_decision_resolved(&mut self, decision: Decision) {
        self.emit_event(EventEnvelope {
            event: EventKind::DecisionResolved,
            data: EventData::DecisionResolved { decision },
        });
    }

    /// Re-assert pending-decision attention on local panes after any decision
    /// lifecycle event (create, answer, cancel, expiry) and at startup. A
    /// pending decision whose `origin.pane_id` resolves to a local pane marks
    /// that pane's terminal so it presents as waiting-for-decision (blocked)
    /// until no pending decision names it anymore; the pane then falls back
    /// to its screen/hook-derived state. Remote decisions never reach the
    /// local store, so their origins never resolve to local panes.
    pub(crate) fn sync_pending_decision_attention(&mut self) {
        let pending_panes = match DecisionStore::open_active() {
            Ok(store) => match store.pending_origin_pane_ids() {
                Ok(pane_ids) => pane_ids,
                Err(err) => {
                    warn!(error = %err, "failed to read pending decisions for pane attention");
                    Vec::new()
                }
            },
            Err(err) => {
                warn!(error = %err, "failed to open decision store for pane attention");
                Vec::new()
            }
        };
        let pending_panes: std::collections::HashSet<crate::layout::PaneId> = pending_panes
            .iter()
            .filter_map(|public_id| {
                self.parse_pane_id(public_id)
                    .map(|(_ws_idx, pane_id)| pane_id)
            })
            .collect();
        for update in self.state.apply_pending_decision_attention(&pending_panes) {
            self.emit_pane_state_update(&update);
        }
    }

    fn open_decision_store(&self, id: &str) -> Result<DecisionStore, String> {
        DecisionStore::open_active().map_err(|err| {
            responses::encode_error(
                id.to_string(),
                "decision_store_error",
                format!("failed to open decision store: {err}"),
            )
        })
    }
}

fn validate_decision_create(params: &DecisionCreateParams) -> Result<(), &'static str> {
    if params.title.trim().is_empty() {
        return Err("decision title must not be empty");
    }
    if params.options.is_empty() {
        return Err("decision requires at least one option");
    }
    let mut seen = std::collections::HashSet::with_capacity(params.options.len());
    for option in &params.options {
        if option.id.trim().is_empty() {
            return Err("decision option ids must not be empty");
        }
        if !seen.insert(option.id.as_str()) {
            return Err("decision option ids must be unique");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::decisions::{DecisionKind, DecisionOption, DecisionOptionRole};

    fn create_params(title: &str, options: Vec<DecisionOption>) -> DecisionCreateParams {
        DecisionCreateParams {
            kind: DecisionKind::Ask,
            title: title.into(),
            body: None,
            options,
            allow_text: false,
            multi_select: false,
            timeout_ms: None,
            origin: None,
        }
    }

    fn option(id: &str) -> DecisionOption {
        DecisionOption {
            id: id.into(),
            label: format!("label {id}"),
            role: DecisionOptionRole::Other,
            description: None,
            preview: None,
            preview_unavailable: false,
        }
    }

    #[test]
    fn create_requires_a_title_and_one_option() {
        assert!(validate_decision_create(&create_params("  ", vec![option("a")])).is_err());
        assert!(validate_decision_create(&create_params("title", vec![])).is_err());
        assert!(validate_decision_create(&create_params("title", vec![option("a")])).is_ok());
    }

    #[test]
    fn create_rejects_empty_and_duplicate_option_ids() {
        assert!(
            validate_decision_create(&create_params("t", vec![option(""), option("b")])).is_err()
        );
        assert!(
            validate_decision_create(&create_params("t", vec![option("a"), option("a")])).is_err()
        );
        assert!(
            validate_decision_create(&create_params("t", vec![option("a"), option("b")])).is_ok()
        );
    }

    #[test]
    fn expire_due_decisions_reports_change_only_when_rows_expire() {
        let config_home =
            std::env::temp_dir().join(format!("herdr-decision-expiry-test-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var(
            crate::session::SESSION_ENV_VAR,
            format!("decision-expiry-{}", std::process::id()),
        );

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.decision_expiry_deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(!app.expire_due_decisions(Instant::now()));
        assert!(app.decision_expiry_deadline.is_none());

        let mut store = DecisionStore::open_active().unwrap();
        store
            .insert(&Decision {
                decision_id: "dec-expiry-test".into(),
                kind: DecisionKind::Ask,
                title: "title".into(),
                body: None,
                options: vec![option("ok")],
                allow_text: false,
                multi_select: false,
                question_label: None,
                origin: None,
                created_unix_ms: 1,
                expires_unix_ms: Some(1),
                status: DecisionStatus::Pending,
                answer: None,
            })
            .unwrap();
        drop(store);

        app.decision_expiry_deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(app.expire_due_decisions(Instant::now()));
        let store = DecisionStore::open_active().unwrap();
        assert_eq!(
            store.get("dec-expiry-test").unwrap().unwrap().status,
            DecisionStatus::Expired
        );

        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&config_home);
    }

    fn app_with_pane(tag: &str) -> (App, crate::layout::PaneId) {
        let dir =
            std::env::temp_dir().join(format!("herdr-origin-labels-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::env::set_var(crate::dispatch::HERDR_DB_PATH_ENV_VAR, dir.join("herdr.db"));
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("ws-name");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces.push(workspace);
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        (app, pane_id)
    }

    #[test]
    fn enrich_decision_origin_fills_workspace_tab_and_pane_labels() {
        let (mut app, pane_id) = app_with_pane("fill");
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .manual_label = Some("my-pane".into());

        let public = app.public_pane_id(0, pane_id).expect("public id");
        let mut origin = crate::api::schema::DecisionOrigin {
            pane_id: Some(public),
            ..Default::default()
        };
        app.enrich_decision_origin(&mut origin);
        assert_eq!(origin.workspace_label.as_deref(), Some("ws-name"));
        assert_eq!(origin.tab_label.as_deref(), Some("1"));
        assert_eq!(origin.pane_label.as_deref(), Some("my-pane"));

        // Caller-supplied labels are kept.
        let mut origin = crate::api::schema::DecisionOrigin {
            pane_id: Some(app.public_pane_id(0, pane_id).unwrap()),
            workspace_label: Some("given".into()),
            ..Default::default()
        };
        app.enrich_decision_origin(&mut origin);
        assert_eq!(origin.workspace_label.as_deref(), Some("given"));
    }

    #[test]
    fn enrich_decision_origin_leaves_unresolvable_panes_untouched() {
        let (app, _pane_id) = app_with_pane("unresolved");
        let mut origin = crate::api::schema::DecisionOrigin {
            pane_id: Some("p999999".into()),
            ..Default::default()
        };
        app.enrich_decision_origin(&mut origin);
        assert!(origin.workspace_label.is_none());
        assert!(origin.tab_label.is_none());
        assert!(origin.pane_label.is_none());

        let mut origin = crate::api::schema::DecisionOrigin::default();
        app.enrich_decision_origin(&mut origin);
        assert!(origin.workspace_label.is_none());
    }

    fn terminal_id_of(app: &App, pane_id: crate::layout::PaneId) -> crate::terminal::TerminalId {
        app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone()
    }

    fn create_decision_for_pane(app: &mut App, public_pane_id: String) -> Decision {
        let response = app.handle_decision_create(
            "req-create".into(),
            DecisionCreateParams {
                kind: DecisionKind::Ask,
                title: "continue?".into(),
                body: None,
                options: vec![option("yes"), option("no")],
                allow_text: false,
                multi_select: false,
                timeout_ms: None,
                origin: Some(crate::api::schema::DecisionOrigin {
                    pane_id: Some(public_pane_id),
                    ..Default::default()
                }),
            },
        );
        let success: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("decision create succeeds");
        let ResponseResult::Decision { decision } = success.result else {
            panic!("expected decision create result");
        };
        decision
    }

    #[test]
    fn pending_decision_marks_origin_pane_blocked_until_answered() {
        let (mut app, pane_id) = app_with_pane("attention");
        let public = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = terminal_id_of(&app, pane_id);

        let decision = create_decision_for_pane(&mut app, public);

        assert!(app.state.terminals[&terminal_id].decision_attention());
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Blocked
        );
        // The public projection rides the same status AskUserQuestion
        // produces: pane, tab, workspace and agent entries all show blocked
        // to TUI and saved-machine endpoint consumers alike.
        assert_eq!(
            app.pane_info(0, pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
        assert_eq!(
            app.workspace_info(0).agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
        assert_eq!(
            app.tab_info(0, 0).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );

        let response = app.handle_decision_answer(
            "req-answer".into(),
            DecisionAnswerParams {
                decision_id: decision.decision_id.clone(),
                option_id: Some("yes".into()),
                option_ids: None,
                text: None,
                responder: Some("test".into()),
            },
        );
        let success: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("decision answer succeeds");
        assert!(matches!(success.result, ResponseResult::Decision { .. }));

        assert!(!app.state.terminals[&terminal_id].decision_attention());
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Unknown
        );
        assert_eq!(
            app.pane_info(0, pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
    }

    #[test]
    fn pending_decision_attention_survives_other_panes_and_cancels() {
        let (mut app, pane_id) = app_with_pane("two-panes");
        app.state.workspaces[0].test_split_pane(
            pane_id,
            ratatui::layout::Direction::Horizontal,
            false,
        );
        let other_pane = *app.state.workspaces[0].tabs[0]
            .panes
            .keys()
            .find(|id| **id != pane_id)
            .expect("second pane");
        app.state.ensure_test_terminals();
        let public = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = terminal_id_of(&app, pane_id);
        let other_terminal_id = terminal_id_of(&app, other_pane);

        let decision = create_decision_for_pane(&mut app, public);

        assert!(app.state.terminals[&terminal_id].decision_attention());
        assert!(!app.state.terminals[&other_terminal_id].decision_attention());

        let response = app.handle_decision_cancel(
            "req-cancel".into(),
            DecisionCancelParams {
                decision_id: decision.decision_id,
            },
        );
        let success: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("decision cancel succeeds");
        assert!(matches!(success.result, ResponseResult::Decision { .. }));
        assert!(!app.state.terminals[&terminal_id].decision_attention());
    }

    #[test]
    fn pending_decision_attention_clears_on_expiry() {
        let (mut app, pane_id) = app_with_pane("expiry");
        let public = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = terminal_id_of(&app, pane_id);

        // A zero timeout writes an `expires_unix_ms` that is already due, so
        // the server-side expiry pass resolves it on the next tick.
        let response = app.handle_decision_create(
            "req-create".into(),
            DecisionCreateParams {
                kind: DecisionKind::Ask,
                title: "continue?".into(),
                body: None,
                options: vec![option("yes"), option("no")],
                allow_text: false,
                multi_select: false,
                timeout_ms: Some(0),
                origin: Some(crate::api::schema::DecisionOrigin {
                    pane_id: Some(public),
                    ..Default::default()
                }),
            },
        );
        let _: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("decision create succeeds");
        assert!(app.state.terminals[&terminal_id].decision_attention());

        app.decision_expiry_deadline = Some(Instant::now() - Duration::from_secs(1));

        assert!(app.expire_due_decisions(Instant::now()));
        assert!(!app.state.terminals[&terminal_id].decision_attention());
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Unknown
        );
    }

    #[test]
    fn pending_decision_attention_ignores_unresolvable_origin_panes() {
        let (mut app, pane_id) = app_with_pane("remote-origin");
        let terminal_id = terminal_id_of(&app, pane_id);

        // A decision whose origin pane id cannot be resolved locally (for
        // example a pane on a saved machine) must not mark local panes.
        let _decision = create_decision_for_pane(&mut app, "p999999".into());

        assert!(!app.state.terminals[&terminal_id].decision_attention());
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Unknown
        );
    }

    #[test]
    fn pending_decision_attention_reasserts_on_restart_sync() {
        let (mut app, pane_id) = app_with_pane("restart");
        let public = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = terminal_id_of(&app, pane_id);

        let decision = create_decision_for_pane(&mut app, public);
        assert!(app.state.terminals[&terminal_id].decision_attention());

        // Runtime attention flags do not persist: simulate a restart by
        // clearing the flag on the terminal, then re-running the startup
        // sync the constructor performs.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_decision_attention(false, Instant::now());
        assert!(!app.state.terminals[&terminal_id].decision_attention());

        app.sync_pending_decision_attention();
        assert!(app.state.terminals[&terminal_id].decision_attention());
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Blocked
        );

        let _ = decision;
    }
}
