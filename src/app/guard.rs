//! Marker sync between herdr's agent state and the command
//! guard's per-pane marker files (`<state>/guard/agent-panes/`).
//!
//! A marker file exists while herdr believes an agent runs in the pane;
//! guard shims/shell functions judge commands only when their pane's marker
//! is present. Human panes therefore never get judged.

use super::App;

impl App {
    /// Sync the pane's guard marker to its current effective agent label.
    /// Called whenever the label may have changed (detect/release, agent
    /// start, pane death).
    pub(crate) fn sync_guard_marker_for_pane(&self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        if !self.state.guard_config.enabled {
            return;
        }
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
        let cwd = tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes);
        let targets = crate::guard::targets(&self.state.guard_config);
        crate::guard::marker::sync(
            &pane_env_id,
            agent,
            cwd.as_deref(),
            Some(&public_pane_id),
            &targets,
        );
    }
}
