//! Resize presentation targets the selected viewer pane; ratios remain server-owned.
use super::*;
use crate::api::schema as api;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind};

pub(super) struct SplitDrag {
    owner: copy::Owner,
    split: wire::PaneSurfaceSplit,
    origin: ratatui::layout::Rect,
    grab_offset: u16,
    ratio: Option<f32>,
    released: bool,
}

pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    origin: ratatui::layout::Rect,
    mouse: crossterm::event::MouseEvent,
) -> io::Result<bool> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if let Some(drag) = frontend.split_drag.as_mut() {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let area = drag.split.area;
                let ratio = match drag.split.direction {
                    wire::PaneSurfaceSplitDirection::Horizontal => {
                        mouse
                            .column
                            .saturating_sub(drag.origin.x)
                            .saturating_add(drag.grab_offset)
                            .saturating_sub(area.x) as f32
                            / area.width.max(1) as f32
                    }
                    wire::PaneSurfaceSplitDirection::Vertical => {
                        mouse
                            .row
                            .saturating_sub(drag.origin.y)
                            .saturating_add(drag.grab_offset)
                            .saturating_sub(area.y) as f32
                            / area.height.max(1) as f32
                    }
                };
                // Existing fork mouse split-drag bounds, not a new resize policy.
                drag.ratio = Some(ratio.clamp(0.1, 0.9));
            }
            MouseEventKind::Up(MouseButton::Left) => drag.released = true,
            _ => return Ok(true),
        }
        observe_drag(frontend)?;
        return Ok(true);
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || !frontend.runtime.input_lease_current()
        || copy::active(frontend)
    {
        return Ok(false);
    }
    let Some(surface) = frontend.runtime.shell.pane_surface.as_ref() else {
        return Ok(false);
    };
    let Some(split) = surface
        .splits
        .iter()
        .find(|split| {
            let hit = split.hit_rect;
            ratatui::layout::Rect::new(origin.x + hit.x, origin.y + hit.y, hit.width, hit.height)
                .contains((mouse.column, mouse.row).into())
        })
        .cloned()
    else {
        return Ok(false);
    };
    let Some(pane) = surface.panes.iter().find(|pane| pane.focused) else {
        return Ok(false);
    };
    let id = &frontend.runtime.shell.active_endpoint_id;
    let Some(endpoint) = frontend.runtime.shell.endpoint(id) else {
        return Ok(false);
    };
    let Some(generation) = endpoint.generation else {
        return Ok(false);
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(false);
    };
    let grab_offset = match split.direction {
        wire::PaneSurfaceSplitDirection::Horizontal => split
            .pos
            .saturating_sub(mouse.column.saturating_sub(origin.x)),
        wire::PaneSurfaceSplitDirection::Vertical => {
            split.pos.saturating_sub(mouse.row.saturating_sub(origin.y))
        }
    };
    frontend.split_drag = Some(SplitDrag {
        owner: copy::Owner {
            endpoint: id.clone(),
            generation,
            boot: snapshot.boot_id.clone(),
            pane: pane.pane_id.clone(),
        },
        split,
        origin,
        grab_offset,
        ratio: None,
        released: false,
    });
    Ok(true)
}

pub(super) fn observe_drag(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(drag) = frontend.split_drag.as_ref() else {
        return Ok(());
    };
    if !drag.owner.exists(frontend)
        || drag.owner.endpoint != frontend.runtime.shell.active_endpoint_id
    {
        frontend.split_drag = None;
        return Ok(());
    }
    if drag.owner.pane(frontend).is_none() || !frontend.runtime.input_lease_current() {
        return Ok(());
    }
    let Some(drag) = frontend.split_drag.as_mut() else {
        return Ok(());
    };
    let method = drag.ratio.take().map(|ratio| {
        api::Method::LayoutSetSplitRatio(api::LayoutSetSplitRatioParams {
            tab_id: None,
            pane_id: Some(drag.owner.pane.clone()),
            path: drag.split.path.clone(),
            ratio,
        })
    });
    if drag.released {
        frontend.split_drag = None;
    }
    if let Some(method) = method {
        match frontend.runtime.issue_method(method) {
            Ok(update) => {
                frontend.update(update)?;
            }
            Err(error) => {
                frontend.split_drag = None;
                frontend.notice = Some(error);
            }
        }
    }
    Ok(())
}

pub(super) fn enter(frontend: &mut ClientFrontend) {
    if !frontend.runtime.input_lease_current() {
        return;
    }
    let id = &frontend.runtime.shell.active_endpoint_id;
    let Some(endpoint) = frontend.runtime.shell.endpoint(id) else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    let Some(pane) = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .and_then(|surface| surface.panes.iter().find(|pane| pane.focused))
    else {
        return;
    };
    frontend.resize_mode = Some(copy::Owner {
        endpoint: id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        pane: pane.pane_id.clone(),
    });
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend.resize_mode.as_ref().is_some_and(|owner| {
        !owner.exists(frontend) || owner.endpoint != frontend.runtime.shell.active_endpoint_id
    }) {
        frontend.resize_mode = None;
    }
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        return Ok(false);
    }
    observe(frontend);
    let Some(owner) = frontend.resize_mode.as_ref() else {
        return Ok(false);
    };
    let RawInputEvent::Key(key) = event else {
        return Ok(true);
    };
    if key.kind == KeyEventKind::Release {
        return Ok(true);
    }
    let binding = &frontend.keybinds.keybinds.resize_mode;
    if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
        || binding.matches_prefix_key(*key)
        || binding.matches_direct_key(*key)
    {
        frontend.resize_mode = None;
        return Ok(true);
    }
    if owner.pane(frontend).is_none() || !frontend.runtime.input_lease_current() {
        return Ok(true);
    }
    let direction = match key.code {
        KeyCode::Char('h') | KeyCode::Left => api::PaneDirection::Left,
        KeyCode::Char('l') | KeyCode::Right => api::PaneDirection::Right,
        KeyCode::Char('j') | KeyCode::Down => api::PaneDirection::Down,
        KeyCode::Char('k') | KeyCode::Up => api::PaneDirection::Up,
        _ => return Ok(true),
    };
    let method = api::Method::PaneResize(api::PaneResizeParams {
        pane_id: Some(owner.pane.clone()),
        direction,
        amount: None,
    });
    match frontend.runtime.issue_method(method) {
        Ok(update) => {
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(true)
}
