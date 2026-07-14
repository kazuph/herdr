use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::scrollbar::{render_pane_scrollbar, should_show_scrollbar};
use super::widgets::panel_contrast_fg;
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::layout::PaneInfo;
use crate::terminal::{TerminalRuntime, TerminalState};

pub(crate) fn pane_is_scrolled_back(rt: &TerminalRuntime) -> bool {
    rt.scroll_metrics()
        .is_some_and(|metrics| metrics.offset_from_bottom > 0)
}

fn truncate_label(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

fn pane_border_title(label: &str, pane_width: u16) -> Option<String> {
    let label = label.trim();
    if label.is_empty() || pane_width <= 4 {
        return None;
    }
    let max_label_width = pane_width.saturating_sub(4) as usize;
    Some(format!(" {} ", truncate_label(label, max_label_width)))
}

fn pane_should_frame(area: Rect) -> bool {
    area.width > 2 && area.height > 2
}

fn stable_terminal_inner_rect(pane_inner: Rect) -> Rect {
    if pane_inner.width <= 4 {
        return pane_inner;
    }

    Rect::new(
        pane_inner.x,
        pane_inner.y,
        pane_inner.width.saturating_sub(1),
        pane_inner.height,
    )
}

fn pane_inner_rect(area: Rect) -> Rect {
    if pane_should_frame(area) {
        Block::default().borders(Borders::ALL).inner(area)
    } else {
        area
    }
}

fn command_label(argv: &[String]) -> Option<&str> {
    let command = argv.first()?.trim();
    if command.is_empty() {
        return None;
    }
    command
        .rsplit(['/', '\\'])
        .next()
        .filter(|label| !label.is_empty())
}

fn terminal_pane_label(terminal: &TerminalState) -> &str {
    terminal
        .manual_label
        .as_deref()
        .or(terminal.agent_name.as_deref())
        .or_else(|| terminal.effective_agent_label())
        .or_else(|| terminal.launch_argv.as_deref().and_then(command_label))
        .unwrap_or("terminal")
}

fn restore_status_label(terminal: Option<&TerminalState>) -> Option<&'static str> {
    let terminal = terminal?;
    let has_recorded_session = terminal
        .agent_session_id
        .as_deref()
        .is_some_and(crate::agent_sessions::is_safe_session_id)
        && terminal.agent_session_agent.is_some();
    let has_pending_restore = terminal.pending_restore.as_ref().is_some_and(|restore| {
        restore
            .session_id
            .as_deref()
            .is_some_and(crate::agent_sessions::is_safe_session_id)
            && crate::detect::parse_agent_label(&restore.agent).is_some()
    });

    let launched_agent = terminal
        .launch_argv
        .as_deref()
        .and_then(command_label)
        .and_then(crate::detect::parse_agent_label)
        .is_some();

    if has_recorded_session || has_pending_restore {
        Some("saved session")
    } else if terminal.is_agent_terminal()
        || launched_agent
        || terminal.agent_session_agent.is_some()
    {
        Some("no saved session")
    } else {
        None
    }
}

fn pane_title(
    pane_id: crate::layout::PaneId,
    terminal: Option<&TerminalState>,
    cwd: Option<&std::path::Path>,
    zoomed: bool,
) -> String {
    let label = terminal.map(terminal_pane_label).unwrap_or("terminal");
    let title = terminal.and_then(|terminal| {
        terminal
            .agent_task_title
            .as_deref()
            .or(terminal.pane_title.as_deref())
    });
    let branch = cwd.and_then(crate::workspace::git_branch);
    let mut parts = Vec::new();
    if zoomed {
        parts.push("ZOOM".to_string());
    }
    parts.extend([format!("%{}", pane_id.raw()), label.to_string()]);
    if let Some(title) = title {
        parts.push(title.to_string());
    }
    if let Some(branch) = branch {
        parts.push(branch);
    }
    parts.join(" ")
}

fn render_restore_status_label(
    frame: &mut Frame,
    area: Rect,
    terminal: Option<&TerminalState>,
    style: Style,
) {
    let Some(label) = restore_status_label(terminal) else {
        return;
    };
    if area.width < 34 || area.height == 0 {
        return;
    }
    let rect = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {label} "), style)))
            .alignment(Alignment::Right),
        rect,
    );
}

