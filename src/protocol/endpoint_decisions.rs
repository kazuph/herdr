//! Pending-decision projection delivered over the endpoint `EndpointControl`
//! channel, mirroring the jobs projection in `endpoint_jobs`. Generation-1
//! peers ignore the unknown kind, so this stays additive.
//!
//! The projection always lists *pending* decisions only (server-owned);
//! clients must not reconstruct decision state from individual events.

use serde::{Deserialize, Serialize};

/// Endpoint handshake capability advertising decisions-projection support.
pub(crate) const DECISIONS_PROJECTION_CAPABILITY: &str = "decisions_projection_v1";
/// `EndpointControl` kind carrying the pending-decision projection.
pub(crate) const DECISIONS_PROJECTION_KIND: &str = "decisions_projection";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EndpointDecisionsProjection {
    /// Server boot stamp; the client rejects payloads bound to another boot.
    pub(crate) boot_id: String,
    /// Server-issued monotonically increasing projection revision. The client
    /// discards revisions that do not move forward within one connection
    /// generation, so delayed or replayed frames cannot resurrect resolved
    /// decisions.
    pub(crate) revision: u64,
    /// Pending decisions in creation order, exactly as `decision.list` with
    /// `status = pending` returns them.
    pub(crate) decisions: Vec<crate::api::schema::Decision>,
}
