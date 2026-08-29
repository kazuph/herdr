use std::path::PathBuf;

use crate::app::{App, Mode};
use crate::layout::PaneId;
use crate::pane::PaneLaunchEnv;
use crate::popup_size::{resolve_popup_geometry, PopupSize};
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PopupGeometry {
    pub width: Option<PopupSize>,
    pub height: Option<PopupSize>,
}

impl App {
    pub(crate) fn popup_runtime(&self) -> Option<&TerminalRuntime> {
        let terminal_id = &self.state.active_popup_pane()?.terminal_id;
        self.terminal_runtimes.get(terminal_id)
    }

    /// Closes the active workspace's popup, if it has one. Other
    /// workspaces' popups are left running in the background.
    pub(crate) fn close_popup_pane(&mut self) -> bool {
        let Some(pane_id) = self.state.active_popup_pane().map(|popup| popup.pane_id) else {
            return false;
        };
        self.close_popup_pane_by_pane_id(pane_id)
    }

    /// Closes whichever popup owns `pane_id`, regardless of which
    /// workspace is currently active. Only touches `mode` when the closed
    /// popup belonged to the active workspace, since `mode` reflects the
    /// active workspace's UI state.
    pub(crate) fn close_popup_pane_by_pane_id(&mut self, pane_id: PaneId) -> bool {
        let Some(popup) = self.state.popup_pane_by_pane_id(pane_id) else {
            return false;
        };
        let workspace_id = popup.workspace_id.clone();
        let was_active = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            .is_some_and(|workspace| workspace.id == workspace_id);
        let Some(popup) = self.state.take_popup_pane_for_workspace(&workspace_id) else {
            return false;
        };
        if let Some(runtime) = self.terminal_runtimes.remove(&popup.terminal_id) {
            runtime.shutdown();
        }
        if was_active {
            self.state.mode = if self.state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
        true
    }

    pub(crate) fn try_route_paste_to_popup(&mut self, text: &str) -> bool {
        if !self.state.popup_pane_is_visible() || self.state.mode != Mode::Terminal {
            return false;
        }
        let Some(runtime) = self.popup_runtime() else {
            self.close_popup_pane();
            return true;
        };
        let _ = runtime.try_send_paste(text.to_owned());
        true
    }

    pub(crate) fn spawn_popup_shell_command(
        &mut self,
        command: &str,
        cwd: Option<PathBuf>,
        extra_env: Vec<(String, String)>,
        geometry: PopupGeometry,
    ) -> std::io::Result<()> {
        self.spawn_popup_command(
            cwd,
            extra_env,
            geometry,
            |pane_id, rows, cols, cwd, launch_env, app| {
                TerminalRuntime::spawn_shell_command(
                    pane_id,
                    rows,
                    cols,
                    cwd,
                    command,
                    launch_env,
                    crate::pane::AgentDetection::Disabled,
                    app.state.pane_scrollback_limit_bytes,
                    app.state.host_terminal_theme,
                    app.event_tx.clone(),
                    app.render_notify.clone(),
                    app.render_dirty.clone(),
                )
                .map(|runtime| (runtime, None))
            },
        )
    }

    pub(crate) fn spawn_popup_argv_command(
        &mut self,
        argv: &[String],
        cwd: Option<PathBuf>,
        extra_env: Vec<(String, String)>,
        geometry: PopupGeometry,
    ) -> std::io::Result<()> {
        self.spawn_popup_command(
            cwd,
            extra_env,
            geometry,
            |pane_id, rows, cols, cwd, launch_env, app| {
                TerminalRuntime::spawn_argv_command(
                    pane_id,
                    rows,
                    cols,
                    cwd,
                    argv,
                    launch_env,
                    crate::pane::AgentDetection::Disabled,
                    app.state.pane_scrollback_limit_bytes,
                    app.state.host_terminal_theme,
                    app.event_tx.clone(),
                    app.render_notify.clone(),
                    app.render_dirty.clone(),
                )
                .map(|runtime| (runtime, Some(argv.to_vec())))
            },
        )
    }

    fn spawn_popup_command<F>(
        &mut self,
        cwd: Option<PathBuf>,
        extra_env: Vec<(String, String)>,
        geometry: PopupGeometry,
        spawn: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(
            PaneId,
            u16,
            u16,
            PathBuf,
            &PaneLaunchEnv,
            &mut App,
        ) -> std::io::Result<(TerminalRuntime, Option<Vec<String>>)>,
    {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let workspace_id = ws.id.clone();
        if self.state.popup_pane_for_workspace(&workspace_id).is_some() {
            return Err(std::io::Error::other("popup already open"));
        }
        let active_tab = ws
            .active_tab()
            .ok_or_else(|| std::io::Error::other("active tab disappeared"))?;
        let focused_pane = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("active tab has no focused pane"))?;
        let cwd = cwd.or_else(|| {
            active_tab.cwd_for_pane(focused_pane, &self.state.terminals, &self.terminal_runtimes)
        });
        let cwd = cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let pane_id = PaneId::alloc();
        let terminal_id = TerminalId::alloc();
        let launch_env = PaneLaunchEnv::from_extra(extra_env).without_pane_identity();
        let terminal_area = if self.state.view.terminal_area.width >= 4
            && self.state.view.terminal_area.height >= 4
        {
            self.state.view.terminal_area
        } else {
            let (estimated_rows, estimated_cols) = self.state.estimate_pane_size();
            ratatui::layout::Rect::new(0, 0, estimated_cols, estimated_rows)
        };
        let Some(resolved_geometry) =
            resolve_popup_geometry(geometry.width, geometry.height, terminal_area)
        else {
            return Err(std::io::Error::other("terminal area too small for popup"));
        };
        let rows = resolved_geometry.inner.height;
        let cols = resolved_geometry.inner.width;
        let (runtime, launch_argv) = spawn(pane_id, rows, cols, cwd.clone(), &launch_env, self)?;
        let terminal = match launch_argv {
            Some(argv) => TerminalState::new(terminal_id.clone(), cwd).with_launch_argv(argv),
            None => TerminalState::new(terminal_id.clone(), cwd),
        };
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        self.state.terminals.insert(terminal_id.clone(), terminal);
        self.state
            .popup_panes
            .push(crate::app::state::PopupPaneState {
                pane_id,
                terminal_id,
                workspace_id,
                width: geometry.width,
                height: geometry.height,
            });
        self.state.mode = Mode::Terminal;
        Ok(())
    }
}

#[cfg(test)]
impl App {
    pub(crate) fn install_test_popup_runtime(
        &mut self,
        runtime: TerminalRuntime,
    ) -> (PaneId, TerminalId) {
        if self.state.workspaces.is_empty() {
            self.state
                .workspaces
                .push(crate::workspace::Workspace::test_new("popup"));
            self.state.active = Some(0);
            self.state.selected = 0;
        }
        let ws_idx = self.state.active.expect("active workspace");
        self.install_test_popup_runtime_for_workspace(ws_idx, runtime)
    }

