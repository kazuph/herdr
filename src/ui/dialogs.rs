use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use super::text::{display_width_u16, truncate_end};
use super::widgets::{
    action_button_row_rects, action_button_width, centered_popup_rect, panel_contrast_fg,
    render_action_button, render_modal_header, render_modal_shell, render_panel_shell,
    ActionButtonSpec,
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

const DECISION_DIALOG_WIDTH: u16 = 80;
const DECISION_DIALOG_MAX_HEIGHT: u16 = 22;
/// Description rows are inset this many columns under the option label.
const DECISION_OPTION_DESC_INDENT: u16 = 3;
/// Uniform left/right margin inside the border; every row of the dialog
/// (header, content, options, footer) shares it.
const DECISION_DIALOG_PAD: u16 = 1;
/// Header context shown when a decision carries no resolvable origin.
const DECISION_ORIGIN_UNKNOWN: &str = "出どころ不明";

/// Fit the `workspace › tab › pane (agent)` context into the header by first
/// dropping leading segments (the tail — pane and agent — identifies the
/// decision best) and only then truncating the text itself.
fn elide_header_summary(summary: &str, budget: u16) -> String {
    if display_width_u16(summary) <= budget {
        return summary.to_string();
    }
    let segments: Vec<&str> = summary.split(" › ").collect();
    for drop in 1..segments.len() {
        let candidate = format!("… › {}", segments[drop..].join(" › "));
        if display_width_u16(&candidate) <= budget {
            return candidate;
        }
    }
    truncate_end(summary, budget as usize)
}

/// One selectable option: its label plus the optional detail text shown
/// wrapped below it (Claude Code AskUserQuestion descriptions).
pub(crate) struct DecisionDialogOption {
    pub label: String,
    pub description: Option<String>,
}

/// Render facts for one pending decision, already resolved for display by the
/// caller (machine label, queue position, remaining time).
pub(crate) struct DecisionDialogFacts {
    pub machine: String,
    /// Where the decision came from, resolved by the caller for the header
    /// (workspace/tab/pane/agent names). `None` renders an explicit
    /// unknown-origin marker instead of a blank.
    pub origin_summary: Option<String>,
    /// 1-based position inside the pending queue.
    pub position: usize,
    pub total: usize,
    pub title: String,
    pub body: Option<String>,
    /// `(label, value)` origin rows such as `("pane", "p1")`.
    pub origin: Vec<(String, String)>,
    pub options: Vec<DecisionDialogOption>,
    /// Option highlighted for keyboard activation; only used when
    /// `keyboard_active` is set.
    pub selected: usize,
    pub allow_text: bool,
    pub text: String,
    /// Remaining time such as `"42s"`, when the decision expires.
    pub remaining: Option<String>,
    /// Scroll offset of the content band (title/body/origin).
    pub scroll: u16,
    /// Scroll offset of the options band when options outgrow it.
    pub option_scroll: u16,
    /// False while keyboard input is still going to the pane (before the first
    /// click inside the dialog).
    pub keyboard_active: bool,
}

/// Geometry shared by the renderer and the mouse hit-test. The content band is
/// a scroll region; option rows sit at the bottom so long bodies never push
/// the answers off screen. When options (labels + descriptions) outgrow the
/// band the band itself scrolls while staying pinned above the footer.
pub(crate) struct DecisionDialogRects {
    pub popup: Rect,
    pub content: Rect,
    pub text: Option<Rect>,
    /// Pinned band holding the option blocks; scrolls via `option_scroll`.
    pub options_band: Rect,
    /// Visible portion of each option's block inside the band (a rect of
    /// height 0 marks an option scrolled fully out of view).
    pub options: Vec<Rect>,
    /// Largest usable `option_scroll` at the current band size.
    pub options_scroll_max: u16,
    pub confirm: Rect,
    pub close: Rect,
}

/// Rows each option occupies at `width`: one label row plus the wrapped
/// description rows.
pub(crate) fn decision_dialog_option_rows(facts: &DecisionDialogFacts, width: u16) -> Vec<u16> {
    facts
        .options
        .iter()
        .map(|option| {
            option
                .description
                .as_deref()
                .filter(|description| !description.trim().is_empty())
                .map_or(1, |description| {
                    1 + Paragraph::new(description.to_string())
                        .wrap(Wrap { trim: false })
                        .line_count(width.saturating_sub(DECISION_OPTION_DESC_INDENT).max(1))
                        as u16
                })
        })
        .collect()
}

/// `option_scroll` adjusted so the whole block of `selected` is visible (or,
/// for a block taller than the band, so its label row stays on top).
pub(crate) fn decision_dialog_reveal_option_scroll(facts: &DecisionDialogFacts, band: Rect) -> u16 {
    let scroll = facts
        .option_scroll
        .min(decision_dialog_options_scroll_max(facts, band));
    let rows = decision_dialog_option_rows(facts, band.width);
    let Some(index) = facts
        .options
        .len()
        .checked_sub(1)
        .map(|last| facts.selected.min(last))
    else {
        return scroll;
    };
    let top: u16 = rows.iter().take(index).sum();
    let bottom = top + rows[index];
    if top < scroll {
        top
    } else if bottom > scroll + band.height {
        if rows[index] >= band.height {
            top
        } else {
            bottom - band.height
        }
    } else {
        scroll
    }
}

/// Largest usable `option_scroll` for the options band.
pub(crate) fn decision_dialog_options_scroll_max(facts: &DecisionDialogFacts, band: Rect) -> u16 {
    let total: u16 = decision_dialog_option_rows(facts, band.width).iter().sum();
    total.saturating_sub(band.height)
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
    let content_width = popup_w.saturating_sub(2 + 2 * DECISION_DIALOG_PAD);
    if content_width == 0 {
        return None;
    }
    let content_lines = Paragraph::new(decision_dialog_content_lines(facts, palette))
        .wrap(Wrap { trim: false })
        .line_count(content_width) as u16;
    let option_rows = decision_dialog_option_rows(facts, content_width);
    let total_option_rows: u16 = option_rows.iter().sum();
    let inner_needed = 1u16
        .saturating_add(content_lines.max(1))
        .saturating_add(u16::from(facts.allow_text))
        .saturating_add(total_option_rows.max(1))
        .saturating_add(1);
    let popup_h = inner_needed
        .saturating_add(2)
        .clamp(4, DECISION_DIALOG_MAX_HEIGHT);
    let popup = centered_popup_rect(area, DECISION_DIALOG_WIDTH, popup_h)?;
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let padded = Rect::new(
        inner.x + DECISION_DIALOG_PAD.min(inner.width / 2),
        inner.y,
        inner.width.saturating_sub(2 * DECISION_DIALOG_PAD),
        inner.height,
    );
    // The band keeps at most what is left after header, a minimal content
    // row, the text row, and the footer; overflow scrolls.
    let band_height = total_option_rows
        .max(1)
        .min(
            padded
                .height
                .saturating_sub(3 + u16::from(facts.allow_text)),
        )
        .max(1);
    let mut constraints = vec![Constraint::Length(1), Constraint::Min(1)];
    if facts.allow_text {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(band_height));
    constraints.push(Constraint::Length(1));
    let slots = Layout::vertical(constraints).split(padded);
    let mut cursor = 2usize;
    let text = facts.allow_text.then(|| {
        let rect = slots[cursor];
        cursor += 1;
        rect
    });
    let options_band = slots[cursor];
    let options_scroll_max = total_option_rows.saturating_sub(options_band.height);
    let option_scroll = facts.option_scroll.min(options_scroll_max);
    let options = option_rows
        .iter()
        .scan(0u16, |top, rows| {
            let option_top = *top;
            *top += *rows;
            let visible_top = option_top.max(option_scroll);
            let visible_bottom = (*top).min(option_scroll + options_band.height);
            let height = visible_bottom.saturating_sub(visible_top);
            Some(Rect::new(
                options_band.x,
                options_band.y + visible_top.saturating_sub(option_scroll),
                options_band.width,
                height,
            ))
        })
        .collect();
    let footer = slots[cursor + 1];
    let close = Rect::new(
        footer.right().saturating_sub(11),
        footer.y,
        11.min(footer.width),
        1,
    );
    let confirm_width = action_button_width(Some("↵"), "confirm");
    let confirm = Rect::new(
        close.x.saturating_sub(confirm_width + 1).max(footer.x),
        footer.y,
        confirm_width.min(footer.width),
        1,
    );
    Some(DecisionDialogRects {
        popup,
        content: slots[1],
        text,
        options_band,
        options,
        options_scroll_max,
        confirm,
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
                Span::styled(format!("{label}: "), Style::default().fg(palette.overlay0)),
                Span::styled(value.clone(), Style::default().fg(palette.subtext0)),
            ]));
        }
    }
    if let Some(remaining) = &facts.remaining {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("expires in {remaining}"),
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

    let header = Rect::new(rects.options_band.x, inner.y, rects.options_band.width, 1);
    render_modal_header(frame, header, "decision", palette);
    let summary = facts
        .origin_summary
        .as_deref()
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or(DECISION_ORIGIN_UNKNOWN);
    let tail = format!(" · {}/{}", facts.position, facts.total);
    let prefix = format!(" {} · ", facts.machine);
    let context_x = header
        .x
        .saturating_add(display_width_u16("decision "))
        .min(header.right());
    let context_width = header.right().saturating_sub(context_x);
    let summary_width = context_width
        .saturating_sub(display_width_u16(&tail))
        .saturating_sub(display_width_u16(&prefix));
    let context = format!(
        "{prefix}{}{tail}",
        elide_header_summary(summary, summary_width)
    );
    frame.render_widget(
        Paragraph::new(Span::styled(context, Style::default().fg(palette.overlay0))),
        Rect::new(context_x, header.y, context_width, 1),
    );

    frame.render_widget(
        Paragraph::new(decision_dialog_content_lines(facts, palette))
            .style(Style::default().fg(palette.text))
            .wrap(Wrap { trim: false })
            .scroll((facts.scroll, 0)),
        rects.content,
    );

    if let Some(text) = rects.text {
        let (value, style) = if facts.text.is_empty() {
            (
                " type a response (optional)".to_string(),
                Style::default().fg(palette.overlay0).bg(palette.surface0),
            )
        } else {
            (
                format!(" {}", facts.text),
                Style::default().fg(palette.text).bg(palette.surface0),
            )
        };
        frame.render_widget(Paragraph::new(value).style(style), text);
    }

    let band = rects.options_band;
    let option_scroll = facts.option_scroll.min(rects.options_scroll_max);
    let mut top = 0u16;
    for (index, (option, rows)) in facts
        .options
        .iter()
        .zip(decision_dialog_option_rows(facts, band.width).iter())
        .enumerate()
    {
        let option_top = top;
        top += *rows;
        if option_top >= option_scroll && option_top < option_scroll + band.height {
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
                Paragraph::new(truncate_end(&option.label, band.width as usize)).style(style),
                Rect::new(band.x, band.y + option_top - option_scroll, band.width, 1),
            );
        }
        let Some(description) = option
            .description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
        else {
            continue;
        };
        let desc_top = option_top + 1;
        let desc_end = option_top + *rows;
        let visible_top = desc_top.max(option_scroll);
        let visible_bottom = desc_end.min(option_scroll + band.height);
        if visible_bottom > visible_top {
            frame.render_widget(
                Paragraph::new(description.to_string())
                    .style(Style::default().fg(palette.subtext0).bg(palette.surface0))
                    .wrap(Wrap { trim: false })
                    .scroll((visible_top - desc_top, 0)),
                Rect::new(
                    band.x + DECISION_OPTION_DESC_INDENT.min(band.width),
                    band.y + visible_top - option_scroll,
                    band.width.saturating_sub(DECISION_OPTION_DESC_INDENT),
                    visible_bottom - visible_top,
                ),
            );
        }
    }

    if !facts.keyboard_active {
        frame.render_widget(
            Paragraph::new("click the dialog to use the keyboard")
                .style(Style::default().fg(palette.overlay0)),
            Rect::new(
                header.x,
                rects.close.y,
                rects.confirm.x.saturating_sub(header.x),
                1,
            ),
        );
    }
    render_action_button(
        frame,
        rects.confirm,
        Some("↵"),
        "confirm",
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
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
    use ratatui::{backend::TestBackend, Terminal};

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
            origin_summary: Some("ws › tab › pane (agent)".into()),
            position: 1,
            total: 1,
            title: "title".into(),
            body: body.map(|body| body.to_owned()),
            origin: Vec::new(),
            options: (0..options)
                .map(|index| DecisionDialogOption {
                    label: format!("opt{index}"),
                    description: None,
                })
                .collect(),
            selected: 0,
            allow_text,
            text: String::new(),
            remaining: Some("42s".into()),
            scroll: 0,
            option_scroll: 0,
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

    fn decision_facts_with_descriptions(descriptions: &[Option<&str>]) -> DecisionDialogFacts {
        let mut facts = decision_facts(None, false, descriptions.len());
        for (option, description) in facts.options.iter_mut().zip(descriptions.iter().copied()) {
            option.description = description.map(str::to_string);
        }
        facts
    }

    #[test]
    fn decision_dialog_renders_wrapped_option_descriptions() {
        let palette = crate::app::state::Palette::catppuccin();
        let mut facts = decision_facts_with_descriptions(&[
            Some(
                "main にそのまま続けて取り込んで（fast-forward）、店長の Mac の gtx を入れ直す。\
                 店長が動かしてる gtx のサーバーは止めない。",
            ),
            Some("取り込むだけで gtx は入れ直さない"),
            None,
        ]);
        facts.options[0].label = "取り込んで入れ直す".into();
        facts.options[1].label = "取り込むだけ".into();
        facts.options[2].label = "まだ取り込まない".into();

        let area = Rect::new(0, 0, 140, 40);
        let rects = decision_dialog_rects(area, &facts, &palette).expect("rects");
        // Each described option takes its label row plus wrapped desc rows.
        let rows = decision_dialog_option_rows(&facts, rects.options_band.width);
        assert_eq!(rows.len(), 3);
        assert!(rows[0] >= 2, "long description must wrap: {rows:?}");
        assert_eq!(rows[1], 2);
        assert_eq!(rows[2], 1);
        // Options still sit directly above the footer.
        assert_eq!(rects.options[2].y + rows[2], rects.close.y);
        assert_eq!(rects.options_scroll_max, 0);

        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| {
                render_decision_dialog(frame, area, &facts, &palette);
            })
            .unwrap();
        // Wide chars leave a spacer cell after them; drop spaces so marker
        // comparison is cell-layout independent.
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        for marker in [
            "取り込んで入れ直す",
            "入れ直す。",
            "取り込むだけ",
            "gtx は入れ直さない",
            "まだ取り込まない",
        ] {
            let marker: String = marker.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(text.contains(&marker), "missing {marker:?}\n{text}");
        }
    }

    #[test]
    fn decision_dialog_option_band_scrolls_to_reach_every_description() {
        let palette = crate::app::state::Palette::catppuccin();
        // Short tokens that never split mid-word when the band wraps them;
        // enough of them to overflow the capped dialog.
        let long = (0..400)
            .map(|index| format!("d{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut facts = decision_facts_with_descriptions(&[
            Some(long.as_str()),
            Some("short"),
            Some("tailmarkerXYZ"),
        ]);
        let area = Rect::new(0, 0, 140, 30);
        let rects = decision_dialog_rects(area, &facts, &palette).expect("rects");
        let total: u16 = decision_dialog_option_rows(&facts, rects.options_band.width)
            .iter()
            .sum();
        assert!(total > rects.options_band.height);
        assert_eq!(rects.options_scroll_max, total - rects.options_band.height);
        // Option 0's label is visible; option 2's block is clipped by the band.
        assert!(rects.options[0].height >= 1);
        assert!(
            rects.options[2].height
                < decision_dialog_option_rows(&facts, rects.options_band.width)[2]
        );

        // Scrolling to the bottom reveals the last option's whole block.
        facts.option_scroll = rects.options_scroll_max;
        let rects = decision_dialog_rects(area, &facts, &palette).expect("rects");
        let last_rows = decision_dialog_option_rows(&facts, rects.options_band.width)[2];
        assert_eq!(rects.options[2].height, last_rows);
        assert_eq!(rects.options[2].y + last_rows, rects.close.y);

        // Keyboard selection reveal pulls the band so the option stays visible.
        facts.option_scroll = 0;
        facts.selected = 2;
        assert_eq!(
            decision_dialog_reveal_option_scroll(&facts, rects.options_band),
            rects.options_scroll_max
        );
        facts.selected = 0;
        assert_eq!(
            decision_dialog_reveal_option_scroll(&facts, rects.options_band),
            0
        );

        // Every description token is reachable through the scroll.
        let mut seen = String::new();
        for offset in 0..=rects.options_scroll_max {
            facts.option_scroll = offset;
            let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
            terminal
                .draw(|frame| {
                    render_decision_dialog(frame, area, &facts, &palette);
                })
                .unwrap();
            for cell in terminal.backend().buffer().content() {
                for ch in cell.symbol().chars() {
                    if !ch.is_whitespace() {
                        seen.push(ch);
                    }
                }
            }
        }
        for index in 0..400 {
            assert!(seen.contains(&format!("d{index}")), "missing d{index}");
        }
        assert!(seen.contains("tailmarkerXYZ"));
    }

    fn decision_buffer(
        facts: &DecisionDialogFacts,
        palette: &Palette,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render_decision_dialog(frame, Rect::new(0, 0, width, height), facts, palette);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, y: u16, x: u16, width: u16) -> String {
        (x..x + width)
            .map(|x| buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "))
            .collect()
    }

    #[test]
    fn decision_dialog_header_shows_resolved_origin_context() {
        let palette = crate::app::state::Palette::catppuccin();
        let mut facts = decision_facts(None, false, 2);
        facts.origin_summary = Some("herdr › main › claude (claude)".into());
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");

        let buffer = decision_buffer(&facts, &palette, 140, 40);
        let header = buffer_row_text(&buffer, rects.popup.y + 1, rects.popup.x, rects.popup.width);
        assert!(
            header.contains("herdr › main › claude (claude)"),
            "{header:?}"
        );
        assert!(header.contains("Local"), "{header:?}");
        assert!(header.contains("1/1"), "{header:?}");
    }

    #[test]
    fn decision_dialog_header_marks_unknown_origin_and_truncates_long_context() {
        let palette = crate::app::state::Palette::catppuccin();
        let mut facts = decision_facts(None, false, 2);
        facts.origin_summary = None;
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");
        let buffer = decision_buffer(&facts, &palette, 140, 40);
        let header = buffer_row_text(&buffer, rects.popup.y + 1, rects.popup.x, rects.popup.width);
        // Wide characters occupy a reset cell after them, so compare without spaces.
        let compact: String = header.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.contains("出どころ不明"), "{header:?}");

        // A very long context keeps the identifying tail (pane and agent)
        // while the queue position survives.
        facts.origin_summary = Some(format!("{} › tab › pane (agent)", "w".repeat(120)));
        let buffer = decision_buffer(&facts, &palette, 140, 40);
        let header = buffer_row_text(&buffer, rects.popup.y + 1, rects.popup.x, rects.popup.width);
        assert!(header.contains("pane (agent)"), "{header:?}");
        assert!(header.contains('…'), "{header:?}");
        assert!(header.contains("1/1"), "{header:?}");
    }

    #[test]
    fn decision_dialog_pads_every_row_and_shows_no_empty_band() {
        let palette = crate::app::state::Palette::catppuccin();
        let mut facts = decision_facts(Some("body line"), true, 2);
        facts.origin = vec![
            ("pane".into(), "p2293".into()),
            ("agent".into(), "claude".into()),
            ("cwd".into(), "/repo/herdr".into()),
            ("reason".into(), "screen_prompt".into()),
        ];
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");
        let buffer = decision_buffer(&facts, &palette, 140, 40);

        // Every row inside the popup keeps one blank column of margin next to
        // the border on both sides.
        for y in (rects.popup.y + 1)..(rects.popup.y + rects.popup.height - 1) {
            for x in [rects.popup.x + 1, rects.popup.x + rects.popup.width - 2] {
                let cell = buffer.cell((x, y)).expect("cell");
                assert_eq!(cell.symbol(), " ", "expected margin at ({x},{y})");
            }
        }

        // No row is a meaningless empty band: each painted surface row holds
        // text (option labels or the empty-text placeholder).
        for y in (rects.popup.y + 1)..(rects.popup.y + rects.popup.height - 1) {
            let mut surface_cells = 0usize;
            let mut surface_text = String::new();
            for x in (rects.popup.x + 2)..(rects.popup.x + rects.popup.width - 2) {
                let cell = buffer.cell((x, y)).expect("cell");
                if cell.bg == palette.surface0 {
                    surface_cells += 1;
                    surface_text.push_str(cell.symbol());
                }
            }
            if surface_cells > 0 {
                assert!(
                    !surface_text.trim().is_empty(),
                    "row {y} is an empty surface band"
                );
            }
        }

        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("type a response (optional)"), "{text}");
        assert!(text.contains("pane: p2293"), "{text}");
    }

    #[test]
    fn decision_dialog_options_are_left_aligned_with_content() {
        let palette = crate::app::state::Palette::catppuccin();
        let mut facts = decision_facts(None, false, 2);
        facts.options[0].label = "short".into();
        facts.options[1].label = "also short".into();
        let rects =
            decision_dialog_rects(Rect::new(0, 0, 140, 40), &facts, &palette).expect("rects");
        let buffer = decision_buffer(&facts, &palette, 140, 40);

        // Option labels start at the same column as the content text, not
        // centered inside the band.
        let first = buffer_row_text(
            &buffer,
            rects.options[0].y,
            rects.options_band.x,
            rects.options_band.width,
        );
        assert!(first.starts_with("short"), "{first:?}");
        assert_eq!(rects.options_band.x, rects.popup.x + 2);
    }
}
