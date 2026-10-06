//! Explicit-tab surfaces, ported from fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0.
// The server endpoint viewer consumes this independently of the private active view.
#![allow(dead_code)]
use ratatui::{layout::Rect, Frame};

use super::panes::{compute_tab_pane_infos, render_tab_panes, resize_tab_panes, TabPaneRender};
use crate::app::AppState;
use crate::layout::{PaneInfo, SplitBorder};
use crate::protocol::endpoint_wire::CursorState;
use crate::terminal::TerminalRuntimeRegistry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSurfaceTarget {
    pub(crate) workspace_index: usize,
    pub(crate) tab_index: usize,
    pub(crate) pane_focus: Option<crate::layout::PaneId>,
}

pub(crate) struct TabSurfaceLayout {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: Vec<PaneInfo>,
    pub(crate) split_borders: Vec<SplitBorder>,
}

#[derive(Clone, Copy)]
pub(crate) struct TargetTabSurfaceView<'a> {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: &'a [PaneInfo],
    pub(crate) split_borders: &'a [SplitBorder],
}

pub(crate) fn compute_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let target = app.active.and_then(|workspace_index| {
        let workspace = app.workspaces.get(workspace_index)?;
        Some(TabSurfaceTarget {
            workspace_index,
            tab_index: workspace.active_tab_index(),
            pane_focus: None,
        })
    });
    compute_tab_surface_for(
        app,
        terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    )
}

pub(crate) fn compute_tab_surface_for(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    target: Option<TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let tab = target.and_then(|target| {
        app.workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)
    });
    let split_borders = tab
        .map(|tab| {
            if tab.zoomed {
                Vec::new()
            } else {
                tab.layout.splits(area)
            }
        })
        .unwrap_or_default();
    let pane_infos = target.map_or_else(Vec::new, |target| {
        compute_tab_pane_infos(
            app,
            terminal_runtimes,
            target,
            area,
            resize_panes,
            cell_size,
            None,
        )
    });

    TabSurfaceLayout {
        target,
        pane_infos,
        split_borders,
    }
}

pub(crate) fn resize_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    workspace_index: usize,
    tab_index: usize,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let Some(tab) = app
        .workspaces
        .get(workspace_index)
        .and_then(|workspace| workspace.tabs.get(tab_index))
    else {
        return;
    };
    resize_tab_panes(app, terminal_runtimes, tab, area, cell_size);
}

pub(crate) fn render_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TargetTabSurfaceView<'_>,
    frame: &mut Frame,
) {
    let Some(target) = surface.target else {
        return;
    };
    let Some(tab) = app
        .workspaces
        .get(target.workspace_index)
        .and_then(|workspace| workspace.tabs.get(target.tab_index))
    else {
        return;
    };
    render_tab_panes(
        app,
        terminal_runtimes,
        frame,
        TabPaneRender {
            workspace_index: target.workspace_index,
            tab,
            pane_infos: surface.pane_infos,
            split_borders: surface.split_borders,
            local_overlays: false,
        },
    );
}

pub(crate) fn tab_surface_hyperlinks(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TargetTabSurfaceView<'_>,
) -> Vec<((u16, u16), String, String)> {
    let Some(ws_idx) = surface.target.map(|target| target.workspace_index) else {
        return Vec::new();
    };
    if app.workspaces.get(ws_idx).is_none() {
        return Vec::new();
    }

    let mut links = Vec::new();
    for info in surface.pane_infos {
        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            links.extend(runtime.visible_hyperlinks(info.inner_rect));
        }
    }
    links
}

