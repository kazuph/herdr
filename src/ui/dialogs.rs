use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use super::text::{display_width_u16, truncate_end};
use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, render_panel_shell, ActionButtonSpec,
};
use crate::app::{
    state::{Palette, WorktreeCreateState, WorktreeOpenState, WorktreeRemoveState},
    AppState, Mode,
};

const NEW_LINKED_WORKTREE_POPUP_WIDTH: u16 = 68;
const NEW_LINKED_WORKTREE_POPUP_HEIGHT: u16 = 12;

pub(crate) fn rename_button_rects(inner: Rect) -> (Rect, Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "save",
            },
            ActionButtonSpec {
                hint: Some("^c"),
                label: "clear",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        3,
    );
    (rects[0], rects[1], rects[2])
}

pub(super) fn render_rename_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let title = match app.mode {
        Mode::RenameWorkspace => "rename workspace",
        Mode::RenameTab if app.creating_new_tab => "new tab",
        Mode::RenameTab => "rename tab",
        Mode::RenamePane => "rename pane",
        _ => return,
    };

    render_rename_dialog(frame, area, title, &app.name_input, &app.palette);
}

pub(crate) fn render_rename_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &str,
    palette: &crate::app::state::Palette,
) {
    render_input_dialog(frame, area, title, input, palette, false);
}

pub(crate) fn render_pending_input_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &str,
    palette: &crate::app::state::Palette,
) {
    render_input_dialog(frame, area, title, input, palette, true);
}

fn render_input_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &str,
    palette: &crate::app::state::Palette,
    pending: bool,
) {
    super::dim_background(frame, area);

    let Some(inner) = render_modal_shell(frame, area, 56, 7, palette) else {
        return;
    };
    if inner.height < 4 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<5>(inner);

    render_modal_header(frame, rows[0], title, palette);

    let input_rect = Rect::new(rows[2].x, rows[2].y, rows[2].width, 1);
    frame.render_widget(Clear, input_rect);
    frame.render_widget(
        Paragraph::new(format!(" {input}"))
            .style(Style::default().fg(palette.text).bg(palette.surface0)),
        input_rect,
    );

    let (save_rect, clear_rect, cancel_rect) = rename_button_rects(inner);

    if !pending {
        render_action_button(
            frame,
            save_rect,
            Some("↵"),
            "save",
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            clear_rect,
            Some("^c"),
            "clear",
            Style::default()
                .fg(palette.text)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(crate) fn new_linked_worktree_inner_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(
        area,
        NEW_LINKED_WORKTREE_POPUP_WIDTH,
        NEW_LINKED_WORKTREE_POPUP_HEIGHT,
    )
    .map(|popup| {
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    })
}

pub(crate) fn new_linked_worktree_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "create and open",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (rects[0], rects[1])
}

pub(crate) fn remove_worktree_popup_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 72, 10)
}

pub(crate) fn remove_worktree_button_rects(inner: Rect, force_confirmation: bool) -> (Rect, Rect) {
    let primary_label = if force_confirmation {
        "delete anyway"
    } else {
        "remove"
    };
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: primary_label,
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (rects[0], rects[1])
}

pub(crate) fn open_existing_worktree_inner_rect(area: Rect, entry_count: usize) -> Option<Rect> {
    let height = (entry_count as u16)
        .saturating_mul(2)
        .saturating_add(7)
        .clamp(12, 26);
    centered_popup_rect(area, 96, height).map(|popup| {
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    })
}

pub(crate) fn open_existing_worktree_max_visible_rows(inner: Rect) -> usize {
    usize::from(inner.height.saturating_sub(5) / 2)
}

pub(crate) fn open_existing_worktree_visible_start(
    open: &WorktreeOpenState,
    max_rows: usize,
) -> usize {
    let filtered = open.filtered_indices();
    let selected = open.selected_entry_index().unwrap_or(open.selected);
    let selected_pos = filtered
        .iter()
        .position(|idx| *idx == selected)
        .unwrap_or(0);
    selected_pos.saturating_sub(max_rows.saturating_sub(1))
}

pub(crate) fn open_existing_worktree_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "open",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (rects[0], rects[1])
}

pub(super) fn render_new_linked_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    if let Some(create) = app.worktree_create.as_ref() {
        render_worktree_create(create, &app.name_input, &app.palette, frame, area);
    }
}

