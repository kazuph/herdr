//! Client-local resource selection. Runtime and persistent IDs remain server-owned.
//! Location reconciliation is ported from fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0.

// The viewer producer and API dispatcher share this pure selection contract.
#![allow(dead_code)]
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClientShellLocation {
    pub(crate) focused_workspace_id: Option<String>,
    pub(crate) active_tab_ids: HashMap<String, String>,
    pub(crate) focused_pane_ids: HashMap<String, String>,
}

pub(crate) struct ClientShellTopology {
    pub(crate) focused_workspace_id: Option<String>,
    pub(crate) fallback_workspace_id: Option<String>,
    pub(crate) active_tab_ids: HashMap<String, String>,
    pub(crate) tab_workspace_ids: HashMap<String, String>,
    pub(crate) focused_pane_ids: HashMap<String, String>,
    pub(crate) pane_tab_ids: HashMap<String, String>,
}

impl ClientShellLocation {
    pub(crate) fn from_snapshot(
        snapshot: &crate::protocol::endpoint_wire::ClientShellSnapshot,
    ) -> Self {
        Self {
            focused_workspace_id: snapshot.focused_workspace_id.clone(),
            focused_pane_ids: snapshot
                .panes
                .iter()
                .filter(|pane| pane.focused)
                .map(|pane| (pane.tab_id.clone(), pane.pane_id.clone()))
                .collect(),
            active_tab_ids: snapshot
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace.workspace_id.clone(),
                        workspace.active_tab_id.clone(),
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn focused_tab_id(&self) -> Option<&str> {
        self.focused_workspace_id
            .as_deref()
            .and_then(|workspace_id| self.active_tab_ids.get(workspace_id))
            .map(String::as_str)
    }

    pub(crate) fn focused_pane_id(&self) -> Option<&str> {
        self.focused_pane_ids
            .get(self.focused_tab_id()?)
            .map(String::as_str)
    }

    pub(crate) fn focus_pane(&mut self, workspace: String, tab: String, pane: String) {
        self.focused_pane_ids.insert(tab.clone(), pane);
        self.focus_tab(workspace, tab);
    }

    pub(crate) fn focus_workspace(&mut self, workspace_id: String) {
        self.focused_workspace_id = Some(workspace_id);
    }

    pub(crate) fn focus_tab(&mut self, workspace_id: String, tab_id: String) {
        self.focused_workspace_id = Some(workspace_id.clone());
        self.active_tab_ids.insert(workspace_id, tab_id);
    }

    /// Endpoint response facts follow this viewer, just as its snapshot does.
    /// The public JSON API continues to report the server's global location.
    pub(crate) fn project_response(
        &self,
        result: &mut crate::api::schema::ResponseResult,
        focused_pane: Option<&str>,
    ) {
        use crate::api::schema::ResponseResult;
        let workspace = |workspace: &mut crate::api::schema::WorkspaceInfo| {
            workspace.focused =
                self.focused_workspace_id.as_deref() == Some(&workspace.workspace_id);
            if let Some(active) = self.active_tab_ids.get(&workspace.workspace_id) {
                workspace.active_tab_id.clone_from(active);
            }
        };
        let tab = |tab: &mut crate::api::schema::TabInfo| {
            tab.focused = self.focused_tab_id() == Some(&tab.tab_id);
        };
        match result {
            ResponseResult::WorkspaceInfo { workspace: info } => workspace(info),
            ResponseResult::WorkspaceCreated {
                workspace: info,
                tab: created_tab,
                root_pane,
            }
            | ResponseResult::WorktreeCreated {
                workspace: info,
                tab: created_tab,
                root_pane,
                ..
            }
            | ResponseResult::WorktreeOpened {
                workspace: info,
                tab: created_tab,
                root_pane,
                ..
            } => {
                workspace(info);
                tab(created_tab);
                root_pane.focused = focused_pane == Some(root_pane.pane_id.as_str());
            }
            ResponseResult::TabCreated {
                tab: created_tab,
                root_pane,
            } => {
                tab(created_tab);
                root_pane.focused = focused_pane == Some(root_pane.pane_id.as_str());
            }
            ResponseResult::WorkspaceList { workspaces } => {
                for info in workspaces {
                    workspace(info);
                }
            }
            ResponseResult::TabInfo { tab: info } => tab(info),
            ResponseResult::TabList { tabs } => {
                for info in tabs {
                    tab(info);
                }
            }
            ResponseResult::PaneInfo { pane } => {
                pane.focused = focused_pane == Some(&pane.pane_id);
            }
            ResponseResult::PaneLayout { layout } => {
                if let Some(focus) = self.focused_pane_ids.get(&layout.tab_id) {
                    layout.focused_pane_id.clone_from(focus);
                    for pane in &mut layout.panes {
                        pane.focused = pane.pane_id == *focus;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn reconcile(&mut self, topology: &ClientShellTopology) {
        self.focused_pane_ids
            .retain(|tab, pane| topology.pane_tab_ids.get(pane) == Some(tab));
        for (tab, pane) in &topology.focused_pane_ids {
            self.focused_pane_ids
                .entry(tab.clone())
                .or_insert_with(|| pane.clone());
        }
        self.active_tab_ids.retain(|workspace_id, tab_id| {
            topology.active_tab_ids.contains_key(workspace_id)
                && topology.tab_workspace_ids.get(tab_id) == Some(workspace_id)
        });
        for (workspace_id, tab_id) in &topology.active_tab_ids {
            self.active_tab_ids
                .entry(workspace_id.clone())
                .or_insert_with(|| tab_id.clone());
        }
        if self
            .focused_workspace_id
            .as_ref()
            .is_none_or(|workspace_id| !topology.active_tab_ids.contains_key(workspace_id))
        {
            self.focused_workspace_id = topology
                .focused_workspace_id
                .clone()
                .or_else(|| topology.fallback_workspace_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_tab_viewers_keep_pane_focus_when_global_focus_changes_or_pane_closes() {
        let mut topology = topology();
        topology.pane_tab_ids.insert("p3".into(), "s1:t1".into());
        let mut first = ClientShellLocation::default();
        first.reconcile(&topology);
        let mut second = first.clone();
        second.focus_pane("s1".into(), "s1:t1".into(), "p3".into());
        topology
            .focused_pane_ids
            .insert("s1:t1".into(), "p3".into());
        first.reconcile(&topology);
        second.reconcile(&topology);
        assert_eq!(first.focused_pane_id(), Some("p1"));
        assert_eq!(second.focused_pane_id(), Some("p3"));
        topology.pane_tab_ids.remove("p3");
        topology
            .focused_pane_ids
            .insert("s1:t1".into(), "p1".into());
        second.reconcile(&topology);
        assert_eq!(second.focused_pane_id(), Some("p1"));
        assert_eq!(first.focused_pane_id(), Some("p1"));
    }

    fn topology() -> ClientShellTopology {
        ClientShellTopology {
            focused_workspace_id: Some("s1".into()),
            fallback_workspace_id: Some("s1".into()),
            active_tab_ids: HashMap::from([
                ("s1".into(), "s1:t1".into()),
                ("opaque:remote/workspace".into(), "opaque:remote/tab".into()),
            ]),
            tab_workspace_ids: HashMap::from([
                ("s1:t1".into(), "s1".into()),
                ("s1:t2".into(), "s1".into()),
                ("opaque:remote/tab".into(), "opaque:remote/workspace".into()),
            ]),
            focused_pane_ids: HashMap::from([
                ("s1:t1".into(), "p1".into()),
                ("s1:t2".into(), "p2".into()),
                ("opaque:remote/tab".into(), "opaque:remote/pane".into()),
            ]),
            pane_tab_ids: HashMap::from([
                ("p1".into(), "s1:t1".into()),
                ("p2".into(), "s1:t2".into()),
                ("opaque:remote/pane".into(), "opaque:remote/tab".into()),
            ]),
        }
    }

    #[test]
    fn viewer_selection_survives_other_viewer_and_global_focus_changes() {
        let mut first = ClientShellLocation::default();
        let mut second = ClientShellLocation::default();
        first.reconcile(&topology());
        second.reconcile(&topology());
        first.focus_tab("s1".into(), "s1:t2".into());
        second.focus_workspace("opaque:remote/workspace".into());
        first.reconcile(&topology());
        second.reconcile(&topology());
        assert_eq!(first.focused_tab_id(), Some("s1:t2"));
        assert_eq!(second.focused_tab_id(), Some("opaque:remote/tab"));
        assert_eq!(topology().focused_workspace_id.as_deref(), Some("s1"));
    }

    #[test]
    fn moved_or_closed_tabs_reconcile_without_cross_workspace_aliasing() {
        let mut viewer = ClientShellLocation::default();
        viewer.reconcile(&topology());
        viewer.focus_tab("s1".into(), "s1:t2".into());
        let mut changed = topology();
        changed
            .tab_workspace_ids
            .insert("s1:t2".into(), "opaque:remote/workspace".into());
        viewer.reconcile(&changed);
        assert_eq!(viewer.focused_tab_id(), Some("s1:t1"));
        assert_eq!(viewer.active_tab_ids.len(), changed.active_tab_ids.len());
        changed.active_tab_ids.remove("s1");
        changed
            .tab_workspace_ids
            .retain(|_, workspace| workspace != "s1");
        changed.focused_workspace_id = Some("opaque:remote/workspace".into());
        viewer.reconcile(&changed);
        assert_eq!(viewer.focused_tab_id(), Some("opaque:remote/tab"));
        assert!(!viewer.active_tab_ids.contains_key("s1"));
    }
}