fn runtime_for_tab_pane<'a>(
    app: &'a AppState,
    tab: &'a crate::workspace::Tab,
    pane_id: crate::layout::PaneId,
) -> Option<(&'a crate::terminal::TerminalId, &'a TerminalRuntime)> {
    let terminal_id = tab.terminal_id(pane_id)?;
    #[cfg(test)]
    if let Some(runtime) = tab.runtimes.get(&pane_id) {
        return Some((terminal_id, runtime));
    }
    app.terminal_runtimes
        .get(terminal_id)
        .map(|runtime| (terminal_id, runtime))
}

fn stable_scrollbar_gutter(rt: &TerminalRuntime, pane_inner: Rect) -> (Rect, Option<Rect>) {
    let inner_rect = stable_terminal_inner_rect(pane_inner);
    if inner_rect == pane_inner {
        return (inner_rect, None);
    }
    let gutter = Rect::new(
        pane_inner.x + pane_inner.width.saturating_sub(1),
        pane_inner.y,
        1,
        pane_inner.height,
    );
    let scrollbar_rect = rt
        .scroll_metrics()
        .filter(|metrics| should_show_scrollbar(*metrics))
        .map(|_| gutter);

    (inner_rect, scrollbar_rect)
}

/// Resize every visible runtime in a tab to the geometry it would receive if the tab were selected.
pub(super) fn resize_tab_panes(
    app: &AppState,
    tab: &crate::workspace::Tab,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    if tab.zoomed {
        let focused_id = tab.layout.focused();
        if let Some((terminal_id, rt)) = runtime_for_tab_pane(app, tab, focused_id) {
            let pane_inner = pane_inner_rect(area);
            let inner_rect = stable_terminal_inner_rect(pane_inner);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
        return;
    }

    for info in tab.layout.panes(area) {
        let pane_inner = pane_inner_rect(info.rect);

        if let Some((terminal_id, rt)) = runtime_for_tab_pane(app, tab, info.id) {
            let inner_rect = stable_terminal_inner_rect(pane_inner);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
    }
}

/// Compute pane layout info and optionally resize pane runtimes to match.
pub(super) fn compute_pane_infos(
    app: &AppState,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Vec<PaneInfo> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return Vec::new();
    };

    let terminal_active = app.mode == Mode::Terminal;

    if let Some(fullscreen_pane) = app.copy_mode_fullscreen_pane {
        if ws
            .active_tab()
            .is_some_and(|tab| tab.panes.contains_key(&fullscreen_pane))
        {
            if let Some(rt) = app.runtime_for_pane_in_workspace(ws_idx, fullscreen_pane) {
                if resize_panes
                    && ws.terminal_id(fullscreen_pane).is_some_and(|terminal_id| {
                        !app.direct_attach_resize_locks.contains(terminal_id)
                    })
                {
                    rt.resize(
                        area.height,
                        area.width,
                        cell_size.width_px,
                        cell_size.height_px,
                    );
                }
            }
            return vec![PaneInfo {
                id: fullscreen_pane,
                rect: area,
                inner_rect: area,
                scrollbar_rect: None,
                is_focused: true,
            }];
        }
    }

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        let pane_inner = pane_inner_rect(area);
        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(ws_idx, focused_id) {
            (inner_rect, scrollbar_rect) = stable_scrollbar_gutter(rt, pane_inner);
            if resize_panes
                && ws.terminal_id(focused_id).is_some_and(|terminal_id| {
                    !app.direct_attach_resize_locks.contains(terminal_id)
                })
            {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
        return vec![PaneInfo {
            id: focused_id,
            rect: area,
            inner_rect,
            scrollbar_rect,
            is_focused: true,
        }];
    }

    let mut pane_infos = ws.layout.panes(area);
    let pane_local_overlay = app.pane_local_overlay.and_then(|overlay| {
        ws.active_tab().and_then(|tab| {
            if tab.panes.contains_key(&overlay.pane_id)
                && tab.panes.contains_key(&overlay.target_pane_id)
            {
                pane_infos
                    .iter()
                    .find(|info| info.id == overlay.target_pane_id)
                    .cloned()
                    .map(|target_info| (overlay, target_info))
            } else {
                None
            }
        })
    });
    if let Some((overlay, _)) = &pane_local_overlay {
        pane_infos.retain(|info| info.id != overlay.target_pane_id);
    }

    for info in &mut pane_infos {
        let border_set = if info.is_focused && terminal_active {
            ratatui::symbols::border::THICK
        } else {
            ratatui::symbols::border::PLAIN
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(border_set);
        let pane_inner = if pane_should_frame(info.rect) {
            block.inner(info.rect)
        } else {
            info.rect
        };

        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(ws_idx, info.id) {
            (inner_rect, scrollbar_rect) = stable_scrollbar_gutter(rt, pane_inner);
            if resize_panes
                && ws.terminal_id(info.id).is_some_and(|terminal_id| {
                    !app.direct_attach_resize_locks.contains(terminal_id)
                })
            {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }

        info.inner_rect = inner_rect;
        info.scrollbar_rect = scrollbar_rect;
    }

    if let Some((overlay, target_info)) = pane_local_overlay {
        let pane_inner = pane_inner_rect(target_info.rect);
        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(ws_idx, overlay.pane_id) {
            (inner_rect, scrollbar_rect) = stable_scrollbar_gutter(rt, pane_inner);
            if resize_panes
                && ws.terminal_id(overlay.pane_id).is_some_and(|terminal_id| {
                    !app.direct_attach_resize_locks.contains(terminal_id)
                })
            {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
        pane_infos.push(PaneInfo {
            id: overlay.pane_id,
            rect: target_info.rect,
            inner_rect,
            scrollbar_rect,
            is_focused: true,
        });
    }

    pane_infos
}

pub(super) fn render_panes(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(ws_idx) = app.active else {
        render_empty(app, frame, area);
        return;
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        render_empty(app, frame, area);
        return;
    };

    let multi_pane = ws.layout.pane_count() > 1;
    let terminal_active = app.mode == Mode::Terminal;
    let fullscreen_copy_pane = app.copy_mode_fullscreen_pane;

    for info in &app.view.pane_infos {
        if let Some(rt) = app.runtime_for_pane_in_workspace(ws_idx, info.id) {
            if fullscreen_copy_pane != Some(info.id) && pane_should_frame(info.rect) {
                let (border_style, border_set) = if info.is_focused && terminal_active {
                    (
                        Style::default().fg(app.palette.accent),
                        ratatui::symbols::border::THICK,
                    )
                } else if info.is_focused {
                    (
                        Style::default().fg(app.palette.accent),
                        ratatui::symbols::border::PLAIN,
                    )
                } else {
                    (
                        Style::default().fg(app.palette.overlay0),
                        ratatui::symbols::border::PLAIN,
                    )
                };

                let mut block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .border_set(border_set);
                let terminal = ws
                    .pane_state(info.id)
                    .and_then(|pane| app.terminals.get(&pane.attached_terminal_id));
                let runtime_cwd = rt.cwd();
                let title_cwd = runtime_cwd
                    .as_deref()
                    .or_else(|| terminal.map(|terminal| terminal.cwd.as_path()));
                let is_zoomed_title = ws.zoomed && info.is_focused;
                if let Some(title) = pane_border_title(
                    &pane_title(info.id, terminal, title_cwd, is_zoomed_title),
                    info.rect.width,
                ) {
                    block = block.title(Line::from(Span::styled(title, border_style)));
                }
                frame.render_widget(block, info.rect);
                render_restore_status_label(frame, info.rect, terminal, border_style);
            }

            let show_cursor = info.is_focused && terminal_active && !pane_is_scrolled_back(rt);
            rt.render(frame, info.inner_rect, show_cursor);
            render_pane_scrollbar(app, frame, info, rt);

            let should_dim = !info.is_focused && multi_pane && !terminal_active;
            if should_dim {
                let inner = info.inner_rect;
                let buf = frame.buffer_mut();
                for y in inner.y..inner.y + inner.height {
                    for x in inner.x..inner.x + inner.width {
                        let cell = &mut buf[(x, y)];
                        cell.set_style(cell.style().add_modifier(Modifier::DIM));
                    }
                }
            }

            render_selection_highlight(
                &app.selection,
                frame,
                info.id,
                info.inner_rect,
                rt.scroll_metrics(),
                &app.palette,
            );
            render_copy_mode_cursor(app, frame, info);
        }
    }
}

fn render_copy_mode_cursor(app: &AppState, frame: &mut Frame, info: &PaneInfo) {
    if app.mode != Mode::Copy {
        return;
    }
    let Some(copy_mode) = app.copy_mode else {
        return;
    };
    if copy_mode.pane_id != info.id
        || copy_mode.cursor_row >= info.inner_rect.height
        || copy_mode.cursor_col >= info.inner_rect.width
    {
        return;
    }

    let x = info.inner_rect.x + copy_mode.cursor_col;
    let y = info.inner_rect.y + copy_mode.cursor_row;
    let style = Style::default()
        .fg(app.palette.panel_bg)
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    frame.buffer_mut()[(x, y)].set_style(style);
}

fn render_selection_highlight(
    selection: &Option<crate::selection::Selection>,
    frame: &mut Frame,
    pane_id: crate::layout::PaneId,
    inner: Rect,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    p: &Palette,
) {
    if let Some(sel) = selection {
        if sel.is_visible() && sel.pane_id == pane_id {
            let buf = frame.buffer_mut();
            for y in 0..inner.height {
                for x in 0..inner.width {
                    if sel.contains(y, x, scroll_metrics) {
                        let cell = &mut buf[(inner.x + x, inner.y + y)];
                        cell.set_style(Style::default().fg(panel_contrast_fg(p)).bg(p.blue));
                    }
                }
            }
        }
    }
}

fn render_empty(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  No workspaces yet",
            Style::default().fg(p.overlay0),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  A workspace is one project context.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(Span::styled(
            "  Its root pane (top-left) sets the default repo or folder name.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(p.overlay0)),
            Span::styled(
                app.keybinds
                    .new_workspace
                    .label()
                    .unwrap_or_else(|| "unset".to_string()),
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to create one", Style::default().fg(p.overlay0)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(p.surface_dim)),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::terminal::TerminalId;
    use crate::terminal::TerminalRuntime;
    use crate::workspace::Workspace;

    #[test]
    fn pane_border_title_trims_and_truncates() {
        assert_eq!(
            pane_border_title(" claude ", 20).as_deref(),
            Some(" claude ")
        );
        assert_eq!(pane_border_title("", 20), None);
        assert_eq!(pane_border_title("abcdef", 8).as_deref(), Some(" abc… "));
        assert_eq!(pane_border_title("abcdef", 4), None);
    }

    #[test]
    fn pane_title_uses_id_and_best_available_label() {
        let pane_id = crate::layout::PaneId::from_raw(5);
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);

        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%5 codex"
        );

        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%5 claude"
        );

        terminal.set_agent_name("agent-one".into());
        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%5 agent-one"
        );

        terminal.set_manual_label(" reviewer ".into());
        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%5 reviewer"
        );

        assert_eq!(pane_title(pane_id, None, None, false), "%5 terminal");
    }

    #[test]
    fn pane_title_prefixes_zoom_state() {
        let pane_id = crate::layout::PaneId::from_raw(5);

        assert_eq!(pane_title(pane_id, None, None, true), "ZOOM %5 terminal");
    }

    #[test]
    fn pane_title_appends_osc_title_after_agent_label() {
        let pane_id = crate::layout::PaneId::from_raw(81);
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);

        terminal.set_pane_title(Some("thinking".into()));

        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%81 codex thinking"
        );
    }

    #[test]
    fn pane_title_prefers_agent_task_title_over_osc_title() {
        let pane_id = crate::layout::PaneId::from_raw(81);
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);

        terminal.set_pane_title(Some("herdr".into()));
        terminal.set_agent_task_title(Some("restore pane sessions".into()));

        assert_eq!(
            pane_title(pane_id, Some(&terminal), None, false),
            "%81 codex restore pane sessions"
        );
    }

    #[test]
    fn restore_status_label_marks_agent_session_state() {
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);

        assert_eq!(
            restore_status_label(Some(&terminal)),
            Some("no saved session")
        );

        terminal.agent_session_id = Some("019ef3a2-749c-7b52-b324-2c20cb0b2379".into());
        terminal.agent_session_agent = Some(Agent::Codex);
        assert_eq!(restore_status_label(Some(&terminal)), Some("saved session"));

        assert_eq!(restore_status_label(None), None);
    }

    #[test]
    fn render_restore_status_label_draws_on_right_edge() {
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);
        terminal.agent_session_id = Some("019ef3a2-749c-7b52-b324-2c20cb0b2379".into());
        terminal.agent_session_agent = Some(Agent::Codex);

        let backend = ratatui::backend::TestBackend::new(50, 3);
        let mut terminal_ui = ratatui::Terminal::new(backend).unwrap();
        terminal_ui
            .draw(|frame| {
                let area = Rect::new(0, 0, 50, 3);
                frame.render_widget(Block::default().borders(Borders::ALL), area);
                render_restore_status_label(frame, area, Some(&terminal), Style::default());
            })
            .unwrap();

        let top_row = (0..50)
            .map(|x| terminal_ui.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert!(top_row.ends_with(" saved session ┐"), "row: {top_row:?}");
    }

    #[test]
    fn pane_title_appends_git_branch_after_osc_title() {
        let pane_id = crate::layout::PaneId::from_raw(81);
        let terminal_id = TerminalId::alloc();
        let mut terminal = TerminalState::new(terminal_id, "/tmp".into())
            .with_launch_argv(vec!["/usr/local/bin/codex".into()]);
        terminal.set_pane_title(Some("thinking".into()));
        let repo = std::env::temp_dir().join(format!(
            "herdr-pane-title-branch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();

        assert_eq!(
            pane_title(pane_id, Some(&terminal), Some(&repo), false),
            "%81 codex thinking feature"
        );

        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn zoomed_pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        workspace.zoomed = true;
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn zoomed_multi_pane_keeps_border_space() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let focused_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.zoomed = true;
        workspace.tabs[0].runtimes.insert(
            focused_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.id, focused_pane);
        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn pane_local_overlay_replaces_only_target_pane_rect() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let target_pane = workspace.tabs[0].root_pane;
        let other_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let overlay_pane = crate::layout::PaneId::alloc();
        let overlay_terminal = crate::terminal::TerminalId::alloc();
        workspace.tabs[0]
            .panes
            .insert(overlay_pane, crate::pane::PaneState::new(overlay_terminal));
        workspace.tabs[0].runtimes.insert(
            overlay_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"overlay\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.pane_local_overlay = Some(crate::app::state::PaneLocalOverlay {
            pane_id: overlay_pane,
            target_pane_id: target_pane,
        });

        let area = Rect::new(0, 0, 100, 20);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );

        assert_eq!(infos.len(), 2);
        assert!(infos.iter().any(|info| info.id == other_pane));
        assert!(!infos.iter().any(|info| info.id == target_pane));
        let overlay_info = infos
            .iter()
            .find(|info| info.id == overlay_pane)
            .expect("overlay pane should render");
        let other_info = infos
            .iter()
            .find(|info| info.id == other_pane)
            .expect("other pane should remain visible");
        assert_eq!(overlay_info.rect, Rect::new(0, 0, 50, 20));
        assert_eq!(other_info.rect, Rect::new(50, 0, 50, 20));
    }

    #[tokio::test]
    async fn tiny_pane_does_not_reserve_scrollbar_gutter() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(4, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 4, 8);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 2, 6));
    }

    #[tokio::test]
    async fn pane_scrollbar_reserves_last_column_from_terminal_area() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(
                40,
                8,
                1024,
                b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
            ),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, Some(Rect::new(48, 4, 1, 6)));
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn single_pane_renders_frame_with_id_and_label() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.insert_test_runtime(
            root_pane,
            TerminalRuntime::test_with_screen_bytes(20, 5, b"ready"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].terminal_id(root_pane).unwrap().clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("reviewer".into());

        let area = Rect::new(0, 0, 32, 8);
        app.view.pane_infos = compute_pane_infos(
            &app,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );

        let backend = ratatui::backend::TestBackend::new(32, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_panes(&app, frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let top_row = (0..area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();

        assert!(top_row.contains(&format!("%{} reviewer", root_pane.raw())));
        assert_ne!(buffer[(0, 0)].symbol(), " ");
    }
}
