//! Pointer selection uses the fork's absolute-row algorithm and client-host clipboard.
use super::copy::Owner;
use super::*;
use crate::api::schema as api;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::VecDeque;

enum Operation {
    Copy { revision: u64 },
    Scroll(u64),
}

#[derive(Default)]
pub(super) struct PointerSelection {
    owner: Option<Owner>,
    selection: Option<crate::selection::TextSelection<()>>,
    scrollbar: Option<u16>,
    pending: Option<(String, Operation)>,
    waiting_scroll: Option<u64>,
    events: VecDeque<MouseEvent>,
    pointer: Option<MouseEvent>,
    autoscroll: Option<crate::app::state::SelectionAutoscroll>,
    pub(super) deadline: Option<Instant>,
}

pub(super) fn clear(frontend: &mut ClientFrontend) {
    frontend.pointer_selection = PointerSelection::default();
}

#[cfg(test)]
pub(super) fn range_for_test(frontend: &ClientFrontend) -> Option<((u32, u16), (u32, u16))> {
    frontend
        .pointer_selection
        .selection
        .as_ref()
        .map(crate::selection::TextSelection::ordered_cells)
}

fn metrics(pane: &wire::PaneSurfacePane) -> Option<crate::pane::ScrollMetrics> {
    let scroll = pane.scroll?;
    Some(crate::pane::ScrollMetrics {
        offset_from_bottom: scroll.offset_from_bottom.min(usize::MAX as u64) as usize,
        max_offset_from_bottom: scroll.max_offset_from_bottom.min(usize::MAX as u64) as usize,
        viewport_rows: scroll.viewport_rows.min(usize::MAX as u64) as usize,
    })
}

fn rect(rect: wire::SurfaceRect, origin: Rect) -> Rect {
    Rect::new(
        origin.x + rect.x,
        origin.y + rect.y,
        rect.width,
        rect.height,
    )
}

pub(super) fn drag_text_selection(
    selection: &mut crate::selection::TextSelection<()>,
    inner: Rect,
    metrics: crate::pane::ScrollMetrics,
    mouse: MouseEvent,
) -> (Option<crate::app::state::SelectionAutoscroll>, Option<u64>) {
    let (row, col) = selection.anchor_screen_pos(inner, Some(metrics));
    let dragging = selection.is_dragging() || row != mouse.row || col != mouse.column;
    selection.drag(mouse.column, mouse.row, inner, Some(metrics));
    if dragging {
        selection.force_dragging();
    }
    let bottom = inner.bottom().saturating_sub(1);
    let direction = if dragging && mouse.row <= inner.y {
        Some(crate::app::state::SelectionAutoscrollDirection::Up)
    } else if dragging && mouse.row >= bottom {
        Some(crate::app::state::SelectionAutoscrollDirection::Down)
    } else {
        None
    };
    let autoscroll = direction.map(|direction| crate::app::state::SelectionAutoscroll {
        direction,
        last_mouse_screen_col: mouse.column,
        last_mouse_screen_row: mouse.row,
        inner_rect: inner,
    });
    let distance = if mouse.row < inner.y {
        inner.y - mouse.row
    } else {
        mouse.row.saturating_sub(bottom)
    };
    let offset = (distance > 0 && dragging).then(|| {
        let lines = usize::from(distance).saturating_mul(3).clamp(3, 15) as u64;
        if mouse.row < inner.y {
            (metrics.offset_from_bottom as u64).saturating_add(lines)
        } else {
            (metrics.offset_from_bottom as u64).saturating_sub(lines)
        }
    });
    (autoscroll, offset)
}

fn issue(
    frontend: &mut ClientFrontend,
    method: api::Method,
    operation: Operation,
) -> io::Result<()> {
    match frontend.runtime.issue_method_with_id(method) {
        Ok((request, update)) => {
            frontend.pointer_selection.pending = Some((request, operation));
            frontend.update(update)?;
        }
        Err(error) => {
            clear(frontend);
            frontend.notice = Some(error);
        }
    }
    Ok(())
}

