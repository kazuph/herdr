//! Host graphics belong to the currently presented endpoint generation and boot.
use super::*;
use crate::kitty_graphics::endpoint_client::{ClientState, Occlusion, Visibility};

pub(super) fn encode(
    frontend: &ClientFrontend,
    view: &super::super::chrome::ChromeView,
) -> (ClientState, Vec<u8>) {
    let mut next = frontend.graphics.clone();
    let endpoint = &frontend.runtime.shell.active_endpoint_id;
    let owner = frontend.runtime.shell.endpoint(endpoint).and_then(|entry| {
        let generation = entry.generation?;
        let snapshot = entry.cache.live_snapshot(generation)?;
        Some(match endpoint {
            ClientEndpointId::Local => snapshot.boot_id.clone(),
            ClientEndpointId::Ssh(profile) => format!("{profile}:{}", snapshot.boot_id),
        })
    });
    next.set_scope(owner.as_deref().unwrap_or_default());
    let cell = crate::kitty_graphics::HostCellSize {
        width_px: frontend.options.cell_width_px,
        height_px: frontend.options.cell_height_px,
    };
    let mut visibility = Visibility::Hidden;
    let mut occlusion = Occlusion::default();
    let mut popup_origin = None;
    if let Some(surface) = view.surface.as_ref().filter(|surface| {
        frontend
            .runtime
            .shell
            .endpoint(endpoint)
            .and_then(|entry| entry.generation)
            .is_some_and(|generation| {
                frontend
                    .runtime
                    .shell
                    .endpoint_surface_matches(endpoint, generation, surface)
            })
    }) {
        next.set_scene(surface.graphics.clone());
        visibility = Visibility::Main;
        if let Some(geometry) = surface
            .popup
            .as_deref()
            .and_then(|popup| popup::geometry(popup, view.layout.pane_surface))
        {
            visibility = Visibility::Popup;
            occlusion.start_popup(geometry.outer);
            popup_origin = Some((geometry.inner.x, geometry.inner.y));
        }
    }
    for rect in [
        notification::rect(frontend),
        mobile::graphics_rect(frontend, view.layout.pane_surface),
        navigator::graphics_rect(frontend),
        menu::graphics_rect(frontend, view.menu_launcher),
        context::graphics_rect(frontend),
        notes::graphics_rect(frontend),
        help::graphics_rect(frontend),
        settings::graphics_rect(frontend),
        worktrees::graphics_rect(frontend),
        modal::graphics_rect(frontend, view.layout.pane_surface),
    ]
    .into_iter()
    .flatten()
    {
        occlusion.cover(rect);
    }
    if frontend.resize_mode.is_some() && view.layout.pane_surface.height > 0 {
        let area = view.layout.pane_surface;
        occlusion.cover(ratatui::layout::Rect::new(
            area.x,
            area.bottom() - 1,
            area.width,
            1,
        ));
    }
    if copy::active(frontend) && frontend.rows > 0 {
        occlusion.cover(ratatui::layout::Rect::new(
            0,
            frontend.rows - 1,
            frontend.cols,
            1,
        ));
    }
    if frontend.notice.is_some() && frontend.rows > 0 {
        occlusion.cover(ratatui::layout::Rect::new(
            0,
            frontend.rows - 1,
            frontend.cols,
            1,
        ));
    }
    let bytes = next.encode(
        visibility,
        (view.layout.pane_surface.x, view.layout.pane_surface.y),
        popup_origin,
        cell,
        &occlusion,
    );
    (next, bytes)
}
