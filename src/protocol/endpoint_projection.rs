//! Optional JSON facts surround the frozen generation-1 binary snapshot.
use super::endpoint_wire::{ClientShellSnapshot, ClientShellWorktree};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

pub(crate) const FORWARDED_NOTIFICATION_KIND: &str = "runtime.notification";
pub(crate) const VIEWER_FOCUS_KIND: &str = "runtime.viewer-focus";

// Named JSON controls preserve the private notification payload without changing
// the frozen generation-1 binary message graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ForwardedNotification {
    pub boot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_id: Option<u64>,
    pub notification: crate::protocol::ServerMessage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceFacts {
    pub git_space: Option<ClientShellWorktree>,
    #[serde(default, deserialize_with = "optional_section")]
    pub section: Option<crate::workspace::WorkspaceSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaneFacts {
    pub number: u32,
    pub effective_title: Option<String>,
    pub title: Option<String>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    pub agent_label: Option<String>,
    pub display_agent: Option<String>,
    pub launch_argv: Option<Vec<String>>,
    pub state: crate::api::schema::AgentStatus,
    pub seen: bool,
    pub state_labels: BTreeMap<String, String>,
}

fn optional_section<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::workspace::WorkspaceSection>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    // Future section values remain unprovided to this client; server values are not rewritten.
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Serialize only inside the named JSON control, never as a binary wire payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SnapshotJson {
    #[serde(flatten)]
    pub snapshot: ClientShellSnapshot,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_facts: BTreeMap<String, WorkspaceFacts>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pane_facts: BTreeMap<String, PaneFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<super::endpoint_wire::SemanticNotification>,
}
impl From<ClientShellSnapshot> for SnapshotJson {
    fn from(snapshot: ClientShellSnapshot) -> Self {
        Self {
            snapshot,
            workspace_facts: BTreeMap::new(),
            pane_facts: BTreeMap::new(),
            notification: None,
        }
    }
}
impl Deref for SnapshotJson {
    type Target = ClientShellSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}
impl DerefMut for SnapshotJson {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_json_section_absence_and_unknown_value_remain_unprovided() {
        for data in [
            r#"{"git_space":null}"#,
            r#"{"git_space":null,"section":"future"}"#,
        ] {
            let facts: WorkspaceFacts = serde_json::from_str(data).unwrap();
            assert_eq!(facts.section, None);
        }
        for section in crate::workspace::WorkspaceSection::ALL {
            let facts = WorkspaceFacts {
                git_space: None,
                section: Some(section),
            };
            assert_eq!(
                serde_json::from_str::<WorkspaceFacts>(&serde_json::to_string(&facts).unwrap())
                    .unwrap(),
                facts
            );
        }
    }
    #[test]
    fn endpoint_json_facts_do_not_enter_frozen_binary_or_break_old_json_reader() {
        let mut projection: SnapshotJson = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        assert!(projection.workspace_facts.is_empty());
        assert!(projection.pane_facts.is_empty());
        assert!(projection.notification.is_none());
        let binary_before = bincode::serde::encode_to_vec(
            super::super::endpoint_wire::ServerMessage::ClientShellSnapshot(Box::new(
                projection.snapshot.clone(),
            )),
            bincode::config::standard(),
        )
        .unwrap();
        projection.workspace_facts.insert(
            projection.workspaces[0].workspace_id.clone(),
            WorkspaceFacts {
                section: None,
                git_space: Some(ClientShellWorktree {
                    key: "repo".into(),
                    label: "repo".into(),
                    is_linked_worktree: false,
                }),
            },
        );
        projection.pane_facts.insert(
            projection.panes[0].pane_id.clone(),
            PaneFacts {
                number: 1,
                effective_title: Some("owned terminal".into()),
                title: Some("owned terminal".into()),
                manual_label: None,
                agent_name: None,
                agent_label: None,
                display_agent: None,
                launch_argv: Some(vec!["/bin/sh".into()]),
                state: crate::api::schema::AgentStatus::Unknown,
                seen: true,
                state_labels: BTreeMap::new(),
            },
        );
        projection.notification = Some(super::super::endpoint_wire::SemanticNotification {
            kind: super::super::endpoint_wire::SemanticNotificationKind::NeedsAttention,
            title: "owned notification".into(),
            body: Some("owned context".into()),
            sound: None,
            agent: None,
            workspace_id: Some(projection.workspaces[0].workspace_id.clone()),
            tab_id: Some(projection.tabs[0].tab_id.clone()),
            pane_id: Some(projection.panes[0].pane_id.clone()),
            position: Some(crate::config::ToastHerdrPosition::BottomRight),
        });
        let data = serde_json::to_string(&projection).unwrap();
        let old_reader: ClientShellSnapshot = serde_json::from_str(&data).unwrap();
        assert_eq!(old_reader, projection.snapshot);
        let new_reader: SnapshotJson = serde_json::from_str(&data).unwrap();
        assert_eq!(new_reader, projection);
        let binary_after = bincode::serde::encode_to_vec(
            super::super::endpoint_wire::ServerMessage::ClientShellSnapshot(Box::new(old_reader)),
            bincode::config::standard(),
        )
        .unwrap();
        assert_eq!(binary_before, binary_after);
    }
}
