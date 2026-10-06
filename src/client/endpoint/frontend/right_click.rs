//! The fork's configured right-button gesture belongs to its captured viewer pane.
use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::VecDeque;

pub(super) struct Gesture {
    owner: copy::Owner,
    modifiers: KeyModifiers,
    events: VecDeque<(MouseEvent, Option<crate::input::mouse::HostPixels>)>,
    ended: bool,
}

pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    origin: Rect,
    mouse: MouseEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<bool> {
    if let Some(gesture) = frontend.right_click.as_mut() {
        if matches!(
            mouse.kind,
            MouseEventKind::Drag(MouseButton::Right) | MouseEventKind::Up(MouseButton::Right)
        ) {
            gesture.events.push_back((mouse, pixels));
            gesture.ended = matches!(mouse.kind, MouseEventKind::Up(MouseButton::Right));
            observe(frontend)?;
            return Ok(true);
        }
        frontend.right_click = None;
    }
    let Some(modifiers) = frontend.host_settings.right_click_passthrough else {
        return Ok(false);
    };
    if frontend.prefix
        || copy::active(frontend)
        || mouse.kind != MouseEventKind::Down(MouseButton::Right)
        || mouse.modifiers != modifiers
    {
        return Ok(false);
    }
    let Some(pane) = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .and_then(|surface| {
            surface.panes.iter().find(|pane| {
                let inner = pane.inner_rect;
                pane.mouse_reporting
                    && Rect::new(
                        origin.x + inner.x,
                        origin.y + inner.y,
                        inner.width,
                        inner.height,
                    )
                    .contains((mouse.column, mouse.row).into())
            })
        })
        .cloned()
    else {
        return Ok(false);
    };
    let endpoint_id = frontend.runtime.shell.active_endpoint_id.clone();
    let Some(endpoint) = frontend.runtime.shell.endpoint(&endpoint_id) else {
        return Ok(false);
    };
    let Some(generation) = endpoint.generation else {
        return Ok(false);
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(false);
    };
    let owner = copy::Owner {
        endpoint: endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        pane: pane.pane_id.clone(),
    };
    selection::clear(frontend);
    frontend.right_click = Some(Gesture {
        owner,
        modifiers,
        events: VecDeque::from([(mouse, pixels)]),
        ended: false,
    });
    if !pane.focused {
        let update = frontend.runtime.activate(
            endpoint_id,
            Some(super::super::FocusTarget::Pane(pane.pane_id)),
            Instant::now(),
        );
        frontend.update(update)?;
    }
    observe(frontend)?;
    Ok(true)
}

pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(mut gesture) = frontend.right_click.take() else {
        return Ok(());
    };
    if !gesture.owner.exists(frontend)
        || gesture.owner.endpoint != frontend.runtime.shell.active_endpoint_id
        || frontend.runtime.shell.outer_focused == Some(false)
    {
        return Ok(());
    }
    if frontend.runtime.input_lease_current() {
        if let Some(pane) = gesture.owner.pane(frontend).cloned() {
            let view =
                frontend
                    .chrome
                    .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
            while let Some((mouse, pixels)) = gesture.events.pop_front() {
                input::send_pane_mouse(
                    frontend,
                    view.layout.pane_surface,
                    &super::super::ResourceKey {
                        endpoint: gesture.owner.endpoint.clone(),
                        id: gesture.owner.pane.clone(),
                    },
                    &pane,
                    MouseEvent {
                        modifiers: mouse.modifiers.difference(gesture.modifiers),
                        ..mouse
                    },
                    pixels,
                )?;
            }
        }
    }
    if !gesture.ended || !gesture.events.is_empty() {
        frontend.right_click = Some(gesture);
    }
    Ok(())
}