pub(crate) fn render_worktree_create(
    create: &WorktreeCreateState,
    name_input: &str,
    palette: &Palette,
    frame: &mut Frame,
    area: Rect,
) {
    super::dim_background(frame, area);
    let Some(inner) = render_modal_shell(
        frame,
        area,
        NEW_LINKED_WORKTREE_POPUP_WIDTH,
        NEW_LINKED_WORKTREE_POPUP_HEIGHT,
        palette,
    ) else {
        return;
    };
    if inner.height < 9 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<8>(inner);

    render_modal_header(frame, rows[0], "new worktree", palette);

    frame.render_widget(
        Paragraph::new(" branch").style(Style::default().fg(palette.overlay0)),
        rows[1],
    );
    let input_rect = Rect::new(rows[2].x, rows[2].y, rows[2].width, 1);
    frame.render_widget(Clear, input_rect);
    frame.render_widget(
        Paragraph::new(format!(" {}█", name_input))
            .style(Style::default().fg(palette.text).bg(palette.surface0)),
        input_rect,
    );

    let checkout = create.checkout_path.display().to_string();
    frame.render_widget(
        Paragraph::new(" checkout").style(Style::default().fg(palette.overlay0)),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(format!(" {checkout}")).style(Style::default().fg(palette.subtext0)),
        rows[4],
    );

    if create.creating {
        frame.render_widget(
            Paragraph::new(" creating…").style(Style::default().fg(palette.overlay0)),
            rows[5],
        );
    } else if let Some(error) = &create.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}"))
                .style(Style::default().fg(palette.red))
                .wrap(Wrap { trim: false }),
            rows[5],
        );
    }

    let (create_rect, cancel_rect) = new_linked_worktree_button_rects(inner);
    render_action_button(
        frame,
        create_rect,
        Some("↵"),
        "create and open",
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(super) fn render_remove_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    if let Some(remove) = app.worktree_remove.as_ref() {
        render_worktree_remove(remove, &app.palette, frame, area);
    }
}

pub(crate) fn render_worktree_remove(
    remove: &WorktreeRemoveState,
    palette: &Palette,
    frame: &mut Frame,
    area: Rect,
) {
    super::dim_background(frame, area);
    let Some(popup) = remove_worktree_popup_rect(area) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, palette.red, palette.panel_bg) else {
        return;
    };

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<8>(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " delete worktree checkout?",
            Style::default()
                .fg(palette.red)
                .add_modifier(Modifier::BOLD),
        )])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(" This removes the checkout folder:")
            .style(Style::default().fg(palette.overlay0)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!(" {}", remove.path.display()))
            .style(Style::default().fg(palette.text)),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(" The branch is not deleted. The Herdr workspace will close.")
            .style(Style::default().fg(palette.overlay0)),
        rows[3],
    );
    if remove.force_confirmation {
        frame.render_widget(
            Paragraph::new(" Dirty or untracked files will be permanently deleted.")
                .style(Style::default().fg(palette.red)),
            rows[4],
        );
    }
    if remove.removing {
        frame.render_widget(
            Paragraph::new(" removing…").style(Style::default().fg(palette.overlay0)),
            rows[5],
        );
    } else if let Some(error) = &remove.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}")).style(Style::default().fg(palette.red)),
            rows[5],
        );
    }

    let (remove_rect, cancel_rect) = remove_worktree_button_rects(inner, remove.force_confirmation);
    let remove_label = if remove.force_confirmation {
        "delete anyway"
    } else {
        "remove"
    };
    render_action_button(
        frame,
        remove_rect,
        Some("↵"),
        remove_label,
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.red)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(super) fn render_open_existing_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    if let Some(open) = app.worktree_open.as_ref() {
        render_worktree_open(open, &app.palette, frame, area);
    }
}

