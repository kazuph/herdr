//! The text and geometry producer for a viewer's explicit tab.
//! Ported from fixed upstream client_shell/render_stream; popup and graphics compose separately.

// Consumed by the endpoint viewer independently from the private full-app renderer.
#![allow(dead_code)]
use std::collections::HashMap;

use ratatui::layout::Rect;

use crate::app::App;
use crate::kitty_graphics::HostCellSize;
use crate::protocol::endpoint_wire::{
    FrameData, PaneSurfacePane, PaneSurfaceScrollMetrics, PaneSurfaceSplit,
    PaneSurfaceSplitDirection,
};
use crate::ui::tab_surface::{
    compute_tab_surface_for, render_tab_surface, tab_surface_cursor, tab_surface_hyperlinks,
    TabSurfaceTarget, TargetTabSurfaceView,
};

pub(crate) struct RenderedTextSurface {
    pub(crate) frame: FrameData,
    pub(crate) panes: Vec<PaneSurfacePane>,
    pub(crate) splits: Vec<PaneSurfaceSplit>,
    pub(crate) popup: Option<Box<crate::protocol::endpoint_wire::ClientShellPopupSurface>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SurfaceRenderDeferred {
    Synchronized,
    Changed,
}

pub(crate) fn render_text_surface(
    app: &App,
    target: Option<TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: HostCellSize,
) -> Result<RenderedTextSurface, SurfaceRenderDeferred> {
    let layout = compute_tab_surface_for(
        &app.state,
        &app.terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    );
    let mut revisions = HashMap::new();
    if let Some(target) = target {
        for pane in &layout.pane_infos {
            if let Some(runtime) = app.state.runtime_for_pane_in_workspace(
                &app.terminal_runtimes,
                target.workspace_index,
                pane.id,
            ) {
                let (synchronized, epoch) = runtime.synchronized_output_state();
                if synchronized {
                    return Err(SurfaceRenderDeferred::Synchronized);
                }
                revisions.insert(pane.id, (epoch, runtime.content_seq()));
            }
        }
    }
    let view = TargetTabSurfaceView {
        target: layout.target,
        pane_infos: &layout.pane_infos,
        split_borders: &layout.split_borders,
    };
    let cursor = tab_surface_cursor(&app.state, &app.terminal_runtimes, view);
    let hyperlinks = tab_surface_hyperlinks(&app.state, &app.terminal_runtimes, view);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("in-memory terminal creation is infallible");
    terminal
        .draw(|frame| render_tab_surface(&app.state, &app.terminal_runtimes, view, frame))
        .expect("in-memory drawing is infallible");
    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        terminal.backend().buffer(),
        cursor,
        &hyperlinks,
    );
    let panes = target
        .map(|target| {
            layout
                .pane_infos
                .iter()
                .filter_map(|pane| {
                    let pane_id = app.public_pane_id(target.workspace_index, pane.id)?;
                    let runtime = app.state.runtime_for_pane_in_workspace(
                        &app.terminal_runtimes,
                        target.workspace_index,
                        pane.id,
                    );
                    let content_revision = runtime.map_or(0, |runtime| {
                        let after = runtime.content_seq();
                        if revisions
                            .get(&pane.id)
                            .is_some_and(|&(_, before)| before == after)
                            && after.is_multiple_of(2)
                        {
                            after
                        } else {
                            after | 1
                        }
                    });
                    Some(PaneSurfacePane {
                        pane_id,
                        content_revision,
                        rect: pane.rect.into(),
                        inner_rect: pane.inner_rect.into(),
                        scrollbar_rect: pane.scrollbar_rect.map(Into::into),
                        scroll: runtime.and_then(|runtime| runtime.scroll_metrics()).map(
                            |metrics| PaneSurfaceScrollMetrics {
                                offset_from_bottom: metrics.offset_from_bottom as u64,
                                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                                viewport_rows: metrics.viewport_rows as u64,
                            },
                        ),
                        focused: pane.is_focused,
                        mouse_reporting: runtime
                            .and_then(|runtime| runtime.input_state())
                            .is_some_and(crate::pane::InputState::mouse_reporting_enabled),
                        sgr_pixel_mouse: runtime
                            .and_then(|runtime| runtime.input_state())
                            .is_some_and(|state| {
                                state.mouse_protocol_encoding
                                    == crate::input::MouseProtocolEncoding::SgrPixels
                            }),
                        alternate_screen_active: runtime
                            .and_then(|runtime| runtime.input_state())
                            .is_some_and(|state| state.alternate_screen),
                        pixel_width: if cell_size.is_known() {
                            u32::from(pane.inner_rect.width) * cell_size.width_px
                        } else {
                            0
                        },
                        pixel_height: if cell_size.is_known() {
                            u32::from(pane.inner_rect.height) * cell_size.height_px
                        } else {
                            0
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let pane_frames = layout
        .pane_infos
        .iter()
        .map(|pane| pane.rect)
        .collect::<Vec<_>>();
    let splits = layout
        .split_borders
        .iter()
        .filter_map(|split| {
            let hit_rect = split_hit_rect(
                split,
                app.state.pane_borders,
                app.state.pane_gaps,
                &pane_frames,
            )?;
            Some(PaneSurfaceSplit {
                direction: match split.direction {
                    ratatui::layout::Direction::Horizontal => PaneSurfaceSplitDirection::Horizontal,
                    ratatui::layout::Direction::Vertical => PaneSurfaceSplitDirection::Vertical,
                },
                pos: split.pos,
                area: split.area.into(),
                hit_rect: hit_rect.into(),
                path: split.path.clone(),
            })
        })
        .collect();
    if let Some(target) = target {
        for (pane_id, (epoch, _)) in revisions {
            if let Some(runtime) = app.state.runtime_for_pane_in_workspace(
                &app.terminal_runtimes,
                target.workspace_index,
                pane_id,
            ) {
                let (synchronized, after_epoch) = runtime.synchronized_output_state();
                if synchronized {
                    return Err(SurfaceRenderDeferred::Synchronized);
                }
                if epoch != after_epoch {
                    return Err(SurfaceRenderDeferred::Changed);
                }
            }
        }
    }
    Ok(RenderedTextSurface {
        frame,
        panes,
        splits,
        popup: render_popup_surface(app, target, area, resize_panes, cell_size)?,
    })
}

// Fixed upstream popup producer, selecting the fork's workspace-owned popup explicitly.
fn render_popup_surface(
    app: &App,
    target: Option<TabSurfaceTarget>,
    area: Rect,
    resize_runtime: bool,
    cell_size: HostCellSize,
) -> Result<
    Option<Box<crate::protocol::endpoint_wire::ClientShellPopupSurface>>,
    SurfaceRenderDeferred,
> {
    let Some(workspace) =
        target.and_then(|target| app.state.workspaces.get(target.workspace_index))
    else {
        return Ok(None);
    };
    let Some(popup) = app.state.popup_pane_for_workspace(&workspace.id) else {
        return Ok(None);
    };
    let Some(geometry) = crate::popup_size::resolve_popup_geometry(popup.width, popup.height, area)
    else {
        return Ok(None);
    };
    let Some(runtime) = app.terminal_runtimes.get(&popup.terminal_id) else {
        return Ok(None);
    };
    if resize_runtime
        && !app
            .state
            .direct_attach_resize_locks
            .contains(&popup.terminal_id)
    {
        runtime.resize(
            geometry.inner.height,
            geometry.inner.width,
            cell_size.width_px,
            cell_size.height_px,
        );
    }
    let (synchronized, epoch) = runtime.synchronized_output_state();
    if synchronized {
        return Err(SurfaceRenderDeferred::Synchronized);
    }
    let content_area = Rect::new(0, 0, geometry.inner.width, geometry.inner.height);
    let (buffer, cursor) =
        crate::server::render_stream::render_terminal_virtual(runtime, content_area);
    let hyperlinks = runtime.visible_hyperlinks(content_area);
    let (synchronized, after_epoch) = runtime.synchronized_output_state();
    if synchronized {
        return Err(SurfaceRenderDeferred::Synchronized);
    }
    if epoch != after_epoch {
        return Err(SurfaceRenderDeferred::Changed);
    }
    let title = app
        .state
        .terminals
        .get(&popup.terminal_id)
        .and_then(|terminal| terminal.manual_label.clone())
        .unwrap_or_else(|| "popup".to_owned());
    let (pixel_width, pixel_height) = if cell_size.is_known() {
        (
            u32::from(content_area.width) * cell_size.width_px,
            u32::from(content_area.height) * cell_size.height_px,
        )
    } else {
        (0, 0)
    };
    Ok(Some(Box::new(
        crate::protocol::endpoint_wire::ClientShellPopupSurface {
            terminal_id: popup.terminal_id.to_string(),
            title,
            width: popup.width.map(client_popup_size),
            height: popup.height.map(client_popup_size),
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                &buffer,
                cursor.map(|cursor| crate::protocol::endpoint_wire::CursorState {
                    x: cursor.x,
                    y: cursor.y,
                    visible: cursor.visible,
                    shape: cursor.shape,
                }),
                &hyperlinks,
            ),
            mouse_reporting: runtime
                .input_state()
                .is_some_and(crate::pane::InputState::mouse_reporting_enabled),
            sgr_pixel_mouse: runtime.input_state().is_some_and(|state| {
                state.mouse_protocol_encoding == crate::input::MouseProtocolEncoding::SgrPixels
            }),
            pixel_width,
            pixel_height,
        },
    )))
}
fn client_popup_size(
    size: crate::popup_size::PopupSize,
) -> crate::protocol::endpoint_wire::ClientShellPopupSize {
    match size {
        crate::popup_size::PopupSize::Cells(cells) => {
            crate::protocol::endpoint_wire::ClientShellPopupSize::Cells(cells)
        }
        crate::popup_size::PopupSize::Percent(percent) => {
            crate::protocol::endpoint_wire::ClientShellPopupSize::Percent(percent)
        }
    }
}

fn split_hit_rect(
    split: &crate::layout::SplitBorder,
    pane_borders: bool,
    pane_gaps: bool,
    pane_frames: &[Rect],
) -> Option<Rect> {
    let hit = match (split.direction, pane_borders, pane_gaps) {
        (ratatui::layout::Direction::Horizontal, true, false) => {
            Rect::new(split.pos, split.area.y, 1, split.area.height)
        }
        (ratatui::layout::Direction::Horizontal, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                start,
                split.area.y,
                split.pos.saturating_sub(start).saturating_add(1),
                split.area.height,
            )
        }
        (ratatui::layout::Direction::Horizontal, false, true) => Rect::new(
            split.pos.checked_sub(1)?,
            split.area.y,
            1,
            split.area.height,
        ),
        (ratatui::layout::Direction::Vertical, true, false) => {
            Rect::new(split.area.x, split.pos, split.area.width, 1)
        }
        (ratatui::layout::Direction::Vertical, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                split.area.x,
                start,
                split.area.width,
                split.pos.saturating_sub(start).saturating_add(1),
            )
        }
        (ratatui::layout::Direction::Vertical, false, true) => {
            Rect::new(split.area.x, split.pos.checked_sub(1)?, split.area.width, 1)
        }
        (_, false, false) => return None,
    };
    if !pane_borders
        && pane_frames.iter().any(|pane| {
            hit.x < pane.right()
                && hit.right() > pane.x
                && hit.y < pane.bottom()
                && hit.bottom() > pane.y
        })
    {
        return None;
    }
    Some(hit)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::sync::{mpsc, Notify};

    async fn received(
        runtime: &crate::terminal::TerminalRuntime,
        notify: &Notify,
        dirty: &AtomicBool,
        marker: &str,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                dirty.store(false, Ordering::Release);
                if runtime.visible_text().contains(marker) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("owned PTY must emit its marker");
    }

    #[tokio::test]
    async fn explicit_nonselected_surface_renders_real_pty_and_resizes_only_its_target() {
        let (_, requests) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            requests,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("endpoint-owned");
        let second = workspace.test_add_tab(Some("second"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let mut hooks = Vec::new();
        for tab_index in [0, second] {
            let tab = &app.state.workspaces[0].tabs[tab_index];
            let pane_id = tab.root_pane;
            let terminal_id = tab.terminal_id(pane_id).unwrap().clone();
            let (events, _) = mpsc::channel(64);
            let notify = Arc::new(Notify::new());
            let dirty = Arc::new(AtomicBool::new(false));
            let marker = format!("OWNED-TAB-{tab_index}");
            let command = format!(
                "printf '\\033_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,C=1,q=2;/wAA/w==\\033\\\\'; printf '{marker}\\n'; while IFS= read -r line; do case \"$line\" in HIDE) printf '\\033_Ga=d,d=i,i=7,q=2;\\033\\\\';; REPLAY) printf '\\033_Ga=p,i=7,p=3,c=10,r=5,q=2;\\033\\\\';; DELETE) printf '\\033_Ga=d,d=I,i=7,q=2;\\033\\\\';; esac; printf 'OWNED-%s-DONE\\n' \"$line\"; done"
            );
            let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
                pane_id,
                24,
                80,
                std::env::current_dir().unwrap(),
                &["/bin/sh".into(), "-c".into(), command],
                &crate::pane::PaneLaunchEnv::default(),
                crate::pane::AgentDetection::Disabled,
                0,
                crate::terminal_theme::TerminalTheme::default(),
                events,
                notify.clone(),
                dirty.clone(),
            )
            .unwrap();
            received(&runtime, &notify, &dirty, &marker).await;
            assert!(runtime
                .child_pid()
                .is_some_and(|pid| pid != std::process::id()));
            app.terminal_runtimes.insert(terminal_id.clone(), runtime);
            hooks.push((terminal_id, notify, dirty));
        }
        let area = Rect::new(0, 0, 40, 8);
        let target = Some(TabSurfaceTarget {
            workspace_index: 0,
            tab_index: second,
            pane_focus: None,
        });
        let surface = render_text_surface(
            &app,
            target,
            area,
            true,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
        )
        .unwrap();
        assert_eq!(
            surface.frame.cells.len(),
            usize::from(area.width) * usize::from(area.height)
        );
        let text = surface
            .frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains(&format!("OWNED-TAB-{second}")), "{text}");
        assert!(!text.contains("OWNED-TAB-0"), "{text}");
        assert_eq!(surface.panes.len(), 1);
        assert_eq!(surface.panes[0].inner_rect, Rect::new(1, 1, 37, 6).into());
        assert_eq!(surface.panes[0].pixel_width, 37 * 8);
        assert_eq!(surface.panes[0].pixel_height, 6 * 16);
        let (scene, delivery) = crate::kitty_graphics::endpoint_scene::collect(
            &app,
            target,
            &surface.panes,
            surface.popup.as_deref(),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Default::default(),
        );
        assert_eq!(scene.assets.len(), 1);
        assert_eq!(scene.assets[0].data, [255, 0, 0, 255]);
        assert!(matches!(&scene.assets[0].key.source,
            crate::protocol::endpoint_wire::SurfaceGraphicsSource::Terminal {
                target: crate::protocol::endpoint_wire::SurfaceGraphicsTarget::Pane { pane_id },
                image_id: 7,
            } if pane_id == &surface.panes[0].pane_id));
        let (retained, _) = crate::kitty_graphics::endpoint_scene::collect(
            &app,
            target,
            &surface.panes,
            surface.popup.as_deref(),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &delivery,
        );
        assert!(retained.assets.is_empty());
        assert_eq!(retained.placements, scene.placements);
        let (terminal_id, notify, dirty) = &hooks[second];
        let runtime = app.terminal_runtimes.get(terminal_id).unwrap();
        runtime.send_paste("HIDE\n".into()).await.unwrap();
        received(runtime, notify, dirty, "OWNED-HIDE-DONE").await;
        let (hidden, hidden_delivery) = crate::kitty_graphics::endpoint_scene::collect(
            &app,
            target,
            &surface.panes,
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &delivery,
        );
        assert!(hidden.placements.is_empty());
        assert!(hidden.assets.is_empty());
        assert_eq!(hidden.retained_assets, vec![scene.assets[0].key.clone()]);
        runtime.send_paste("REPLAY\n".into()).await.unwrap();
        received(runtime, notify, dirty, "OWNED-REPLAY-DONE").await;
        let (replayed, replay_delivery) = crate::kitty_graphics::endpoint_scene::collect(
            &app,
            target,
            &surface.panes,
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &hidden_delivery,
        );
        assert!(replayed.assets.is_empty());
        assert_eq!(replayed.placements.len(), 1);
        assert_eq!(replayed.placements[0].asset, scene.assets[0].key);
        runtime.send_paste("DELETE\n".into()).await.unwrap();
        received(runtime, notify, dirty, "OWNED-DELETE-DONE").await;
        let (deleted, _) = crate::kitty_graphics::endpoint_scene::collect(
            &app,
            target,
            &surface.panes,
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &replay_delivery,
        );
        assert!(deleted.assets.is_empty());
        assert!(deleted.placements.is_empty());
        assert!(deleted.retained_assets.is_empty());

        assert_eq!(
            app.terminal_runtimes
                .get(&hooks[0].0)
                .unwrap()
                .current_size(),
            (24, 80)
        );
        assert_eq!(
            app.terminal_runtimes
                .get(&hooks[second].0)
                .unwrap()
                .current_size(),
            (6, 37)
        );
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab_index(), 0);
        let (terminal_id, notify, dirty) = &hooks[second];
        let runtime = app.terminal_runtimes.get(terminal_id).unwrap();
        runtime
            .send_paste("TARGET-PTY-MARKER\n".into())
            .await
            .unwrap();
        received(runtime, notify, dirty, "TARGET-PTY-MARKER").await;
        let surface =
            render_text_surface(&app, target, area, false, HostCellSize::default()).unwrap();
        assert!(surface
            .frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
            .contains("TARGET-PTY-MARKER"));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab_index(), 0);
        app.state.assert_invariants_for_test();
        app.spawn_popup_argv_command(
            &["/bin/sh".into(), "-c".into(), "printf 'OWNED-POPUP\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\"; done".into()],
            Some(std::env::current_dir().unwrap()), Vec::new(), Default::default(),
        ).unwrap();
        let popup_runtime = app.popup_runtime().unwrap();
        let popup_pid = popup_runtime.child_pid().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let notified = app.render_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if popup_runtime.visible_text().contains("OWNED-POPUP") {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
        let popup = render_text_surface(
            &app,
            target,
            area,
            true,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
        )
        .unwrap()
        .popup
        .unwrap();
        assert!(popup
            .frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>()
            .contains("OWNED-POPUP"));
        assert_eq!(popup.pixel_width, u32::from(popup.frame.width) * 8);
        assert_eq!(popup.pixel_height, u32::from(popup.frame.height) * 16);
        let mut chrome = crate::client::endpoint::chrome::ClientChrome::new(
            crate::client::endpoint::chrome::ChromeSettings::from_config(
                &crate::config::Config::default(),
                crate::app::state::Palette::catppuccin(),
                None,
            ),
        );
        let mut view = chrome.compute_view(
            &crate::client::endpoint::shell::ClientShellState::new(),
            area.width,
            area.height,
        );
        view.layout.pane_surface = area;
        view.surface = Some(crate::protocol::endpoint_wire::PaneSurfaceFrame {
            boot_id: "owned-popup-composition".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: surface.frame.clone(),
            panes: surface.panes.clone(),
            splits: surface.splits.clone(),
            popup: Some(popup.clone()),
            graphics: Default::default(),
        });
        let composed = chrome.render(&view);
        let geometry = crate::popup_size::resolve_popup_geometry(None, None, area).unwrap();
        assert_eq!(
            composed.cells.len(),
            usize::from(area.width) * usize::from(area.height)
        );
        for y in 0..geometry.inner.height {
            for x in 0..geometry.inner.width {
                let actual = &composed.cells[usize::from(y + geometry.inner.y)
                    * usize::from(area.width)
                    + usize::from(x + geometry.inner.x)];
                let expected = &popup.frame.cells
                    [usize::from(y) * usize::from(geometry.inner.width) + usize::from(x)];
                assert_eq!(
                    (
                        &actual.symbol,
                        actual.fg,
                        actual.bg,
                        actual.modifier,
                        actual.skip
                    ),
                    (
                        &expected.symbol,
                        expected.fg,
                        expected.bg,
                        expected.modifier,
                        expected.skip
                    )
                );
            }
        }
        assert_eq!(
            composed.cursor,
            popup
                .frame
                .cursor
                .as_ref()
                .map(|cursor| crate::protocol::CursorState {
                    x: cursor.x + geometry.inner.x,
                    y: cursor.y + geometry.inner.y,
                    visible: cursor.visible,
                    shape: cursor.shape,
                })
        );
        let owner = app.state.workspaces[0].id.clone();
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("background"));
        let background = Some(TabSurfaceTarget {
            workspace_index: 1,
            tab_index: 0,
            pane_focus: None,
        });
        assert!(
            render_text_surface(&app, background, area, false, HostCellSize::default())
                .unwrap()
                .popup
                .is_none()
        );
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.popup_panes[0].workspace_id, owner);
        assert!(app.close_popup_pane());
        assert!(!crate::platform::process_exists(popup_pid));
        // App/registry drop owns only the two tab PTYs created by this test.
    }
}
