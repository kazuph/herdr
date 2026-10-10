//! Pending-decision projection served to endpoint clients (all machines UI).
//!
//! The projection is a whole-list replacement filtered to `pending`, so a
//! client never has to merge events; resolved, expired, and cancelled
//! decisions simply disappear from the next projection. Clients discard
//! non-increasing revisions inside a connection generation.

use crate::protocol::endpoint_decisions::EndpointDecisionsProjection;

/// Projection payload for pending decisions, in the stable `decision.list`
/// creation order.
pub(crate) fn decisions_projection(
    boot_id: &str,
    revision: u64,
    pending: &[crate::api::schema::Decision],
) -> EndpointDecisionsProjection {
    EndpointDecisionsProjection {
        boot_id: boot_id.to_owned(),
        revision,
        decisions: pending.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Decision, DecisionKind, DecisionOption, DecisionStatus};

    fn pending(id: &str, created: u64) -> Decision {
        Decision {
            decision_id: id.into(),
            kind: DecisionKind::Ask,
            title: format!("title-{id}"),
            body: None,
            options: vec![DecisionOption {
                id: "yes".into(),
                label: "Yes".into(),
                role: crate::api::schema::DecisionOptionRole::Approve,
            }],
            allow_text: false,
            origin: None,
            created_unix_ms: created,
            expires_unix_ms: None,
            status: DecisionStatus::Pending,
            answer: None,
        }
    }

    #[test]
    fn projection_carries_boot_revision_and_pending_list() {
        let pending = vec![pending("a", 1), pending("b", 2)];
        let projection = decisions_projection("boot", 7, &pending);
        assert_eq!(projection.boot_id, "boot");
        assert_eq!(projection.revision, 7);
        assert_eq!(projection.decisions, pending);
        let roundtrip: EndpointDecisionsProjection =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        assert_eq!(roundtrip, projection);
    }
}