pub(crate) fn render_worktree_open(
    open: &WorktreeOpenState,
    palette: &Palette,
    frame: &mut Frame,
    area: Rect,
) {
    super::dim_background(frame, area);
    let height = (open.entries.len() as u16)
        .saturating_mul(2)
        .saturating_add(7)
        .clamp(12, 26);
    let Some(inner) = render_modal_shell(frame, area, 96, height, palette) else {
        return;
    };
    if inner.height < 8 {
        return;
    }

    render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "open worktree",
        palette,
    );
    render_open_worktree_search(
        palette,
        frame,
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
        open,
    );
    frame.render_widget(
        Paragraph::new("─".repeat(inner.width as usize))
            .style(Style::default().fg(palette.surface1)),
        Rect::new(inner.x, inner.y.saturating_add(2), inner.width, 1),
    );

    let filtered = open.filtered_indices();
    let max_rows = open_existing_worktree_max_visible_rows(inner);
    let start = open_existing_worktree_visible_start(open, max_rows);
    for (visible_idx, entry_idx) in filtered.iter().skip(start).take(max_rows).enumerate() {
        let Some(entry) = open.entries.get(*entry_idx) else {
            continue;
        };
        let selected = Some(*entry_idx) == open.selected_entry_index();
        let y = inner.y.saturating_add(3 + (visible_idx as u16 * 2));
        let marker = if selected { "›" } else { " " };
        let row_style = if selected {
            Style::default()
                .fg(palette.text)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.subtext0)
        };
        let path_style = if selected {
            Style::default().fg(palette.subtext0).bg(palette.surface0)
        } else {
            Style::default().fg(palette.overlay0)
        };
        let status = entry.status_label();
        let title_width = inner
            .width
            .saturating_sub(display_width_u16(status))
            .saturating_sub(4) as usize;
        let mut title = format!(
            "{marker} {}",
            truncate_end(&entry.display_name(), title_width)
        );
        if !status.is_empty() {
            let pad = inner
                .width
                .saturating_sub(display_width_u16(&title))
                .saturating_sub(display_width_u16(status))
                .max(1);
            title.push_str(&" ".repeat(pad as usize));
            title.push_str(status);
        }
        frame.render_widget(
            Paragraph::new(truncate_end(&title, inner.width as usize)).style(row_style),
            Rect::new(inner.x, y, inner.width, 1),
        );
        frame.render_widget(
            Paragraph::new(truncate_end(
                &format!("  {}", entry.path.display()),
                inner.width as usize,
            ))
            .style(path_style),
            Rect::new(inner.x, y.saturating_add(1), inner.width, 1),
        );
    }

    if filtered.is_empty() {
        frame.render_widget(
            Paragraph::new(" no matching worktrees").style(Style::default().fg(palette.overlay0)),
            Rect::new(inner.x, inner.y.saturating_add(3), inner.width, 1),
        );
    }

    if let Some(error) = &open.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}")).style(Style::default().fg(palette.red)),
            Rect::new(
                inner.x,
                inner.y + inner.height.saturating_sub(2),
                inner.width,
                1,
            ),
        );
    }

    let (open_rect, cancel_rect) = open_existing_worktree_button_rects(inner);
    render_action_button(
        frame,
        open_rect,
        Some("↵"),
        "open",
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_open_worktree_search(
    palette: &Palette,
    frame: &mut Frame,
    area: Rect,
    open: &WorktreeOpenState,
) {
    let focus_style = if open.search_focused {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.overlay0)
    };
    let filtered_count = open.filtered_indices().len();
    let count = if open.query.trim().is_empty() {
        format!("{} checkouts", open.entries.len())
    } else {
        format!("{filtered_count}/{} checkouts", open.entries.len())
    };
    let mut spans = vec![Span::styled(" / ", focus_style)];
    if open.query.trim().is_empty() {
        spans.push(Span::styled(
            "filter worktrees",
            Style::default().fg(palette.overlay0),
        ));
    } else {
        spans.push(Span::styled(
            open.query.clone(),
            Style::default().fg(palette.text),
        ));
    }
    spans.push(Span::styled(
        format!(
            "{count:>width$}",
            width = area.width.saturating_sub(18) as usize
        ),
        Style::default().fg(palette.overlay0),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn confirm_close_overlay_text(app: &AppState) -> (String, String) {
    let ws_name = app
        .workspaces
        .get(app.selected)
        .map(|ws| ws.display_name())
        .unwrap_or_else(|| "?".to_string());
    let selected_space = app
        .workspaces
        .get(app.selected)
        .and_then(|ws| ws.worktree_space());
    let group_member_indices = selected_space
        .filter(|space| !space.is_linked_worktree)
        .map(|space| {
            app.workspaces
                .iter()
                .enumerate()
                .filter_map(|(idx, ws)| {
                    ws.worktree_space()
                        .is_some_and(|member| member.key == space.key)
                        .then_some(idx)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let closes_group = group_member_indices.len() > 1;
    let pane_count = if closes_group {
        group_member_indices
            .iter()
            .filter_map(|idx| app.workspaces.get(*idx))
            .map(|ws| ws.layout.pane_count())
            .sum()
    } else {
        app.workspaces
            .get(app.selected)
            .map(|ws| ws.layout.pane_count())
            .unwrap_or(0)
    };

    let pane_text = if pane_count == 1 {
        "1 pane".to_string()
    } else {
        format!("{pane_count} panes")
    };
    let workspace_text = if closes_group {
        let count = group_member_indices.len();
        if count == 1 {
            "1 workspace, ".to_string()
        } else {
            format!("{count} workspaces, ")
        }
    } else {
        String::new()
    };

    let title = if closes_group {
        "Close worktree group?"
    } else {
        "Close workspace?"
    };
    let detail = format!("{ws_name} — {workspace_text}{pane_text}");
    (title.to_string(), detail)
}

pub(super) fn render_confirm_close_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let (title, detail) = confirm_close_overlay_text(app);
    render_confirm_close_dialog(frame, area, &title, &detail, &app.palette);
}

pub(crate) fn render_confirm_close_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    detail: &str,
    palette: &crate::app::state::Palette,
) {
    super::dim_background(frame, area);

    let Some(popup) = confirm_close_popup_rect(area) else {
        return;
    };

    let warn = Style::default()
        .fg(palette.red)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette.overlay0);

    let title_line = Line::from(vec![Span::styled(format!(" {title}"), warn)]);

    let detail_line = Line::from(vec![
        Span::styled(
            format!(" {}", detail.split(" — ").next().unwrap_or(detail)),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            detail
                .split_once(" — ")
                .map(|(_, rest)| format!(" — {rest}"))
                .unwrap_or_default(),
            dim,
        ),
    ]);

    let Some(inner) = render_panel_shell(frame, popup, palette.red, palette.panel_bg) else {
        return;
    };

    if inner.height >= 3 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas::<4>(inner);

        frame.render_widget(Paragraph::new(title_line), rows[0]);
        frame.render_widget(Paragraph::new(detail_line), rows[1]);

        let (confirm_rect, cancel_rect) = confirm_close_button_rects(inner);
        render_action_button(
            frame,
            confirm_rect,
            Some("↵"),
            "confirm",
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.red)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            cancel_rect,
            Some("esc"),
            "cancel",
            Style::default()
                .fg(palette.text)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
}

pub(super) fn render_confirm_danger_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(action) = app.dangerous_action else {
        return;
    };
    let missing_sessions = if action == crate::app::state::DangerousAction::Restart {
        app.restart_missing_agent_sessions()
            .iter()
            .map(crate::api::schema::AgentSessionWarningInfo::from)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    render_confirm_danger_from(frame, area, &app.palette, action, &missing_sessions);
}

pub(crate) fn render_confirm_danger_from(
    frame: &mut Frame,
    area: Rect,
    palette: &crate::app::state::Palette,
    action: crate::app::state::DangerousAction,
    missing_sessions: &[crate::api::schema::AgentSessionWarningInfo],
) {
    let visible_count = missing_sessions.len().min(10);

    super::dim_background(frame, area);

    let Some(popup) = confirm_danger_popup_rect(area, visible_count) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, palette.red, palette.panel_bg) else {
        return;
    };

    let warn = Style::default()
        .fg(palette.red)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette.overlay0);
    let title = if missing_sessions.is_empty() {
        action.title()
    } else {
        "Restart with missing agent sessions?"
    };
    let detail = if missing_sessions.is_empty() {
        action.detail().to_string()
    } else {
        "These AI panes do not have a recorded session id:".to_string()
    };

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(visible_count as u16),
        Constraint::Length(1),
    ])
    .areas::<4>(inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {title}"), warn))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {detail}"), dim))),
        rows[1],
    );
    for (idx, info) in missing_sessions.iter().take(visible_count).enumerate() {
        let row = Rect::new(rows[2].x, rows[2].y + idx as u16, rows[2].width, 1);
        let title = info.title.as_deref().unwrap_or("-");
        let text = format!(
            " space {} {} pane {} {} title={} cwd={} reason={}",
            info.workspace_number,
            info.workspace_label,
            info.pane_label,
            info.agent,
            title,
            info.cwd,
            info.reason
        );
        frame.render_widget(
            Paragraph::new(truncate_end(&text, row.width as usize))
                .style(Style::default().fg(palette.text)),
            row,
        );
    }

    let (confirm_rect, cancel_rect) = confirm_danger_button_rects(inner);
    render_action_button(
        frame,
        confirm_rect,
        Some("↵"),
        action.confirm_label(),
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.red)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(crate) fn confirm_close_popup_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 64, 6)
}