fn scroll(
    frontend: &mut ClientFrontend,
    pane: &wire::PaneSurfacePane,
    offset: u64,
) -> io::Result<()> {
    let Some(current) = pane.scroll else {
        return Ok(());
    };
    let offset = offset.min(current.max_offset_from_bottom);
    if offset == current.offset_from_bottom {
        return Ok(());
    }
    issue(
        frontend,
        api::Method::PaneScroll(api::PaneScrollParams {
            pane_id: pane.pane_id.clone(),
            offset_from_bottom: offset,
        }),
        Operation::Scroll(offset),
    )
}

fn owner(frontend: &ClientFrontend, pane: &wire::PaneSurfacePane) -> Option<Owner> {
    let endpoint = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)?;
    let generation = endpoint.generation?;
    let snapshot = endpoint.cache.live_snapshot(generation)?;
    Some(Owner {
        endpoint: endpoint.endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        pane: pane.pane_id.clone(),
    })
}

pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    origin: Rect,
    mouse: MouseEvent,
) -> io::Result<bool> {
    if frontend
        .pointer_selection
        .owner
        .as_ref()
        .is_some_and(|owner| {
            owner.exists(frontend)
                && owner.endpoint == frontend.runtime.shell.active_endpoint_id
                && owner.visible_pane(frontend).is_none()
        })
    {
        frontend.pointer_selection.events.push_back(mouse);
        return Ok(true);
    }
    let owned = frontend
        .pointer_selection
        .owner
        .as_ref()
        .and_then(|owner| owner.visible_pane(frontend))
        .cloned();
    if let Some(pane) = owned {
        let waiting = frontend.pointer_selection.pending.is_some()
            || frontend.pointer_selection.waiting_scroll.is_some()
            || !pane.focused
            || !frontend.runtime.input_lease_current();
        if waiting {
            frontend.pointer_selection.events.push_back(mouse);
            return Ok(true);
        }
        if frontend.pointer_selection.selection.is_some()
            || frontend.pointer_selection.scrollbar.is_some()
        {
            return drag(frontend, &pane, origin, mouse);
        }
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || !frontend.runtime.input_lease_current()
    {
        return Ok(false);
    }
    clear(frontend);
    let Some(pane) = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .and_then(|surface| {
            surface
                .panes
                .iter()
                .find(|pane| rect(pane.rect, origin).contains((mouse.column, mouse.row).into()))
        })
        .cloned()
    else {
        return Ok(false);
    };
    let inner = rect(pane.inner_rect, origin);
    let Some(metrics) = metrics(&pane) else {
        return Ok(false);
    };
    let track = pane
        .scrollbar_rect
        .map(|track| rect(track, origin))
        .filter(|track| track.contains((mouse.column, mouse.row).into()));
    let selection = frontend.copy_on_select
        && mouse.modifiers.is_empty()
        && !pane.mouse_reporting
        && inner.contains((mouse.column, mouse.row).into());
    if track.is_none() && !selection {
        return Ok(false);
    }
    frontend.pointer_selection.owner = owner(frontend, &pane);
    frontend.pointer_selection.pointer = Some(mouse);
    if let Some(track) = track {
        frontend.pointer_selection.scrollbar =
            crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row);
        if frontend.pointer_selection.scrollbar.is_none() {
            frontend.pointer_selection.scrollbar = Some(0);
            frontend.pointer_selection.events.push_back(mouse);
        }
    } else {
        frontend.pointer_selection.selection = Some(crate::selection::TextSelection::anchor(
            (),
            mouse.row - inner.y,
            mouse.column - inner.x,
            Some(metrics),
        ));
    }
    if !pane.focused {
        let update = frontend.runtime.activate(
            frontend.runtime.shell.active_endpoint_id.clone(),
            Some(super::super::FocusTarget::Pane(pane.pane_id.clone())),
            Instant::now(),
        );
        frontend.update(update)?;
    } else if let Some(track) = track {
        if crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row).is_none() {
            frontend.pointer_selection.events.clear();
            scroll(
                frontend,
                &pane,
                crate::ui::scrollbar_offset_from_row(metrics, track, mouse.row) as u64,
            )?;
        }
    }
    Ok(true)
}