    /// Like [`install_test_popup_runtime`], but installs the popup as owned
    /// by `ws_idx` regardless of which workspace is currently active. Lets
    /// tests set up multiple simultaneous popups across workspaces.
    pub(crate) fn install_test_popup_runtime_for_workspace(
        &mut self,
        ws_idx: usize,
        runtime: TerminalRuntime,
    ) -> (PaneId, TerminalId) {
        let pane_id = PaneId::alloc();
        let terminal_id = TerminalId::alloc();
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        self.state.terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id.clone(), PathBuf::from("/popup")),
        );
        self.state
            .popup_panes
            .push(crate::app::state::PopupPaneState {
                pane_id,
                terminal_id: terminal_id.clone(),
                workspace_id: self.state.workspaces[ws_idx].id.clone(),
                width: None,
                height: None,
            });
        (pane_id, terminal_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_popup() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("popup")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let terminal_id = TerminalId::alloc();
        app.state.terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id.clone(), PathBuf::from("/popup")),
        );
        app.state
            .popup_panes
            .push(crate::app::state::PopupPaneState {
                pane_id: PaneId::alloc(),
                terminal_id,
                workspace_id: app.state.workspaces[0].id.clone(),
                width: None,
                height: None,
            });
        app
    }

    #[test]
    fn close_popup_uses_terminal_mode_with_active_workspace() {
        let mut app = app_with_popup();
        app.state.mode = Mode::Navigate;

        assert!(app.close_popup_pane());

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    /// `close_popup_pane` only ever closes the *active* workspace's popup.
    /// With no active workspace there is no "the" popup to close, so it
    /// must no-op rather than reach for an arbitrary orphaned popup.
    #[test]
    fn close_popup_pane_is_noop_without_active_workspace() {
        let mut app = app_with_popup();
        app.state.workspaces.clear();
        app.state.active = None;
        app.state.mode = Mode::Navigate;

        assert!(!app.close_popup_pane());

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(!app.state.popup_panes.is_empty());
    }

    #[test]
    fn close_popup_clears_direct_attach_resize_lock() {
        let mut app = app_with_popup();
        let terminal_id = app.state.popup_panes[0].terminal_id.clone();
        app.state
            .direct_attach_resize_locks
            .insert(terminal_id.clone());

        assert!(app.close_popup_pane());

        assert!(!app.state.direct_attach_resize_locks.contains(&terminal_id));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn closing_popup_terminates_its_child_process() {
        let mut app = app_with_popup();
        app.state.popup_panes.clear();
        app.state.terminals.clear();
        app.spawn_popup_shell_command("sleep 30", None, Vec::new(), PopupGeometry::default())
            .expect("popup process should start");
        let child_pid = app
            .popup_runtime()
            .and_then(TerminalRuntime::child_pid)
            .expect("popup child pid");
        assert!(crate::platform::process_exists(child_pid));

        assert!(app.close_popup_pane());

        assert!(!crate::platform::process_exists(child_pid));
        assert!(app.state.popup_panes.is_empty());
    }

    #[test]
    fn popup_survives_background_workspace_removal() {
        let mut app = app_with_popup();
        let owner_workspace_id = app.state.workspaces[0].id.clone();
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("background"));

        app.state.workspaces.remove(1);

        assert_eq!(
            app.state
                .popup_pane_for_workspace(&owner_workspace_id)
                .map(|popup| popup.workspace_id.as_str()),
            Some(owner_workspace_id.as_str())
        );
    }

    #[test]
    fn closing_popup_owner_workspace_removes_popup_state() {
        let mut app = app_with_popup();
        let terminal_id = app.state.popup_panes[0].terminal_id.clone();

        app.state.close_selected_workspace();

        app.state.assert_invariants_for_test();
        assert!(app.state.popup_panes.is_empty());
        assert!(!app.state.terminals.contains_key(&terminal_id));
        assert!(app.state.terminal_runtime_shutdowns.contains(&terminal_id));
    }

    #[tokio::test]
    async fn popup_close_api_closes_only_active_popup() {
        let mut app = app_with_two_workspaces();
        let (runtime_a, _rx_a) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (runtime_b, _rx_b) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (_pane_a, terminal_a) = app.install_test_popup_runtime_for_workspace(0, runtime_a);
        let (_pane_b, terminal_b) = app.install_test_popup_runtime_for_workspace(1, runtime_b);
        app.state.active = Some(1);
        let close = || crate::api::schema::Request {
            id: "close-popup".into(),
            method: crate::api::schema::Method::PopupClose(
                crate::api::schema::EmptyParams::default(),
            ),
        };

        let response = app.handle_api_request(close());
        let response: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).unwrap();
        assert_eq!(response.result, crate::api::schema::ResponseResult::Ok {});
        assert!(app.terminal_runtimes.get(&terminal_a).is_some());
        assert!(app.terminal_runtimes.get(&terminal_b).is_none());

        let response = app.handle_api_request(close());
        let response: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.error.code, "popup_not_open");
    }

    fn app_with_two_workspaces() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("space-a"),
            crate::workspace::Workspace::test_new("space-b"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    /// A popup opened in Space A must not block opening a second, distinct
    /// popup in Space B: each Space gets its own PaneId/TerminalId/runtime,
    /// and switching the active Space swaps which popup is visible without
    /// destroying the other.
    #[cfg(unix)]
    #[tokio::test]
    async fn multiple_spaces_can_each_open_their_own_popup_and_switching_preserves_both() {
        let mut app = app_with_two_workspaces();

        app.spawn_popup_shell_command("sleep 1", None, Vec::new(), PopupGeometry::default())
            .expect("space a popup opens");
        let popup_a = app.state.active_popup_pane().cloned().unwrap();
        assert_eq!(popup_a.workspace_id, app.state.workspaces[0].id);

        // Same Space, second launch: rejected.
        let dup_err = app
            .spawn_popup_shell_command("sleep 1", None, Vec::new(), PopupGeometry::default())
            .unwrap_err();
        assert_eq!(dup_err.to_string(), "popup already open");

        // Switch to Space B: no popup visible there yet.
        app.state.active = Some(1);
        assert!(!app.state.popup_pane_is_visible());
        assert!(app.popup_runtime().is_none());

        app.spawn_popup_shell_command("sleep 1", None, Vec::new(), PopupGeometry::default())
            .expect("space b popup opens");
        let popup_b = app.state.active_popup_pane().cloned().unwrap();
        assert_eq!(popup_b.workspace_id, app.state.workspaces[1].id);

        // Distinct PaneId/TerminalId/runtime per Space.
        assert_ne!(popup_a.pane_id, popup_b.pane_id);
        assert_ne!(popup_a.terminal_id, popup_b.terminal_id);
        assert!(app.terminal_runtimes.get(&popup_a.terminal_id).is_some());
        assert!(app.terminal_runtimes.get(&popup_b.terminal_id).is_some());

        // Switch back to Space A: its popup is restored, Space B's is
        // preserved in the background (not owned/visible from A).
        app.state.active = Some(0);
        assert_eq!(app.state.active_popup_pane(), Some(&popup_a));
        assert!(app.terminal_runtimes.get(&popup_b.terminal_id).is_some());

        app.state.assert_invariants_for_test();

        // Closing A's popup (the active one) must not affect B's.
        assert!(app.close_popup_pane());
        assert!(app
            .state
            .popup_pane_for_workspace(&popup_a.workspace_id)
            .is_none());
        assert!(app
            .state
            .popup_pane_for_workspace(&popup_b.workspace_id)
            .is_some());
        assert!(app.terminal_runtimes.get(&popup_a.terminal_id).is_none());
        assert!(app.terminal_runtimes.get(&popup_b.terminal_id).is_some());

        app.state.assert_invariants_for_test();

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn close_popup_pane_by_pane_id_closes_background_popup_without_touching_active_mode() {
        let mut app = app_with_two_workspaces();
        let (runtime_a, _rx_a) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (runtime_b, _rx_b) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (pane_a, terminal_a) = app.install_test_popup_runtime_for_workspace(0, runtime_a);
        let (_pane_b, terminal_b) = app.install_test_popup_runtime_for_workspace(1, runtime_b);
        app.state.active = Some(1);
        app.state.mode = Mode::Terminal;

        assert!(app.close_popup_pane_by_pane_id(pane_a));

        assert!(app.terminal_runtimes.get(&terminal_a).is_none());
        assert!(app.terminal_runtimes.get(&terminal_b).is_some());
        assert_eq!(
            app.state.mode,
            Mode::Terminal,
            "background close must not change active workspace mode"
        );
        assert!(app
            .state
            .popup_pane_for_workspace(&app.state.workspaces[1].id)
            .is_some());
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn closing_one_popup_owner_workspace_preserves_the_other_popup() {
        let mut app = app_with_two_workspaces();
        let removed_workspace_id = app.state.workspaces[0].id.clone();
        let preserved_workspace_id = app.state.workspaces[1].id.clone();
        let (runtime_a, _rx_a) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (runtime_b, _rx_b) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (_pane_a, terminal_a) = app.install_test_popup_runtime_for_workspace(0, runtime_a);
        let (_pane_b, terminal_b) = app.install_test_popup_runtime_for_workspace(1, runtime_b);

        app.state.close_selected_workspace();
        app.shutdown_detached_terminal_runtimes();

        assert!(app
            .state
            .popup_pane_for_workspace(&removed_workspace_id)
            .is_none());
        assert!(app
            .state
            .popup_pane_for_workspace(&preserved_workspace_id)
            .is_some());
        assert!(app.terminal_runtimes.get(&terminal_a).is_none());
        assert!(app.terminal_runtimes.get(&terminal_b).is_some());
        app.state.assert_invariants_for_test();
    }
}
