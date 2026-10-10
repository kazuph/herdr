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
    pub(crate) fn handle_decision_create(
        &mut self,
        id: String,
        params: DecisionCreateParams,
    ) -> String {
        if let Err(message) = validate_decision_create(&params) {
            return responses::encode_error(id, "invalid_request", message);
        }
        let now = unix_ms_now();
        let decision = Decision {
            decision_id: new_decision_id(),
            kind: params.kind,
            title: params.title,
            body: params.body.filter(|body| !body.is_empty()),
            options: params.options,
            allow_text: params.allow_text,
            origin: params
                .origin
                .filter(|origin| *origin != crate::api::schema::DecisionOrigin::default()),
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
        match store.answer(
            &params.decision_id,
            params.option_id,
            params.text,
            params.responder,
            unix_ms_now(),
        ) {
            Ok(AnswerOutcome::Answered(decision)) => {
                let decision = *decision;
                self.emit_decision_resolved(decision.clone());
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
        self.expire_due_decisions_at(now);
        true
    }

    fn expire_due_decisions_at(&mut self, now: Instant) {
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
        for decision in expired {
            self.emit_decision_resolved(decision);
        }
        self.sync_decision_expiry_deadline(now);
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

    fn emit_decision_resolved(&mut self, decision: Decision) {
        self.emit_event(EventEnvelope {
            event: EventKind::DecisionResolved,
            data: EventData::DecisionResolved { decision },
        });
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
            timeout_ms: None,
            origin: None,
        }
    }

    fn option(id: &str) -> DecisionOption {
        DecisionOption {
            id: id.into(),
            label: format!("label {id}"),
            role: DecisionOptionRole::Other,
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
}