fn drag(
    frontend: &mut ClientFrontend,
    pane: &wire::PaneSurfacePane,
    origin: Rect,
    mouse: MouseEvent,
) -> io::Result<bool> {
    let inner = rect(pane.inner_rect, origin);
    let Some(metrics) = metrics(pane) else {
        clear(frontend);
        return Ok(true);
    };
    frontend.pointer_selection.pointer = Some(mouse);
    if let Some(grab) = frontend.pointer_selection.scrollbar {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(track) = pane.scrollbar_rect {
                    scroll(
                        frontend,
                        pane,
                        crate::ui::scrollbar_offset_from_row(
                            metrics,
                            rect(track, origin),
                            mouse.row,
                        ) as u64,
                    )?;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(track) = pane.scrollbar_rect {
                    scroll(
                        frontend,
                        pane,
                        crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            rect(track, origin),
                            mouse.row,
                            grab,
                        ) as u64,
                    )?;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => clear(frontend),
            _ => {}
        }
        return Ok(true);
    }
    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some(selection) = frontend.pointer_selection.selection.as_mut() else {
                return Ok(false);
            };
            let (autoscroll, offset) = drag_text_selection(selection, inner, metrics, mouse);
            frontend.pointer_selection.deadline = autoscroll
                .as_ref()
                .map(|_| Instant::now() + crate::app::SELECTION_AUTOSCROLL_INTERVAL);
            frontend.pointer_selection.autoscroll = autoscroll;
            if let Some(offset) = offset {
                scroll(frontend, pane, offset)?;
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            frontend.pointer_selection.autoscroll = None;
            frontend.pointer_selection.deadline = None;
            let Some(selection) = frontend.pointer_selection.selection.as_mut() else {
                return Ok(false);
            };
            if !selection.finish() {
                clear(frontend);
                return Ok(true);
            }
            let ((ar, ac), (cr, cc)) = selection.ordered_cells();
            frontend.pointer_selection.selection = None;
            issue(
                frontend,
                api::Method::PaneSelectionRead(api::PaneSelectionReadParams {
                    pane_id: pane.pane_id.clone(),
                    anchor: api::PaneTextPoint { row: ar, col: ac },
                    cursor: api::PaneTextPoint { row: cr, col: cc },
                    content_revision: Some(pane.content_revision),
                }),
                Operation::Copy {
                    revision: pane.content_revision,
                },
            )?;
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let lines = frontend.chrome.settings.mouse_scroll_lines as u64;
            let offset = if mouse.kind == MouseEventKind::ScrollUp {
                (metrics.offset_from_bottom as u64).saturating_add(lines)
            } else {
                (metrics.offset_from_bottom as u64).saturating_sub(lines)
            };
            scroll(frontend, pane, offset)?;
        }
        MouseEventKind::Down(_) => {
            clear(frontend);
            return Ok(false);
        }
        _ => {}
    }
    Ok(true)
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) {
    let state = &frontend.pointer_selection;
    let Some(owner) = state.owner.as_ref() else {
        return;
    };
    let Some((request, _)) = state.pending.as_ref() else {
        return;
    };
    if result.request_id != *request
        || result.endpoint_id != owner.endpoint
        || result.generation != owner.generation
        || result.boot_id != owner.boot
    {
        return;
    }
    let pane = owner.visible_pane(frontend).cloned();
    let Some((_, operation)) = frontend.pointer_selection.pending.take() else {
        return;
    };
    let Some(pane) = pane else {
        clear(frontend);
        return;
    };
    let Ok(value) = &result.result else {
        clear(frontend);
        return;
    };
    let Ok(response) = serde_json::from_value::<api::ResponseResult>(value.clone()) else {
        clear(frontend);
        return;
    };
    match (operation, response) {
        (Operation::Copy { revision }, api::ResponseResult::PaneSelection { pane_id, text })
            if pane_id == pane.pane_id && pane.content_revision == revision =>
        {
            if !text.is_empty() {
                crate::selection::write_osc52_bytes(text.as_bytes());
            }
            clear(frontend);
        }
        (Operation::Scroll(offset), api::ResponseResult::PaneInfo { pane: info })
            if info.pane_id == pane.pane_id
                && info
                    .scroll
                    .is_some_and(|metrics| metrics.offset_from_bottom == offset) =>
        {
            frontend.pointer_selection.waiting_scroll = Some(offset)
        }
        _ => clear(frontend),
    }
}

pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(owner) = frontend.pointer_selection.owner.as_ref() else {
        return Ok(());
    };
    if !owner.exists(frontend) || owner.endpoint != frontend.runtime.shell.active_endpoint_id {
        clear(frontend);
        return Ok(());
    }
    let Some(pane) = owner.visible_pane(frontend).cloned() else {
        return Ok(());
    };
    if !pane.focused
        || !frontend.runtime.input_lease_current()
        || frontend.pointer_selection.pending.is_some()
    {
        return Ok(());
    }
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let inner = rect(pane.inner_rect, view.layout.pane_surface);
    if frontend
        .pointer_selection
        .autoscroll
        .as_ref()
        .is_some_and(|state| state.inner_rect != inner)
    {
        frontend.pointer_selection.autoscroll = None;
        frontend.pointer_selection.deadline = None;
    }
    if let Some(offset) = frontend.pointer_selection.waiting_scroll {
        if pane
            .scroll
            .is_none_or(|metrics| metrics.offset_from_bottom != offset)
        {
            return Ok(());
        }
        frontend.pointer_selection.waiting_scroll = None;
        if let (Some(mouse), Some(selection)) = (
            frontend.pointer_selection.pointer,
            frontend.pointer_selection.selection.as_mut(),
        ) {
            selection.drag(mouse.column, mouse.row, inner, metrics(&pane));
        }
    }
    while let Some(event) = frontend.pointer_selection.events.pop_front() {
        drag(frontend, &pane, view.layout.pane_surface, event)?;
        if frontend.pointer_selection.pending.is_some() {
            break;
        }
    }
    Ok(())
}

pub(super) fn tick(frontend: &mut ClientFrontend, now: Instant) -> io::Result<()> {
    frontend.pointer_selection.deadline = None;
    let Some(state) = frontend.pointer_selection.autoscroll.clone() else {
        return Ok(());
    };
    let Some(pane) = frontend
        .pointer_selection
        .owner
        .as_ref()
        .and_then(|owner| owner.visible_pane(frontend))
        .cloned()
    else {
        clear(frontend);
        return Ok(());
    };
    let Some(metrics) = metrics(&pane) else {
        clear(frontend);
        return Ok(());
    };
    let up = state.direction == crate::app::state::SelectionAutoscrollDirection::Up;
    if (up && metrics.offset_from_bottom == metrics.max_offset_from_bottom)
        || (!up && metrics.offset_from_bottom == 0)
    {
        frontend.pointer_selection.autoscroll = None;
        return Ok(());
    }
    frontend.pointer_selection.deadline = Some(now + crate::app::SELECTION_AUTOSCROLL_INTERVAL);
    if !frontend.runtime.input_lease_current()
        || frontend.pointer_selection.pending.is_some()
        || frontend.pointer_selection.waiting_scroll.is_some()
    {
        return Ok(());
    }
    let offset = if up {
        metrics.offset_from_bottom.saturating_add(1)
    } else {
        metrics.offset_from_bottom.saturating_sub(1)
    };
    scroll(frontend, &pane, offset as u64)
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, origin: Rect) {
    let Some(owner) = frontend.pointer_selection.owner.as_ref() else {
        return;
    };
    let Some(pane) = owner.visible_pane(frontend) else {
        return;
    };
    let Some(selection) = frontend.pointer_selection.selection.as_ref() else {
        return;
    };
    let inner = rect(pane.inner_rect, origin);
    let style = crate::ui::automatic_selection_style(
        &frontend.chrome.settings.palette,
        frontend.host_theme,
    );
    for y in 0..inner.height {
        for x in 0..inner.width {
            if selection.contains(y, x, metrics(pane)) {
                frame.buffer_mut()[(inner.x + x, inner.y + y)].set_style(style);
            }
        }
    }
}
