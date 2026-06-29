use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, render_panel_shell, ActionButtonSpec,
};
use crate::app::{AppState, Mode};

fn bordered_inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

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
    super::dim_background(frame, area);

    let title = match app.mode {
        Mode::RenameWorkspace => "rename workspace",
        Mode::RenameTab if app.creating_new_tab => "new tab",
        Mode::RenameTab => "rename tab",
        Mode::RenamePane => "rename pane",
        _ => return,
    };

    let Some(inner) = render_modal_shell(frame, area, 56, 7, &app.palette) else {
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

    render_modal_header(frame, rows[0], title, &app.palette);

    let input_rect = Rect::new(rows[2].x, rows[2].y, rows[2].width, 1);
    frame.render_widget(Clear, input_rect);
    frame.render_widget(
        Paragraph::new(format!(" {}█", app.name_input)).style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0),
        ),
        input_rect,
    );

    let (save_rect, clear_rect, cancel_rect) = rename_button_rects(inner);

    render_action_button(
        frame,
        save_rect,
        Some("↵"),
        "save",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        clear_rect,
        Some("^c"),
        "clear",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(super) fn render_confirm_close_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let ws_name = app
        .workspaces
        .get(app.selected)
        .map(|ws| ws.display_name())
        .unwrap_or_else(|| "?".to_string());
    let pane_count = app
        .workspaces
        .get(app.selected)
        .map(|ws| ws.layout.pane_count())
        .unwrap_or(0);

    let pane_text = if pane_count == 1 {
        "1 pane".to_string()
    } else {
        format!("{pane_count} panes")
    };

    super::dim_background(frame, area);

    let Some(popup) = confirm_close_popup_rect(area) else {
        return;
    };

    let warn = Style::default()
        .fg(app.palette.red)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let title_line = Line::from(vec![Span::styled(" Close workspace?", warn)]);

    let detail_line = Line::from(vec![
        Span::styled(
            format!(" {ws_name}"),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" — {pane_text}"), dim),
    ]);

    let Some(inner) = render_panel_shell(frame, popup, app.palette.red, app.palette.panel_bg)
    else {
        return;
    };

    if inner.height >= 3 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas::<3>(inner);

        frame.render_widget(Paragraph::new(title_line), rows[0]);
        frame.render_widget(Paragraph::new(detail_line), rows[1]);

        let (confirm_rect, cancel_rect) = confirm_close_button_rects(inner);
        render_action_button(
            frame,
            confirm_rect,
            Some("↵"),
            "confirm",
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.red)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            cancel_rect,
            Some("esc"),
            "cancel",
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
}

pub(super) fn render_confirm_danger_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(action) = app.pending_danger_action else {
        return;
    };
    super::dim_background(frame, area);

    let missing_sessions = if action == crate::app::state::DangerousAction::Restart {
        app.missing_agent_session_infos()
    } else {
        Vec::new()
    };
    let Some(popup) = confirm_danger_popup_rect(area, missing_sessions.len()) else {
        return;
    };

    let warn = Style::default()
        .fg(app.palette.red)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

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

    let title_line = Line::from(vec![Span::styled(format!(" {title}"), warn)]);
    let detail_line = Line::from(vec![Span::styled(format!(" {detail}"), dim)]);

    let Some(inner) = render_panel_shell(frame, popup, app.palette.red, app.palette.panel_bg)
    else {
        return;
    };

    if inner.height >= 3 {
        let list_height = missing_sessions
            .len()
            .min(inner.height.saturating_sub(3) as usize);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(list_height as u16),
            Constraint::Length(1),
        ])
        .areas::<4>(inner);

        frame.render_widget(Paragraph::new(title_line), rows[0]);
        frame.render_widget(Paragraph::new(detail_line), rows[1]);
        for (idx, info) in missing_sessions.iter().take(list_height).enumerate() {
            let row = Rect::new(rows[2].x, rows[2].y + idx as u16, rows[2].width, 1);
            let title = info.title.as_deref().unwrap_or("-");
            let text = format!(
                " space {} {} pane {} {} title={} cwd={} reason={}",
                info.workspace_number,
                info.workspace_label,
                info.pane_label,
                info.agent,
                title,
                info.cwd.display(),
                info.reason
            );
            frame.render_widget(
                Paragraph::new(truncate_for_width(&text, row.width))
                    .style(Style::default().fg(app.palette.text)),
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
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.red)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            cancel_rect,
            Some("esc"),
            "cancel",
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn truncate_for_width(text: &str, width: u16) -> String {
    let width = width as usize;
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

pub(crate) fn confirm_close_popup_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 44, 5)
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
        2,
    );
    (rects[0], rects[1])
}