pub(crate) fn confirm_close_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "confirm",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        3,
    );
    (rects[0], rects[1])
}

pub(crate) fn confirm_danger_popup_rect(area: Rect, missing_session_count: usize) -> Option<Rect> {
    centered_popup_rect(
        area,
        76,
        5u16.saturating_add(missing_session_count.min(10) as u16),
    )
}

pub(crate) fn confirm_danger_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "confirm",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (rects[0], rects[1])
}

const DECISION_DIALOG_WIDTH: u16 = 64;
const DECISION_DIALOG_MAX_HEIGHT: u16 = 22;

/// Render facts for one pending decision, already resolved for display by the
/// caller (machine label, queue position, remaining time).
pub(crate) struct DecisionDialogFacts {
    pub machine: String,
    /// 1-based position inside the pending queue.
    pub position: usize,
    pub total: usize,
    pub title: String,
    pub body: Option<String>,
    /// `(label, value)` origin rows such as `("pane", "p1")`.
    pub origin: Vec<(String, String)>,
    pub options: Vec<String>,
    /// Option highlighted for keyboard activation; only used when
    /// `keyboard_active` is set.
    pub selected: usize,
    pub allow_text: bool,
    pub text: String,
    /// Remaining time such as `"42s"`, when the decision expires.
    pub remaining: Option<String>,
    /// Scroll offset of the content band (title/body/origin).
    pub scroll: u16,
    /// False while keyboard input is still going to the pane (before the first
    /// click inside the dialog).
    pub keyboard_active: bool,
}

