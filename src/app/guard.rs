//! Marker sync between herdr's agent state and the command
//! guard's per-pane marker files (`<state>/guard/agent-panes/`).
//!
//! A marker file exists only for an agent with effective launch coverage;
//! guard shims/shell functions judge commands only when their pane's marker
//! is present. Human panes therefore never get judged.

use super::App;

impl App {
    pub(crate) fn sync_guard_markers_for_all_panes(&mut self) {
        let panes: Vec<_> = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs
                    .iter()
                    .flat_map(move |tab| tab.panes.keys().copied().map(move |pane| (ws_idx, pane)))
            })
            .collect();
        for (ws_idx, pane) in panes {
            self.sync_guard_marker_for_pane(ws_idx, pane);
        }
    }

    /// Sync the pane's guard marker to its current effective agent label.
    /// Called whenever the label may have changed (detect/release, agent
    /// start, pane death).
    pub(crate) fn sync_guard_marker_for_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let public_pane_id = format!("p{}", pane_id.raw());
        let pane_env_id = crate::workspace::pane_env_id_from_public(&public_pane_id);

        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            crate::guard::marker::sync(&pane_env_id, None, None, None, &[]);
            return;
        };
        let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) else {
            crate::guard::marker::sync(&pane_env_id, None, None, None, &[]);
            return;
        };
        let tab = &ws.tabs[tab_idx];
        // Match `agent list`: a pane counts as agent-owned while the terminal
        // has an agent name, a detected label, or managed launch argv.
        let agent = tab
            .terminal_id(pane_id)
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
            .filter(|terminal| terminal.is_agent_terminal())
            .and_then(|terminal| {
                terminal
                    .effective_agent_label()
                    .or(terminal.agent_name.as_deref())
            });
        let agent = agent.map(str::to_owned);
        let cwd = tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes);
        let warning = if self.state.guard_config.enabled && agent.is_some() {
            let reason = self
                .state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
                .and_then(|runtime| runtime.guard_launch())
                .map_or(
                    Some("起動時に見張りが差し込まれていません"),
                    |launch| launch.reason(self.state.guard_config.mode),
                );
            reason.map(|reason| crate::guard::coverage::warning(&public_pane_id, reason))
        } else {
            None
        };
        let targets = crate::guard::targets(&self.state.guard_config);
        crate::guard::marker::sync(
            &pane_env_id,
            agent
                .as_deref()
                .filter(|_| self.state.guard_config.enabled && warning.is_none()),
            cwd.as_deref(),
            Some(&public_pane_id),
            &targets,
        );
        crate::guard::coverage::sync(&public_pane_id, warning.as_deref());
        if let Some(warning) = warning {
            self.state.guard_warnings.insert(public_pane_id, warning);
        } else {
            self.state.guard_warnings.remove(&public_pane_id);
        }
    }
}