pub(crate) fn confirm_danger_popup_rect(area: Rect, missing_session_count: usize) -> Option<Rect> {
    let height = 5u16.saturating_add(missing_session_count.min(10) as u16);
    centered_popup_rect(area, 76, height)
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

pub(crate) fn new_linked_worktree_inner(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 68, 10).map(bordered_inner)
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

pub(super) fn render_new_linked_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(create) = app.worktree_create.as_ref() else {
        return;
    };

    super::dim_background(frame, area);
    let Some(inner) = render_modal_shell(frame, area, 68, 10, &app.palette) else {
        return;
    };

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas::<8>(inner);

    render_modal_header(frame, rows[0], "new worktree", &app.palette);
    frame.render_widget(
        Paragraph::new(" branch").style(Style::default().fg(app.palette.overlay0)),
        rows[1],
    );
    frame.render_widget(Clear, rows[2]);
    frame.render_widget(
        Paragraph::new(format!(" {}█", app.name_input)).style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0),
        ),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(format!(" path {}", create.checkout_path.display()))
            .style(Style::default().fg(app.palette.overlay0)),
        rows[3],
    );
    if create.creating {
        frame.render_widget(
            Paragraph::new(" creating...").style(Style::default().fg(app.palette.yellow)),
            rows[4],
        );
    }
    if let Some(error) = &create.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}")).style(Style::default().fg(app.palette.red)),
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
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(crate) fn open_existing_worktree_inner(area: Rect, entry_count: usize) -> Option<Rect> {
    let height = (entry_count as u16).saturating_add(5).clamp(8, 18);
    centered_popup_rect(area, 78, height).map(bordered_inner)
}

pub(crate) fn open_existing_worktree_entry_at(
    inner: Rect,
    entry_count: usize,
    selected: usize,
    col: u16,
    row: u16,
) -> Option<usize> {
    if col < inner.x || col >= inner.x + inner.width {
        return None;
    }
    let max_rows = inner.height.saturating_sub(4) as usize;
    let start = selected.saturating_sub(max_rows.saturating_sub(1));
    let first_row = inner.y.saturating_add(2);
    let visible_offset = row.checked_sub(first_row)? as usize;
    if visible_offset >= max_rows {
        return None;
    }
    let idx = start.saturating_add(visible_offset);
    (idx < entry_count).then_some(idx)
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

pub(super) fn render_open_existing_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(open) = app.worktree_open.as_ref() else {
        return;
    };

    super::dim_background(frame, area);
    let height = (open.entries.len() as u16).saturating_add(5).clamp(8, 18);
    let Some(inner) = render_modal_shell(frame, area, 78, height, &app.palette) else {
        return;
    };

    render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "open worktree",
        &app.palette,
    );
    let max_rows = inner.height.saturating_sub(4) as usize;
    let start = open.selected.saturating_sub(max_rows.saturating_sub(1));
    for (visible_idx, (entry_idx, entry)) in open
        .entries
        .iter()
        .enumerate()
        .skip(start)
        .take(max_rows)
        .enumerate()
    {
        let selected = entry_idx == open.selected;
        let y = inner.y.saturating_add(2 + visible_idx as u16);
        let marker = if selected { "›" } else { " " };
        let branch = entry.branch.as_deref().unwrap_or("detached");
        let open_label = if entry.already_open_ws_idx.is_some() {
            " open"
        } else {
            ""
        };
        let style = if selected {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{marker} {branch}{open_label}  {}",
                entry.path.display()
            ))
            .style(style),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
    if let Some(error) = &open.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}")).style(Style::default().fg(app.palette.red)),
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
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

pub(crate) fn remove_worktree_inner(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 72, 10).map(bordered_inner)
}

pub(crate) fn remove_worktree_button_rects(inner: Rect, force_confirmation: bool) -> (Rect, Rect) {
    let primary = if force_confirmation {
        "delete anyway"
    } else {
        "remove"
    };
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: primary,
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

pub(super) fn render_remove_worktree_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(remove) = app.worktree_remove.as_ref() else {
        return;
    };

    super::dim_background(frame, area);
    let Some(popup) = centered_popup_rect(area, 72, 10) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, app.palette.red, app.palette.panel_bg)
    else {
        return;
    };

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas::<8>(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " delete worktree checkout?",
            Style::default()
                .fg(app.palette.red)
                .add_modifier(Modifier::BOLD),
        )])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(format!(" {}", remove.path.display()))
            .style(Style::default().fg(app.palette.text)),
        rows[1],
    );
    if remove.removing {
        frame.render_widget(
            Paragraph::new(" removing...").style(Style::default().fg(app.palette.yellow)),
            rows[3],
        );
    }
    if let Some(error) = &remove.error {
        frame.render_widget(
            Paragraph::new(format!(" {error}")).style(Style::default().fg(app.palette.red)),
            rows[4],
        );
    }

    let primary = if remove.force_confirmation {
        "delete anyway"
    } else {
        "remove"
    };
    let (remove_rect, cancel_rect) = remove_worktree_button_rects(inner, remove.force_confirmation);
    render_action_button(
        frame,
        remove_rect,
        Some("↵"),
        primary,
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.red)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_worktree_buttons_render_below_entry_rows() {
        let inner = open_existing_worktree_inner(Rect::new(0, 0, 120, 30), 3).unwrap();
        let (open_rect, cancel_rect) = open_existing_worktree_button_rects(inner);
        let first_entry_y = inner.y + 2;
        let last_entry_y = first_entry_y + 2;

        assert!(open_rect.y > last_entry_y);
        assert_eq!(cancel_rect.y, open_rect.y);
    }
}