/// Geometry shared by the renderer and the mouse hit-test. The content band is
/// a scroll region; option rows sit at the bottom so long bodies never push
/// the answers off screen.
pub(crate) struct DecisionDialogRects {
    pub popup: Rect,
    pub content: Rect,
    pub text: Option<Rect>,
    pub options: Vec<Rect>,
    pub close: Rect,
}

pub(crate) fn decision_dialog_rects(
    area: Rect,
    facts: &DecisionDialogFacts,
    palette: &Palette,
) -> Option<DecisionDialogRects> {
    // Fit the popup to the content (title/body/origin/timeout + text row +
    // options + footer); only when the content outgrows the cap does the
    // middle band scroll.
    let popup_w = DECISION_DIALOG_WIDTH.min(area.width.saturating_sub(4));
    let content_width = popup_w.saturating_sub(2);
    if content_width == 0 {
        return None;
    }
    let content_lines = Paragraph::new(decision_dialog_content_lines(facts, palette))
        .wrap(Wrap { trim: false })
        .line_count(content_width) as u16;
    let inner_needed = 1u16
        .saturating_add(content_lines.max(1))
        .saturating_add(u16::from(facts.allow_text))
        .saturating_add(facts.options.len().max(1) as u16)
        .saturating_add(1);
    let popup_h = inner_needed
        .saturating_add(2)
        .min(DECISION_DIALOG_MAX_HEIGHT)
        .max(4);
    let popup = centered_popup_rect(area, DECISION_DIALOG_WIDTH, popup_h)?;
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let mut constraints = vec![Constraint::Length(1), Constraint::Min(1)];
    if facts.allow_text {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(facts.options.len().max(1) as u16));
    constraints.push(Constraint::Length(1));
    let slots = Layout::vertical(constraints).split(inner);
    let mut cursor = 2usize;
    let text = facts.allow_text.then(|| {
        let rect = slots[cursor];
        cursor += 1;
        rect
    });
    let options_band = slots[cursor];
    let options = (0..facts.options.len())
        .map(|index| {
            Rect::new(
                options_band.x,
                options_band.y + index as u16,
                options_band.width,
                1,
            )
        })
        .collect();
    let footer = slots[cursor + 1];
    let close = Rect::new(
        footer.right().saturating_sub(11),
        footer.y,
        11.min(footer.width),
        1,
    );
    Some(DecisionDialogRects {
        popup,
        content: slots[1],
        text,
        options,
        close,
    })
}

