//! Popup pointer selection borrows the fork algorithm and reads its owner's retained text.
use super::*;
use crate::api::schema as api;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::VecDeque;

struct Owner {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    terminal: String,
}
struct Facts {
    revision: u64,
    surface: u64,
    scroll: api::PaneScrollInfo,
}
enum Operation {
    Facts(u64),
    Copy(u64),
    Scroll(u64),
}
#[derive(Default)]
pub(super) struct PopupSelection {
    owner: Option<Owner>,
    facts: Option<Facts>,
    selection: Option<crate::selection::TextSelection<()>>,
    pending: Option<(String, Operation)>,
    events: VecDeque<MouseEvent>,
    pointer: Option<MouseEvent>,
    autoscroll: Option<crate::app::state::SelectionAutoscroll>,
    pub(super) deadline: Option<Instant>,
    attempted_surface: Option<u64>,
    waiting_scroll: Option<u64>,
}
pub(super) fn clear(frontend: &mut ClientFrontend) {
    frontend.popup_selection = PopupSelection::default();
}

#[cfg(test)]
pub(super) fn range_for_test(frontend: &ClientFrontend) -> Option<((u32, u16), (u32, u16))> {
    frontend
        .popup_selection
        .selection
        .as_ref()
        .map(crate::selection::TextSelection::ordered_cells)
}
fn surface(frontend: &ClientFrontend) -> Option<&wire::PaneSurfaceFrame> {
    let owner = frontend.popup_selection.owner.as_ref()?;
    if frontend.runtime.shell.active_endpoint_id != owner.endpoint {
        return None;
    }
    let endpoint = frontend.runtime.shell.endpoint(&owner.endpoint)?;
    if endpoint.generation != Some(owner.generation) {
        return None;
    }
    if endpoint.cache.live_snapshot(owner.generation)?.boot_id != owner.boot {
        return None;
    }
    frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .filter(|surface| {
            surface.boot_id == owner.boot
                && surface
                    .popup
                    .as_ref()
                    .is_some_and(|popup| popup.terminal_id == owner.terminal)
        })
}
fn metrics(facts: &Facts) -> crate::pane::ScrollMetrics {
    crate::pane::ScrollMetrics {
        offset_from_bottom: facts.scroll.offset_from_bottom.min(usize::MAX as u64) as usize,
        max_offset_from_bottom: facts.scroll.max_offset_from_bottom.min(usize::MAX as u64) as usize,
        viewport_rows: facts.scroll.viewport_rows.min(usize::MAX as u64) as usize,
    }
}
fn issue(
    frontend: &mut ClientFrontend,
    method: api::Method,
    operation: Operation,
) -> io::Result<()> {
    match frontend.runtime.issue_method_with_id(method) {
        Ok((id, update)) => {
            frontend.popup_selection.pending = Some((id, operation));
            frontend.update(update)?;
        }
        Err(error) => {
            clear(frontend);
            frontend.notice = Some(error);
        }
    }
    Ok(())
}
fn scroll(frontend: &mut ClientFrontend, offset: u64) -> io::Result<()> {
    let Some(facts) = frontend.popup_selection.facts.as_ref() else {
        return Ok(());
    };
    let offset = offset.min(facts.scroll.max_offset_from_bottom);
    if offset == facts.scroll.offset_from_bottom {
        return Ok(());
    }
    let terminal_id = frontend
        .popup_selection
        .owner
        .as_ref()
        .expect("selection owner")
        .terminal
        .clone();
    issue(
        frontend,
        api::Method::PopupScroll(api::PopupScrollParams {
            terminal_id,
            offset_from_bottom: offset,
        }),
        Operation::Scroll(offset),
    )
}
pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    inner: Rect,
    mouse: MouseEvent,
) -> io::Result<bool> {
    if frontend.popup_selection.owner.is_some() {
        if !inner.contains((mouse.column, mouse.row).into())
            && !matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            return Ok(true);
        }
        frontend.popup_selection.events.push_back(mouse);
        observe(frontend)?;
        return Ok(true);
    }
    let Some(popup) = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .and_then(|surface| surface.popup.as_ref())
    else {
        return Ok(false);
    };
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || !frontend.copy_on_select
        || !mouse.modifiers.is_empty()
        || popup.mouse_reporting
        || !inner.contains((mouse.column, mouse.row).into())
    {
        return Ok(false);
    }
    let terminal = popup.terminal_id.clone();
    let endpoint = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .expect("active endpoint");
    let Some(generation) = endpoint.generation else {
        return Ok(true);
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(true);
    };
    frontend.popup_selection.owner = Some(Owner {
        endpoint: endpoint.endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        terminal,
    });
    frontend.popup_selection.events.push_back(mouse);
    observe(frontend)?;
    Ok(true)
}
pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) {
    let Some(owner) = frontend.popup_selection.owner.as_ref() else {
        return;
    };
    let Some((id, _)) = frontend.popup_selection.pending.as_ref() else {
        return;
    };
    if *id != result.request_id
        || owner.endpoint != result.endpoint_id
        || owner.generation != result.generation
        || owner.boot != result.boot_id
    {
        return;
    }
    let terminal = owner.terminal.clone();
    let (_, operation) = frontend
        .popup_selection
        .pending
        .take()
        .expect("matched request");
    let Ok(value) = &result.result else {
        if result
            .result
            .as_ref()
            .err()
            .is_some_and(|error| error.code.as_deref() == Some("stale_content"))
            && matches!(operation, Operation::Facts(_))
        {
            frontend.popup_selection.facts = None;
        } else {
            clear(frontend);
        }
        return;
    };
    let Ok(response) = serde_json::from_value::<api::ResponseResult>(value.clone()) else {
        clear(frontend);
        return;
    };
    match (operation, response) {
        (
            Operation::Facts(requested),
            api::ResponseResult::PopupTerminal {
                terminal_id,
                content_revision,
                scroll,
                surface_revision: Some(revision),
            },
        ) if terminal_id == terminal
            && revision >= requested
            && content_revision.is_multiple_of(2) =>
        {
            // The server can publish a newer frame before answering. Observe consumes
            // these facts only when that exact surface revision has been received.
            frontend.popup_selection.facts = Some(Facts {
                revision: content_revision,
                surface: revision,
                scroll,
            });
        }
        (
            Operation::Copy(revision),
            api::ResponseResult::PopupSelection {
                terminal_id,
                content_revision,
                text,
            },
        ) if terminal_id == terminal && content_revision == revision => {
            let current = surface(frontend).is_some_and(|surface| {
                frontend
                    .popup_selection
                    .facts
                    .as_ref()
                    .is_some_and(|facts| {
                        facts.surface == surface.surface_revision && facts.revision == revision
                    })
            });
            if current && !text.is_empty() {
                crate::selection::write_osc52_bytes(text.as_bytes());
            }
            clear(frontend);
        }
        (
            Operation::Scroll(offset),
            api::ResponseResult::PopupTerminal {
                terminal_id,
                scroll,
                ..
            },
        ) if terminal_id == terminal && scroll.offset_from_bottom == offset => {
            frontend.popup_selection.waiting_scroll = Some(offset);
            frontend.popup_selection.facts = None;
        }
        _ => clear(frontend),
    }
}
pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    if frontend.popup_selection.owner.is_none() {
        return Ok(());
    }
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let Some(surface) = surface(frontend) else {
        clear(frontend);
        return Ok(());
    };
    let revision = surface.surface_revision;
    let popup = surface.popup.as_ref().expect("owned popup");
    let Some(geometry) = super::popup::geometry(popup, view.layout.pane_surface) else {
        clear(frontend);
        return Ok(());
    };
    if !frontend.runtime.input_lease_current() || frontend.popup_selection.pending.is_some() {
        return Ok(());
    }
    if frontend
        .popup_selection
        .facts
        .as_ref()
        .is_none_or(|facts| facts.surface != revision)
    {
        if frontend.popup_selection.attempted_surface == Some(revision) {
            return Ok(());
        }
        frontend.popup_selection.attempted_surface = Some(revision);
        let terminal_id = frontend
            .popup_selection
            .owner
            .as_ref()
            .expect("selection owner")
            .terminal
            .clone();
        return issue(
            frontend,
            api::Method::PopupGet(api::PopupTarget { terminal_id }),
            Operation::Facts(revision),
        );
    }
    let facts = frontend
        .popup_selection
        .facts
        .as_ref()
        .expect("current facts");
    let metric = metrics(facts);
    if let Some(offset) = frontend.popup_selection.waiting_scroll {
        if facts.scroll.offset_from_bottom != offset {
            clear(frontend);
            return Ok(());
        }
        frontend.popup_selection.waiting_scroll = None;
        if let (Some(pointer), Some(selection)) = (
            frontend.popup_selection.pointer,
            frontend.popup_selection.selection.as_mut(),
        ) {
            selection.drag(pointer.column, pointer.row, geometry.inner, Some(metric));
        }
    }
    if frontend
        .popup_selection
        .autoscroll
        .as_ref()
        .is_some_and(|state| state.inner_rect != geometry.inner)
    {
        frontend.popup_selection.autoscroll = None;
        frontend.popup_selection.deadline = None;
    }
    while let Some(mouse) = frontend.popup_selection.events.pop_front() {
        frontend.popup_selection.pointer = Some(mouse);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if frontend.popup_selection.selection.is_some() {
                    clear(frontend);
                    return Ok(());
                }
                if geometry.inner.contains((mouse.column, mouse.row).into()) {
                    frontend.popup_selection.selection =
                        Some(crate::selection::TextSelection::anchor(
                            (),
                            mouse.row - geometry.inner.y,
                            mouse.column - geometry.inner.x,
                            Some(metric),
                        ));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(selection) = frontend.popup_selection.selection.as_mut() {
                    let (autoscroll, offset) = super::selection::drag_text_selection(
                        selection,
                        geometry.inner,
                        metric,
                        mouse,
                    );
                    frontend.popup_selection.deadline = autoscroll
                        .as_ref()
                        .map(|_| Instant::now() + crate::app::SELECTION_AUTOSCROLL_INTERVAL);
                    frontend.popup_selection.autoscroll = autoscroll;
                    if let Some(offset) = offset {
                        scroll(frontend, offset)?;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                frontend.popup_selection.autoscroll = None;
                frontend.popup_selection.deadline = None;
                let Some(mut selection) = frontend.popup_selection.selection.take() else {
                    clear(frontend);
                    return Ok(());
                };
                if !selection.finish() {
                    clear(frontend);
                    return Ok(());
                }
                let ((ar, ac), (cr, cc)) = selection.ordered_cells();
                let terminal_id = frontend
                    .popup_selection
                    .owner
                    .as_ref()
                    .expect("selection owner")
                    .terminal
                    .clone();
                let revision = frontend
                    .popup_selection
                    .facts
                    .as_ref()
                    .expect("current facts")
                    .revision;
                issue(
                    frontend,
                    api::Method::PopupSelectionRead(api::PopupSelectionReadParams {
                        terminal_id,
                        anchor: api::PaneTextPoint { row: ar, col: ac },
                        cursor: api::PaneTextPoint { row: cr, col: cc },
                        content_revision: revision,
                    }),
                    Operation::Copy(revision),
                )?;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let lines = frontend.chrome.settings.mouse_scroll_lines as u64;
                let offset = if mouse.kind == MouseEventKind::ScrollUp {
                    (metric.offset_from_bottom as u64).saturating_add(lines)
                } else {
                    (metric.offset_from_bottom as u64).saturating_sub(lines)
                };
                scroll(frontend, offset)?;
            }
            _ => {}
        }
        if frontend.popup_selection.pending.is_some() {
            break;
        }
    }
    Ok(())
}
pub(super) fn tick(frontend: &mut ClientFrontend, now: Instant) -> io::Result<()> {
    frontend.popup_selection.deadline = None;
    let Some(state) = frontend.popup_selection.autoscroll.as_ref() else {
        return Ok(());
    };
    let up = state.direction == crate::app::state::SelectionAutoscrollDirection::Up;
    frontend.popup_selection.deadline = Some(now + crate::app::SELECTION_AUTOSCROLL_INTERVAL);
    if !frontend.runtime.input_lease_current()
        || frontend.popup_selection.pending.is_some()
        || frontend.popup_selection.waiting_scroll.is_some()
    {
        return Ok(());
    }
    let Some(facts) = frontend.popup_selection.facts.as_ref() else {
        return Ok(());
    };
    if (up && facts.scroll.offset_from_bottom == facts.scroll.max_offset_from_bottom)
        || (!up && facts.scroll.offset_from_bottom == 0)
    {
        frontend.popup_selection.autoscroll = None;
        frontend.popup_selection.deadline = None;
        return Ok(());
    }
    let offset = if up {
        facts.scroll.offset_from_bottom.saturating_add(1)
    } else {
        facts.scroll.offset_from_bottom.saturating_sub(1)
    };
    scroll(frontend, offset)
}
pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, area: Rect) {
    let Some(surface) = surface(frontend) else {
        return;
    };
    let Some(facts) = frontend
        .popup_selection
        .facts
        .as_ref()
        .filter(|facts| facts.surface == surface.surface_revision)
    else {
        return;
    };
    let Some(selection) = frontend.popup_selection.selection.as_ref() else {
        return;
    };
    let Some(geometry) = super::popup::geometry(surface.popup.as_ref().expect("owned popup"), area)
    else {
        return;
    };
    let style = crate::ui::automatic_selection_style(
        &frontend.chrome.settings.palette,
        frontend.host_theme,
    );
    for y in 0..geometry.inner.height {
        for x in 0..geometry.inner.width {
            if selection.contains(y, x, Some(metrics(facts))) {
                frame.buffer_mut()[(geometry.inner.x + x, geometry.inner.y + y)].set_style(style);
            }
        }
    }
}