pub(crate) fn tab_surface_cursor(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TargetTabSurfaceView<'_>,
) -> Option<CursorState> {
    let ws_idx = surface.target?.workspace_index;
    let info = surface.pane_infos.iter().find(|info| info.is_focused)?;
    if !app.pane_exposes_host_cursor(ws_idx, info.id) {
        return None;
    }
    let runtime = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let scrolled_back = super::panes::pane_is_scrolled_back(runtime);
    let reveal = app.reveal_hidden_cursor_for_cjk_ime
        && (!app.cjk_ime_agent_filter_configured || {
            let detected = app
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.tabs.get(surface.target?.tab_index))
                .and_then(|tab| tab.terminal_id(info.id))
                .and_then(|terminal_id| app.terminals.get(terminal_id))
                .and_then(|terminal| terminal.detected_agent);
            detected.is_some_and(|agent| app.cjk_ime_agents.contains(&agent))
        });

    if let Some(cursor) = runtime.cursor_state(info.inner_rect, true) {
        let visible = if reveal {
            !scrolled_back
        } else {
            cursor.visible && !scrolled_back
        };
        Some(CursorState {
            x: cursor.x,
            y: cursor.y,
            visible,
            shape: if reveal && visible {
                app.cjk_ime_cursor_shape
            } else {
                cursor.shape
            },
        })
    } else if reveal && !scrolled_back {
        Some(CursorState {
            x: info.inner_rect.x,
            y: info.inner_rect.y,
            visible: true,
            shape: app.cjk_ime_cursor_shape,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Direction;
    use ratatui::Terminal;

    #[tokio::test]
    async fn nonselected_tab_surface_preserves_local_focus_and_fullscreen_copy_geometry() {
        let mut workspace = Workspace::test_new("viewer");
        let second_tab = workspace.test_add_tab(Some("second"));
        for (tab_index, marker) in [
            (0, b"LOCAL-FIRST".as_slice()),
            (second_tab, b"VIEWER-SECOND"),
        ] {
            let tab = &mut workspace.tabs[tab_index];
            tab.runtimes.insert(
                tab.root_pane,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 8, marker),
            );
        }
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.copy_mode_fullscreen_pane = Some(app.workspaces[0].tabs[0].root_pane);
        let target = TabSurfaceTarget {
            workspace_index: 0,
            tab_index: second_tab,
            pane_focus: None,
        };
        let area = Rect::new(0, 0, 40, 8);
        let runtimes = TerminalRuntimeRegistry::new();
        let surface = compute_tab_surface_for(
            &app,
            &runtimes,
            Some(target),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 1);
        assert_eq!(
            surface.pane_infos[0].id,
            app.workspaces[0].tabs[second_tab].root_pane
        );
        assert_eq!(surface.pane_infos[0].inner_rect, Rect::new(1, 1, 37, 6));
        let view = TargetTabSurfaceView {
            target: surface.target,
            pane_infos: &surface.pane_infos,
            split_borders: &surface.split_borders,
        };
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_tab_surface(&app, &runtimes, view, frame))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("VIEWER-SECOND"), "{rendered:?}");
        assert!(!rendered.contains("LOCAL-FIRST"), "{rendered:?}");
        let cursor = tab_surface_cursor(&app, &runtimes, view).unwrap();
        assert!(cursor.x >= 1 && cursor.x < 38);
        assert!(cursor.y >= 1 && cursor.y < 7);
        assert_eq!(app.active, Some(0));
        assert_eq!(app.workspaces[0].active_tab_index(), 0);
        assert_eq!(
            app.copy_mode_fullscreen_pane,
            Some(app.workspaces[0].tabs[0].root_pane)
        );
    }

    #[test]
    fn explicit_surface_geometry_keeps_adversarial_global_identity_and_selection() {
        let app = AppState::test_with_adversarial_identity_state();
        app.assert_invariants_for_test();
        let runtimes = TerminalRuntimeRegistry::new();
        let active = app.active;
        let selected = app.selected;
        let tabs = app
            .workspaces
            .iter()
            .map(|workspace| workspace.active_tab_index())
            .collect::<Vec<_>>();
        for (workspace_index, workspace) in app.workspaces.iter().enumerate() {
            for (tab_index, tab) in workspace.tabs.iter().enumerate() {
                let target = Some(TabSurfaceTarget {
                    workspace_index,
                    tab_index,
                    pane_focus: None,
                });
                let area = Rect::new(9, 8, 106, 20);
                let surface = compute_tab_surface_for(
                    &app,
                    &runtimes,
                    target,
                    area,
                    false,
                    crate::kitty_graphics::HostCellSize::default(),
                );
                assert_eq!(surface.target, target);
                assert_eq!(
                    surface.pane_infos.len(),
                    if tab.zoomed {
                        1
                    } else {
                        tab.layout.pane_count()
                    }
                );
                for info in &surface.pane_infos {
                    assert!(tab.panes.contains_key(&info.id));
                    assert!(info.inner_rect.x >= area.x && info.inner_rect.y >= area.y);
                    assert!(info.inner_rect.right() <= area.right());
                    assert!(info.inner_rect.bottom() <= area.bottom());
                }
                app.assert_invariants_for_test();
            }
        }
        assert_eq!(app.active, active);
        assert_eq!(app.selected, selected);
        assert_eq!(
            app.workspaces
                .iter()
                .map(|workspace| workspace.active_tab_index())
                .collect::<Vec<_>>(),
            tabs
        );
    }

    #[tokio::test]
    async fn explicit_surface_layout_drives_render_cursor_and_hyperlinks() {
        let uri = "https://example.com/surface";
        let mut workspace = Workspace::test_new("shell-workspace");
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                20,
                8,
                format!("\x1b]8;;{uri}\x1b\\LEFT\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 8, b"RIGHT"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;

        let full_area = Rect::new(0, 0, 106, 20);
        let area = full_area;
        let surface = compute_tab_surface(
            &app,
            &TerminalRuntimeRegistry::new(),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 2);
        assert!(!surface.split_borders.is_empty());

        app.view.terminal_area = Rect::new(9, 8, 7, 6);
        app.view.pane_infos.clear();

        let surface_view = TargetTabSurfaceView {
            target: surface.target,
            pane_infos: &surface.pane_infos,
            split_borders: &surface.split_borders,
        };
        let mut terminal =
            Terminal::new(TestBackend::new(full_area.width, full_area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_tab_surface(&app, &TerminalRuntimeRegistry::new(), surface_view, frame)
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("LEFT"), "surface: {rendered:?}");
        assert!(rendered.contains("RIGHT"), "surface: {rendered:?}");
        assert!(!rendered.contains("shell-workspace"));

        let links = tab_surface_hyperlinks(&app, &TerminalRuntimeRegistry::new(), surface_view);
        assert!(links
            .iter()
            .any(|(_, symbol, link)| { symbol == "L" && link == uri }));
        assert!(tab_surface_cursor(&app, &TerminalRuntimeRegistry::new(), surface_view,).is_some());
    }
}
