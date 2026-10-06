use serde::{Deserialize, Serialize};

use super::agents::AgentInfo;
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_warnings: Option<Vec<AgentSessionWarningInfo>>,
}

/// Runtime owner reports the same safe-session/ledger check used before restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionWarningInfo {
    pub workspace_number: usize,
    pub workspace_label: String,
    pub pane_label: String,
    pub agent: String,
    pub title: Option<String>,
    pub cwd: String,
    pub reason: String,
}

impl From<&crate::app::state::MissingAgentSessionWarning> for AgentSessionWarningInfo {
    fn from(value: &crate::app::state::MissingAgentSessionWarning) -> Self {
        Self {
            workspace_number: value.workspace_number,
            workspace_label: value.workspace_label.clone(),
            pane_label: value.pane_label.clone(),
            agent: value.agent.clone(),
            title: value.title.clone(),
            cwd: value.cwd.to_string_lossy().into_owned(),
            reason: value.reason.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_warning_fact_distinguishes_legacy_absence_from_owner_empty() {
        let legacy = serde_json::json!({"version":"existing","protocol":19,"workspaces":[],"tabs":[],"panes":[],"layouts":[],"agents":[]});
        let mut snapshot: SessionSnapshot = serde_json::from_value(legacy.clone()).unwrap();
        assert!(snapshot.agent_session_warnings.is_none());
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), legacy);
        snapshot.agent_session_warnings = Some(Vec::new());
        assert_eq!(
            serde_json::to_value(snapshot).unwrap()["agent_session_warnings"],
            serde_json::json!([])
        );
    }
}
