use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{Palette, ToastKind, ToastNotification},
    detect::AgentState,
};

const PANE_ACTION_CYCLE_LAYOUT_LABEL: &str = " Cycle layout ";
const PANE_ACTION_ROTATE_LABEL: &str = " Rotate panes ";
const PANE_ACTION_EQUALIZE_LABEL: &str = " Equalize ";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PaneActionBarRects {
    pub cycle_layout: Rect,
    pub rotate: Rect,
    pub equalize: Rect,
}

pub(crate) fn pane_action_bar_rects(area: Rect) -> PaneActionBarRects {
    if area.width == 0 || area.height == 0 {
        return PaneActionBarRects::default();
    }

    let cycle_width = PANE_ACTION_CYCLE_LAYOUT_LABEL.len() as u16;
    let rotate_width = PANE_ACTION_ROTATE_LABEL.len() as u16;
    let equalize_width = PANE_ACTION_EQUALIZE_LABEL.len() as u16;
    let gap = 1;
    let total = cycle_width + rotate_width + equalize_width + gap * 2;
    let right_margin = 1;
    let mut x = if area.width > total + right_margin {
        area.x + area.width - total - right_margin
    } else {
        area.x
    };
    let right = area.x + area.width;

    let cycle_layout = button_rect(area, &mut x, cycle_width, right, gap);
    let rotate = button_rect(area, &mut x, rotate_width, right, gap);
    let equalize = button_rect(area, &mut x, equalize_width, right, 0);

    PaneActionBarRects {
        cycle_layout,
        rotate,
        equalize,
    }
}

pub(super) fn render_pane_action_bar(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    vim_mode_label: Option<&'static str>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let bar_style = Style::default().fg(p.overlay0).bg(Color::Reset);
    frame.render_widget(Paragraph::new("").style(bar_style), area);

    let label = vim_mode_label.unwrap_or(" panes ");
    let label_area = Rect::new(area.x, area.y, area.width.min(label.len() as u16), 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default().fg(p.overlay0).bg(Color::Reset),
        )),
        label_area,
    );

    let rects = pane_action_bar_rects(area);
    render_action_button(frame, rects.cycle_layout, PANE_ACTION_CYCLE_LAYOUT_LABEL, p);
    render_action_button(frame, rects.rotate, PANE_ACTION_ROTATE_LABEL, p);
    render_action_button(frame, rects.equalize, PANE_ACTION_EQUALIZE_LABEL, p);
}

fn button_rect(area: Rect, x: &mut u16, width: u16, right: u16, gap: u16) -> Rect {
    if *x >= right || width == 0 || *x + width > right {
        return Rect::default();
    }
    let rect = Rect::new(*x, area.y, width, 1);
    *x = x.saturating_add(width + gap);
    rect
}

fn render_action_button(frame: &mut Frame, rect: Rect, label: &'static str, p: &Palette) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default()
                .fg(p.accent)
                .bg(Color::Reset)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )),
        rect,
    );
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
) -> Rect {
    let content_width = (toast.title.len().max(toast.context.len()) as u16) + 4;
    let width = content_width.saturating_add(2).min(area.width);
    let height = 4u16.min(area.height);
    let x = area.x + area.width.saturating_sub(width);
    let y = area.y
        + area
            .height
            .saturating_sub(height + if offset_for_warning { 1 } else { 0 });
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 2 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    frame.render_widget(Paragraph::new(context), context_row);
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" config warning: {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_dot(state: AgentState, seen: bool, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_summary_icon(
    state: AgentState,
    seen: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Working, _) => (super::spinner_frame(tick), Style::default().fg(p.yellow)),
        _ => state_dot(state, seen, p),
    }
}

pub(super) fn agent_icon(
    state: AgentState,
    seen: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (super::spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Palette;

    #[test]
    fn state_summary_icon_animates_working_state() {
        let palette = Palette::catppuccin();

        let (icon, style) = state_summary_icon(AgentState::Working, true, 0, &palette);

        assert_eq!(icon, super::super::spinner_frame(0));
        assert_eq!(style, Style::default().fg(palette.yellow));
    }
}