/// Styled content lines for the scrollable band. The caller also uses this to
/// bound wheel/keyboard scrolling before a frame is drawn.
pub(crate) fn decision_dialog_content_lines(
    facts: &DecisionDialogFacts,
    palette: &Palette,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        facts.title.clone(),
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    ))];
    if let Some(body) = facts.body.as_deref().filter(|body| !body.is_empty()) {
        lines.push(Line::default());
        for line in body.lines() {
            lines.push(Line::from(Span::styled(
                line.to_owned(),
                Style::default().fg(palette.subtext0),
            )));
        }
    }
    if !facts.origin.is_empty() {
        lines.push(Line::default());
        for (label, value) in &facts.origin {
            lines.push(Line::from(vec![
                Span::styled(format!(" {label}: "), Style::default().fg(palette.overlay0)),
                Span::styled(value.clone(), Style::default().fg(palette.subtext0)),
            ]));
        }
    }
    if let Some(remaining) = &facts.remaining {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(" expires in {remaining}"),
            Style::default().fg(palette.peach),
        )));
    }
    lines
}

/// Highest usable scroll offset for the content band at `content` width.
pub(crate) fn decision_dialog_scroll_max(
    facts: &DecisionDialogFacts,
    content: Rect,
    palette: &Palette,
) -> u16 {
    Paragraph::new(decision_dialog_content_lines(facts, palette))
        .wrap(Wrap { trim: false })
        .line_count(content.width)
        .saturating_sub(content.height as usize) as u16
}

pub(crate) fn render_decision_dialog(
    frame: &mut Frame,
    area: Rect,
    facts: &DecisionDialogFacts,
    palette: &Palette,
) -> Option<DecisionDialogRects> {
    super::dim_background(frame, area);
    let rects = decision_dialog_rects(area, facts, palette)?;
    let inner = render_panel_shell(frame, rects.popup, palette.accent, palette.panel_bg)?;
    if inner.height < 4 {
        return None;
    }

    let header = Rect::new(inner.x, inner.y, inner.width, 1);
    render_modal_header(frame, header, " decision", palette);
    let machine = format!(" {} · {}/{}", facts.machine, facts.position, facts.total);
    let machine_x = header
        .x
        .saturating_add(display_width_u16(" decision "))
        .min(header.right());
    frame.render_widget(
        Paragraph::new(Span::styled(machine, Style::default().fg(palette.overlay0))),
        Rect::new(machine_x, header.y, header.right() - machine_x, 1),
    );

    frame.render_widget(
        Paragraph::new(decision_dialog_content_lines(facts, palette))
            .style(Style::default().fg(palette.text))
            .wrap(Wrap { trim: false })
            .scroll((facts.scroll, 0)),
        rects.content,
    );

    if let Some(text) = rects.text {
        let style = Style::default().fg(palette.text).bg(palette.surface0);
        frame.render_widget(
            Paragraph::new(format!(" {}", facts.text)).style(style),
            text,
        );
    }

    for (index, (label, rect)) in facts.options.iter().zip(rects.options.iter()).enumerate() {
        let selected = facts.keyboard_active && index == facts.selected;
        let style = if selected {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.surface0)
        };
        frame.render_widget(
            Paragraph::new(format!(" {} ", truncate_end(label, rect.width as usize)))
                .style(style)
                .alignment(ratatui::layout::Alignment::Center),
            *rect,
        );
    }

    if !facts.keyboard_active {
        frame.render_widget(
            Paragraph::new(" click the dialog to use the keyboard")
                .style(Style::default().fg(palette.overlay0)),
            Rect::new(inner.x, rects.close.y, rects.close.x - inner.x, 1),
        );
    }
    render_action_button(
        frame,
        rects.close,
        Some("esc"),
        "close",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(rects)
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{state::WorktreeCreateState, AppState},
        workspace::Workspace,
    };
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    use super::{confirm_close_overlay_text, render_new_linked_worktree_overlay};

    #[test]
    fn confirm_close_text_reports_parent_group_scope() {
        let mut app = AppState::test_new();
        let mut parent = Workspace::test_new("main");
        parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        let mut child = Workspace::test_new("issue");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app.workspaces = vec![parent, child];
        app.selected = 0;

        let (title, detail) = confirm_close_overlay_text(&app);

        assert_eq!(title, "Close worktree group?");
        assert_eq!(detail, "main — 2 workspaces, 2 panes");
    }

    #[test]
    fn new_worktree_error_renders_fatal_stderr_line() {
        let mut app = AppState::test_new();
        app.name_input = "foo".into();
        app.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "foo".into(),
            checkout_path: "/repo/.worktrees/herdr/foo".into(),
            error: Some(
                "Preparing worktree (new branch 'foo')\nfatal: a branch named 'foo' already exists"
                    .into(),
            ),
            creating: false,
        });

        let mut terminal =
            Terminal::new(TestBackend::new(100, 30)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_new_linked_worktree_overlay(&app, frame, Rect::new(0, 0, 100, 30)))
            .expect("new worktree overlay should render");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("fatal: a branch named 'foo' already exists"));
    }

    #[test]
    fn new_worktree_hit_test_geometry_matches_modal_size() {
        let area = Rect::new(0, 0, 100, 30);
        let inner = super::new_linked_worktree_inner_rect(area).unwrap();
        let (create, cancel) = super::new_linked_worktree_button_rects(inner);

        assert_eq!(inner.width, super::NEW_LINKED_WORKTREE_POPUP_WIDTH - 2);
        assert_eq!(inner.height, super::NEW_LINKED_WORKTREE_POPUP_HEIGHT - 2);
        assert_eq!(create.y, inner.y + inner.height - 1);
        assert_eq!(cancel.y, inner.y + inner.height - 1);
    }
}

