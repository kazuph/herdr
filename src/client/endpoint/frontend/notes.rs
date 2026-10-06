//! Cached release notes reuse the fork renderer and scroll geometry; no update execution.
use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

pub(super) struct Notes {
    pub(super) state: crate::app::state::ReleaseNotesState,
    grab: Option<u16>,
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    install_command: String,
}

pub(super) fn available(frontend: &ClientFrontend) -> bool {
    frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .and_then(|endpoint| endpoint.cache.snapshot())
        .is_some_and(|snapshot| snapshot.latest_release_notes_available)
}

pub(super) fn open(frontend: &mut ClientFrontend) {
    let Some(endpoint) = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
    else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    let Some(notes) = snapshot.release_notes.as_ref() else {
        return;
    };
    frontend.notes = Some(Notes {
        state: crate::app::state::ReleaseNotesState {
            version: notes.version.clone(),
            body: notes.body.clone(),
            preview: notes.preview,
            scroll: 0,
        },
        grab: None,
        endpoint: endpoint.endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        install_command: snapshot.update_install_command.clone(),
    });
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend.notes.as_ref().is_some_and(|notes| {
        frontend.runtime.shell.active_endpoint_id != notes.endpoint
            || frontend
                .runtime
                .shell
                .endpoint(&notes.endpoint)
                .and_then(|endpoint| endpoint.cache.live_snapshot(notes.generation))
                .is_none_or(|snapshot| snapshot.boot_id != notes.boot)
    }) {
        frontend.notes = None;
    }
}

fn layout(
    frontend: &ClientFrontend,
    scroll: u16,
) -> Option<(Rect, Rect, Rect, crate::pane::ScrollMetrics)> {
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    let popup = crate::ui::centered_popup_rect(
        area,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
    )?;
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(popup);
    if inner.height < 8 || inner.width < 4 {
        return None;
    }
    let body = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content;
    let close =
        crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
    let lines = lines(frontend);
    let full_width = body.width.max(1);
    let rows = crate::ui::release_notes_wrapped_line_count(&lines, full_width);
    let wrap_width = if rows > body.height.max(1) as usize && full_width > 1 {
        body.width.saturating_sub(1).max(1)
    } else {
        full_width
    };
    let total_rows = crate::ui::release_notes_wrapped_line_count(&lines, wrap_width);
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

fn lines(frontend: &ClientFrontend) -> Vec<(usize, ratatui::text::Line<'_>)> {
    frontend
        .notes
        .as_ref()
        .map(|notes| {
            crate::ui::release_notes_display_lines(
                &notes.state,
                &notes.install_command,
                &frontend.chrome.settings.palette,
            )
        })
        .unwrap_or_default()
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(help) = &frontend.notes else {
        return;
    };
    let max = layout(frontend, help.state.scroll)
        .map(|(_, _, _, m)| m.max_offset_from_bottom as u16)
        .unwrap_or(0);
    crate::ui::render_release_notes_from(
        frame,
        Rect::new(0, 0, frontend.cols, frontend.rows),
        &frontend.chrome.settings.palette,
        &help.state,
        &help.install_command,
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
            if let Some(help) = frontend.notes.as_mut() {
                help.grab = None;
            }
        }
        return false;
    }
    let geometry = frontend
        .notes
        .as_ref()
        .and_then(|notes| layout(frontend, notes.state.scroll));
    let Some(mut help) = frontend.notes.take() else {
        return false;
    };
    let max = geometry
        .map(|(_, _, _, m)| m.max_offset_from_bottom as u16)
        .unwrap_or(0);
    let mut delta: i16 = 0;
    let mut close = false;
    match event {
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Up | KeyCode::Char('k') => delta = -1,
            KeyCode::Down | KeyCode::Char('j') => delta = 1,
            KeyCode::PageUp => delta = -8,
            KeyCode::PageDown => delta = 8,
            KeyCode::Home => help.state.scroll = 0,
            KeyCode::End => help.state.scroll = max,
            KeyCode::Esc | KeyCode::Enter => close = true,
            _ => {}
        },
        RawInputEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => delta = -3,
            MouseEventKind::ScrollDown => delta = 3,
            MouseEventKind::Up(MouseButton::Left) => help.grab = None,
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, body, button, metrics)) = geometry {
                    if contains(button, mouse.column, mouse.row) {
                        close = true;
                    } else if let Some(track) =
                        crate::ui::release_notes_scrollbar_rect(body, metrics)
                    {
                        if contains(track, mouse.column, mouse.row) {
                            help.grab =
                                crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row);
                            if help.grab.is_none() {
                                help.state.scroll = max
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
                        help.state.scroll =
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
    help.state.scroll = if delta.is_negative() {
        help.state.scroll.saturating_sub(delta.unsigned_abs())
    } else {
        help.state.scroll.saturating_add(delta as u16)
    }
    .min(max);
    if !close {
        frontend.notes = Some(help);
    }
    true
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<Rect> {
    layout(frontend, frontend.notes.as_ref()?.state.scroll).map(|(popup, ..)| popup)
}
