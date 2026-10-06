//! Keybind help is client presentation; runtime input stays fenced while it is open.
use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Default)]
pub(super) struct Help {
    pub(super) scroll: u16,
    grab: Option<u16>,
}

fn layout(
    frontend: &ClientFrontend,
    scroll: u16,
) -> Option<(Rect, Rect, Rect, crate::pane::ScrollMetrics)> {
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    let popup = crate::ui::centered_popup_rect(area, 76, 22)?;
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(popup);
    if inner.height < 6 || inner.width < 4 {
        return None;
    }
    let body = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content;
    let close =
        crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
    let total_rows = lines(frontend)
        .into_iter()
        .map(|(width, _)| width.max(1).div_ceil(body.width.max(1) as usize))
        .sum::<usize>();
    let max = total_rows.saturating_sub(body.height.max(1) as usize);
    Some((
        popup,
        body,
        close,
        crate::pane::ScrollMetrics {
            offset_from_bottom: max.saturating_sub(scroll as usize),
            max_offset_from_bottom: max,
            viewport_rows: body.height.max(1) as usize,
        },
    ))
}

fn lines(frontend: &ClientFrontend) -> Vec<(usize, ratatui::text::Line<'static>)> {
    crate::ui::keybind_help_lines_from(
        frontend.keybinds.prefix,
        &frontend.keybinds.keybinds,
        &frontend.chrome.settings.palette,
    )
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(help) = &frontend.help else {
        return;
    };
    let max = layout(frontend, help.scroll)
        .map(|(_, _, _, m)| m.max_offset_from_bottom as u16)
        .unwrap_or(0);
    crate::ui::render_keybind_help_from(
        frame,
        &frontend.chrome.settings.palette,
        lines(frontend),
        help.scroll.min(max),
        max,
    );
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    rect.contains(ratatui::layout::Position::new(column, row))
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> bool {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        if matches!(event, RawInputEvent::OuterFocusLost) {
            if let Some(help) = frontend.help.as_mut() {
                help.grab = None;
            }
        }
        return false;
    }
    let Some(mut help) = frontend.help.take() else {
        return false;
    };
    let geometry = layout(frontend, help.scroll);
    let max = geometry
        .map(|(_, _, _, m)| m.max_offset_from_bottom as u16)
        .unwrap_or(0);
    let mut delta = 0;
    let mut close = false;
    match event {
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Up | KeyCode::Char('k') => delta = -1,
            KeyCode::Down | KeyCode::Char('j') => delta = 1,
            KeyCode::PageUp => delta = -8,
            KeyCode::PageDown => delta = 8,
            KeyCode::Home => help.scroll = 0,
            KeyCode::End => help.scroll = max,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => close = true,
            _ => {}
        },
        RawInputEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => delta = -3,
            MouseEventKind::ScrollDown => delta = 3,
            MouseEventKind::Up(MouseButton::Left) => help.grab = None,
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((popup, body, button, metrics)) = geometry {
                    if contains(button, mouse.column, mouse.row)
                        || !contains(popup, mouse.column, mouse.row)
                    {
                        close = true;
                    } else if let Some(track) =
                        crate::ui::release_notes_scrollbar_rect(body, metrics)
                    {
                        if contains(track, mouse.column, mouse.row) {
                            help.grab =
                                crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row);
                            if help.grab.is_none() {
                                help.scroll = max
                                    .saturating_sub(crate::ui::scrollbar_offset_from_row(
                                        metrics, track, mouse.row,
                                    ) as u16);
                            }
                        }
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let (Some(grab), Some((_, body, _, metrics))) = (help.grab, geometry) {
                    if let Some(track) = crate::ui::release_notes_scrollbar_rect(body, metrics) {
                        help.scroll =
                            max.saturating_sub(crate::ui::scrollbar_offset_from_drag_row(
                                metrics, track, mouse.row, grab,
                            ) as u16);
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
    help.scroll = (help.scroll as i16)
        .saturating_add(delta)
        .clamp(0, max as i16) as u16;
    if !close {
        frontend.help = Some(help);
    }
    true
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<Rect> {
    layout(frontend, frontend.help.as_ref()?.scroll).map(|(popup, ..)| popup)
}
