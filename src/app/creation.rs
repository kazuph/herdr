use tracing::error;

use super::{
    api_helpers::{pane_agent_status, tab_attention_priority},
    App, Mode,
};
use crate::workspace::Workspace;

impl App {
    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<std::path::PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.state.terminal_runtimes)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    /// Create a workspace with a real PTY (needs event_tx).
    pub(crate) fn create_workspace(&mut self) {
        let requested_section = self.state.requested_new_workspace_section.take();
        let initial_cwd = self
            .workspace_creation_source()
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match self.create_workspace_with_options(initial_cwd, true) {
            Ok(ws_idx) => {
                if let Some(section) = requested_section {
                    if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                        ws.section = section;
                    }
                    self.state.collapsed_workspace_sections.remove(&section);
                    self.schedule_session_save();
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create workspace");
                self.state.mode = Mode::Navigate;
            }
        }
    }

    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let initial_cwd = self
            .state
            .active
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(tab_idx) => {
                if let Some(name) = custom_name {
                    if let Some(ws) = self
                        .state
                        .active
                        .and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    pub(super) fn create_tab_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let ws = &mut self.state.workspaces[ws_idx];
        let (idx, terminal, runtime) = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            &self.state.default_shell,
        )?;
        self.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        if focus {
            ws.switch_tab(idx);
            self.state.mode = Mode::Terminal;
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, idx)
            .unwrap_or_else(|| format!("{}:{}", workspace_id, idx + 1));
        let root_pane = self.state.workspaces[ws_idx].tabs[idx].root_pane.raw();
        crate::logging::tab_created(&workspace_id, &tab_id, root_pane);
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn create_workspace_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, terminal, runtime) = Workspace::new(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            &self.state.default_shell,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        self.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        } else {
            self.state.ensure_workspace_visible(idx);
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn duplicate_workspace(&mut self, ws_idx: usize) -> std::io::Result<()> {
        if ws_idx >= self.state.workspaces.len() {
            return Err(std::io::Error::other("workspace not found"));
        }

        let (rows, cols) = self.state.estimate_pane_size();
        let mut snapshot = crate::persist::capture(
            &self.state.workspaces,
            &self.state.terminals,
            &self.state.terminal_runtimes,
            Some(ws_idx),
            ws_idx,
            self.state.agent_panel_scope,
            self.state.sidebar_width,
            self.state.sidebar_section_split,
            &self.state.collapsed_workspace_sections,
        );
        let mut workspace_snapshot = snapshot.workspaces.remove(ws_idx);
        workspace_snapshot.id = None;

        let duplicate_snapshot = crate::persist::SessionSnapshot {
            version: snapshot.version,
            workspaces: vec![workspace_snapshot],
            active: Some(0),
            selected: 0,
            agent_panel_scope: self.state.agent_panel_scope,
            sidebar_width: Some(self.state.sidebar_width),
            sidebar_section_split: Some(self.state.sidebar_section_split),
            collapsed_workspace_sections: self.state.collapsed_workspace_sections.clone(),
        };
        let (mut workspaces, terminals, terminal_runtimes) = crate::persist::restore(
            &duplicate_snapshot,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            false,
            &self.state.default_shell,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        );
        let Some(workspace) = workspaces.pop() else {
            return Err(std::io::Error::other("workspace could not be restored"));
        };

        for (terminal_id, runtime) in terminal_runtimes {
            self.state.terminal_runtimes.insert(terminal_id, runtime);
        }
        for (terminal_id, terminal) in terminals {
            self.state.terminals.insert(terminal_id, terminal);
        }
        self.state.workspaces.push(workspace);
        let duplicate_idx = self.state.workspaces.len() - 1;
        self.state.switch_workspace(duplicate_idx);
        self.state.mode = Mode::Terminal;
        self.schedule_session_save();
        Ok(())
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .filter_map(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab_idx + 1,
            label: tab.display_name(),
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: pane_agent_status(agg_state, seen),
        })
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        let workspace_number = ws_idx + 1;
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            short_id: format!("{workspace_number}-{pane_number}"),
            global_id: format!("p_{}", pane_id.raw()),
            global_number: pane_id.raw(),
            workspace_number,
            pane_number,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(
                    pane_id,
                    &self.state.terminals,
                    &self.state.terminal_runtimes,
                )
                .map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            agent_status: pane_agent_status(terminal.state, pane.seen),
            custom_status: terminal.effective_custom_status().map(str::to_string),
            revision: terminal.revision,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime = self.state.runtime_for_pane_in_workspace(ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state.runtime_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.state.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self
                .public_tab_id(index, ws.active_tab)
                .unwrap_or_else(|| format!("{}:{}", ws.id, ws.active_tab + 1)),
            agent_status: pane_agent_status(agg_state, seen),
        }
    }
}