#[cfg(test)]
mod shared_dialog_tests {
    use super::*;

    #[test]
    fn shared_rename_renderer_keeps_fork_cells_for_tiny_unicode_and_all_titles() {
        for (mode, creating, title) in [
            (Mode::RenameWorkspace, false, "rename workspace"),
            (Mode::RenameTab, false, "rename tab"),
            (Mode::RenameTab, true, "new tab"),
            (Mode::RenamePane, false, "rename pane"),
        ] {
            for text in ["", "existing", "漢字e\u{301}👩🏽‍💻"] {
                let mut app = AppState::test_new();
                app.mode = mode;
                app.creating_new_tab = creating;
                app.name_input = text.into();
                for (cols, rows) in [(0, 0), (1, 1), (20, 4), (56, 7), (80, 24), (160, 40)] {
                    let area = Rect::new(0, 0, cols, rows);
                    let draw = |shared| {
                        let mut terminal =
                            ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows))
                                .unwrap();
                        terminal
                            .draw(|frame| {
                                frame
                                    .buffer_mut()
                                    .set_string(0, 0, "漢字", Style::default());
                                if shared {
                                    render_rename_dialog(frame, area, title, text, &app.palette);
                                } else {
                                    render_rename_overlay(&app, frame, area);
                                }
                            })
                            .unwrap();
                        terminal.backend().buffer().clone()
                    };
                    assert_eq!(draw(true), draw(false), "{cols}x{rows} {title} {text}");
                }
            }
        }
    }

    fn decision_facts(body: Option<&str>, allow_text: bool, options: usize) -> DecisionDialogFacts {
        DecisionDialogFacts {
            machine: "Local".into(),
            position: 1,
            total: 1,
            title: "title".into(),
            body: body.map(|body| body.to_owned()),
            origin: Vec::new(),
            options: (0..options).map(|index| format!("opt{index}")).collect(),
            selected: 0,
            allow_text,
            text: String::new(),
            remaining: Some("42s".into()),
            scroll: 0,
            keyboard_active: false,
        }
    }

    #[test]
    fn decision_dialog_height_fits_short_content() {
        let palette = crate::app::state::Palette::catppuccin();
        let facts = decision_facts(Some("one line"), false, 2);
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");
        // header(1) + content(title+blank+body+blank+expires = 5) + options(2)
        // + footer(1) + borders(2) = 11
        assert_eq!(rects.popup.height, 11);
        assert_eq!(rects.options.len(), 2);
        // No spare rows inside: the last option sits directly above the footer.
        assert_eq!(rects.close.y, rects.options[1].y + 1);
    }

    #[test]
    fn decision_dialog_height_caps_and_scrolls_long_content() {
        let palette = crate::app::state::Palette::catppuccin();
        let long_body = (0..40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let facts = decision_facts(Some(&long_body), true, 2);
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");
        assert_eq!(rects.popup.height, DECISION_DIALOG_MAX_HEIGHT);
        assert!(decision_dialog_scroll_max(&facts, rects.content, &palette) > 0);
        // A small surface still clamps to the area instead of overflowing.
        let small =
            decision_dialog_rects(Rect::new(0, 0, 140, 12), &facts, &palette).expect("rects");
        assert_eq!(small.popup.height, 10);
    }
}
