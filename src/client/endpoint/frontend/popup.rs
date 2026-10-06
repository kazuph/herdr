//! Popup pointer events use their endpoint-owned terminal and the existing popup geometry.
use super::super::ResourceKey;
use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

pub(super) fn geometry(
    popup: &wire::ClientShellPopupSurface,
    area: Rect,
) -> Option<crate::popup_size::PopupResolvedGeometry> {
    let size = |size| match size {
        wire::ClientShellPopupSize::Cells(cells) => crate::popup_size::PopupSize::Cells(cells),
        wire::ClientShellPopupSize::Percent(percent) => {
            crate::popup_size::PopupSize::Percent(percent)
        }
    };
    crate::popup_size::resolve_popup_geometry(popup.width.map(size), popup.height.map(size), area)
}

pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    area: Rect,
    mouse: MouseEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<bool> {
    let Some(popup) = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .and_then(|surface| surface.popup.as_deref())
        .cloned()
    else {
        return Ok(false);
    };
    if !frontend.runtime.input_lease_current() {
        return Ok(true);
    }
    let Some(geometry) = geometry(&popup, area) else {
        return Ok(true);
    };
    let point = (mouse.column, mouse.row).into();
    if !geometry.outer.contains(point) && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        match frontend
            .runtime
            .issue_method(crate::api::schema::Method::PopupClose(
                crate::api::schema::EmptyParams {},
            )) {
            Ok(update) => {
                frontend.update(update)?;
            }
            Err(error) => frontend.notice = Some(error),
        }
        return Ok(true);
    }
    if super::popup_selection::mouse(frontend, geometry.inner, mouse)? {
        return Ok(true);
    }
    if !geometry.inner.contains(point) {
        return Ok(true);
    }
    let event = input::surface_mouse_event(
        input::MouseSurface {
            origin: Rect::default(),
            inner: geometry.inner.into(),
            sgr_pixel_mouse: popup.sgr_pixel_mouse,
            pixel_width: popup.pixel_width,
            pixel_height: popup.pixel_height,
        },
        mouse,
        pixels,
        frontend.chrome.settings.mouse_scroll_lines,
    );
    if let Some(event) = event {
        frontend.runtime.popup_input(
            &ResourceKey {
                endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
                id: popup.terminal_id,
            },
            vec![event],
        );
    }
    Ok(true)
}
