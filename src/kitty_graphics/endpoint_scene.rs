//! Stable scenes borrow the fork's image collector without changing public focus.
use super::*;
use crate::protocol::endpoint_wire::{
    ClientShellPopupSurface, PaneSurfacePane, SurfaceGraphicsAsset, SurfaceGraphicsAssetKey,
    SurfaceGraphicsFormat, SurfaceGraphicsPlacement, SurfaceGraphicsScene, SurfaceGraphicsSource,
    SurfaceGraphicsTarget,
};

// Fixed upstream 5da0a01 kitty_graphics/surface.rs.
const MAX_SURFACE_GRAPHICS_PLACEMENTS: usize = 4_096;
// Fixed 5da0a01 surface.rs retained-image bounds.
const MAX_OFFSCREEN_IMAGES: usize = 16;
const MAX_OFFSCREEN_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default, Clone)]
pub(crate) struct DeliveryCache {
    assets: HashSet<SurfaceGraphicsAssetKey>,
    offscreen: Vec<SurfaceGraphicsAssetKey>,
    pending: bool,
}

impl DeliveryCache {
    pub(crate) fn has_pending(&self) -> bool {
        self.pending
    }
}

pub(crate) fn collect(
    app: &crate::app::App,
    target: Option<crate::ui::tab_surface::TabSurfaceTarget>,
    panes: &[PaneSurfacePane],
    popup: Option<&ClientShellPopupSurface>,
    cell_size: HostCellSize,
    delivered: &DeliveryCache,
) -> (SurfaceGraphicsScene, DeliveryCache) {
    let Some(target) = target else {
        return (SurfaceGraphicsScene::default(), DeliveryCache::default());
    };
    if !cell_size.is_known() {
        return (SurfaceGraphicsScene::default(), DeliveryCache::default());
    }
    let workspace = &app.state.workspaces[target.workspace_index];
    let rect =
        |r: crate::protocol::endpoint_wire::SurfaceRect| Rect::new(r.x, r.y, r.width, r.height);
    let infos = panes
        .iter()
        .filter_map(|pane| {
            let (ws, id) = app.parse_pane_id(&pane.pane_id)?;
            (ws == target.workspace_index).then_some(PaneInfo {
                id,
                rect: rect(pane.rect),
                inner_rect: rect(pane.inner_rect),
                scrollbar_rect: pane.scrollbar_rect.map(rect),
                borders: ratatui::widgets::Borders::NONE,
                is_focused: pane.focused,
            })
        })
        .collect::<Vec<_>>();
    let mut placements = collect_visible_placements_in_workspace(
        &app.state,
        &app.pane_graphics,
        &app.terminal_runtimes,
        target.workspace_index,
        crate::ui::TabSurfaceView {
            pane_infos: &infos,
            split_borders: &[],
        },
        cell_size,
        &HashMap::new(),
    );
    let mut targets = infos
        .iter()
        .filter_map(|pane| {
            app.public_pane_id(target.workspace_index, pane.id)
                .map(|id| (pane.id, SurfaceGraphicsTarget::Pane { pane_id: id }))
        })
        .collect::<HashMap<_, _>>();
    if let (Some(surface), Some(owned)) = (popup, app.state.popup_pane_for_workspace(&workspace.id))
    {
        if surface.terminal_id == owned.terminal_id.to_string() {
            if let Some(runtime) = app.terminal_runtimes.get(&owned.terminal_id) {
                targets.insert(
                    owned.pane_id,
                    SurfaceGraphicsTarget::Popup {
                        terminal_id: surface.terminal_id.clone(),
                    },
                );
                for placement in runtime.kitty_image_placements_with_data_filter(|_| true) {
                    placements.push(HostPlacement {
                        pane_id: owned.pane_id,
                        host_image_id: None,
                        area: Rect::new(0, 0, surface.frame.width, surface.frame.height),
                        cell_size,
                        source_key: HostSourceKey::Terminal {
                            pane_id: owned.pane_id,
                            image_id: placement.image_id,
                        },
                        placement,
                        scrollback_offset: runtime
                            .scroll_metrics()
                            .map(|m| m.offset_from_bottom as u32)
                            .unwrap_or(0),
                    });
                }
            }
        }
    }
    let mut scene = SurfaceGraphicsScene::default();
    let mut data = HashMap::<SurfaceGraphicsAssetKey, Vec<u8>>::new();
    for mut placement in placements {
        if scene.placements.len() == MAX_SURFACE_GRAPHICS_PLACEMENTS {
            break;
        }
        let Some((clipped, _)) = clipped_placement(&placement) else {
            continue;
        };
        let Some(target) = targets.get(&placement.pane_id) else {
            continue;
        };
        let source = match &placement.source_key {
            HostSourceKey::ClientSurface { .. } => continue,
            HostSourceKey::Terminal { image_id, .. } => SurfaceGraphicsSource::Terminal {
                target: target.clone(),
                image_id: *image_id,
            },
            HostSourceKey::PaneLayer { pane_id, layer_id } => {
                let SurfaceGraphicsTarget::Pane { pane_id: public } = target else {
                    continue;
                };
                if placement.placement.data.is_empty() {
                    if let Some(lease) = app
                        .pane_graphics
                        .slots
                        .get(&(*pane_id, layer_id.clone()))
                        .and_then(|slot| slot.layer.as_ref())
                        .and_then(crate::app::pane_graphics::Layer::direct_lease)
                    {
                        let Ok(bytes) = lease.copy_rgba() else {
                            continue;
                        };
                        placement.placement.data = bytes;
                    }
                }
                SurfaceGraphicsSource::PaneLayer {
                    pane_id: public.clone(),
                    layer_id: layer_id.clone(),
                }
            }
        };
        let image = &placement.placement;
        let key = SurfaceGraphicsAssetKey {
            source,
            image_width: image.image_width,
            image_height: image.image_height,
            format: match image.format {
                KittyImageFormat::Rgb => SurfaceGraphicsFormat::Rgb,
                KittyImageFormat::Rgba => SurfaceGraphicsFormat::Rgba,
                KittyImageFormat::Png => SurfaceGraphicsFormat::Png,
            },
            data_len: image.data_len as u64,
            data_fingerprint: image.data_fingerprint,
        };
        if !delivered.assets.contains(&key) && image.data.is_empty() {
            continue;
        }
        if !delivered.assets.contains(&key) {
            data.entry(key.clone())
                .or_insert_with(|| std::mem::take(&mut placement.placement.data));
        }
        scene.placements.push(SurfaceGraphicsPlacement {
            asset: key,
            logical_placement_id: placement.placement.placement_id,
            x: clipped.x,
            y: clipped.y,
            cols: clipped.cols,
            rows: clipped.rows,
            source_x: clipped.source_x,
            source_y: clipped.source_y,
            source_width: clipped.source_width,
            source_height: clipped.source_height,
            x_offset: clipped.x_offset,
            y_offset: clipped.y_offset,
            z: placement.placement.z,
            scrollback_offset: placement.scrollback_offset,
        });
    }
    scene.placements.sort_by_key(|p| {
        (
            format!("{:?}", p.asset.source),
            p.logical_placement_id,
            p.y,
            p.x,
        )
    });
    let visible = scene
        .placements
        .iter()
        .map(|p| p.asset.clone())
        .collect::<HashSet<_>>();
    let public_panes = infos
        .iter()
        .filter_map(|pane| {
            app.public_pane_id(target.workspace_index, pane.id)
                .map(|id| (id, pane.id))
        })
        .collect::<HashMap<_, _>>();
    scene.retained_assets = offscreen_assets(
        app,
        Some(target.workspace_index),
        &public_panes,
        delivered,
        &visible,
    );
    let mut next = DeliveryCache {
        assets: delivered
            .assets
            .intersection(&visible)
            .chain(&scene.retained_assets)
            .cloned()
            .collect(),
        offscreen: scene.retained_assets.clone(),
        pending: false,
    };
    deliver_assets(&mut scene, &mut next, data);
    (scene, next)
}

fn deliver_assets(
    scene: &mut SurfaceGraphicsScene,
    next: &mut DeliveryCache,
    data: HashMap<SurfaceGraphicsAssetKey, Vec<u8>>,
) {
    let mut available = data.into_iter().collect::<Vec<_>>();
    available.sort_by_key(|(key, _)| format!("{:?}", key.source));
    let mut payload_bytes = 0usize;
    for (key, data) in available {
        if next.assets.contains(&key) {
            continue;
        }
        let encoded_size = super::image_transfer_estimated_size(
            usize::try_from(key.data_len).unwrap_or(usize::MAX),
        );
        if encoded_size > super::HEADLESS_GRAPHICS_TRANSACTION_BUDGET {
            continue;
        }
        if payload_bytes.saturating_add(encoded_size) > super::HEADLESS_GRAPHICS_TRANSACTION_BUDGET
        {
            next.pending = true;
            continue;
        }
        payload_bytes = payload_bytes.saturating_add(encoded_size);
        scene.assets.push(SurfaceGraphicsAsset {
            key: key.clone(),
            data,
        });
        next.assets.insert(key);
    }
    scene
        .assets
        .sort_by_key(|asset| format!("{:?}", asset.key.source));
}

fn offscreen_assets(
    app: &crate::app::App,
    workspace_index: Option<usize>,
    public_panes: &HashMap<String, PaneId>,
    delivered: &DeliveryCache,
    visible: &HashSet<SurfaceGraphicsAssetKey>,
) -> Vec<SurfaceGraphicsAssetKey> {
    let Some(workspace_index) = workspace_index else {
        return Vec::new();
    };
    fn pane_image(key: &SurfaceGraphicsAssetKey) -> Option<(&str, u32)> {
        match &key.source {
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane { pane_id },
                image_id,
            } => Some((pane_id.as_str(), *image_id)),
            _ => None,
        }
    }
    let mut newly_hidden = delivered
        .assets
        .iter()
        .filter(|key| !visible.contains(*key) && !delivered.offscreen.contains(key))
        .collect::<Vec<_>>();
    if newly_hidden.is_empty() && delivered.offscreen.is_empty() {
        return Vec::new();
    }
    newly_hidden.sort_by_key(|key| pane_image(key));
    let visible_sources = visible
        .iter()
        .map(|key| &key.source)
        .collect::<HashSet<_>>();
    let mut candidates_by_pane = HashMap::<PaneId, Vec<&SurfaceGraphicsAssetKey>>::new();
    let candidates = newly_hidden
        .into_iter()
        .chain(&delivered.offscreen)
        .filter(|key| {
            !visible_sources.contains(&key.source) && key.data_len <= MAX_OFFSCREEN_IMAGE_BYTES
        })
        .filter_map(|key| {
            let (public_id, _) = pane_image(key)?;
            let pane_id = *public_panes.get(public_id)?;
            candidates_by_pane.entry(pane_id).or_default().push(key);
            Some(key)
        })
        .collect::<Vec<_>>();

    let mut live = HashSet::new();
    for (pane_id, keys) in candidates_by_pane {
        let Some(runtime) = app.state.runtime_for_pane_in_workspace(
            &app.terminal_runtimes,
            workspace_index,
            pane_id,
        ) else {
            continue;
        };
        let image_ids = keys
            .iter()
            .filter_map(|key| pane_image(key).map(|(_, image_id)| image_id))
            .collect::<Vec<_>>();
        for (key, fingerprint) in keys
            .into_iter()
            .zip(runtime.kitty_image_fingerprints(&image_ids))
        {
            if fingerprint == Some(key.data_fingerprint) {
                live.insert(key);
            }
        }
    }

    let mut bytes = 0u64;
    let mut offscreen = Vec::new();
    for key in candidates {
        if offscreen.len() == MAX_OFFSCREEN_IMAGES {
            break;
        }
        if !live.contains(key) || bytes.saturating_add(key.data_len) > MAX_OFFSCREEN_IMAGE_BYTES {
            continue;
        }
        bytes += key.data_len;
        offscreen.push(key.clone());
    }
    offscreen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_delivery_does_not_mark_deferred_pixels_sent_and_continues_in_order() {
        let length = crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES;
        let key = |id| SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "opaque".into(),
                },
                image_id: id,
            },
            image_width: 2048,
            image_height: 2048,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: length as u64,
            data_fingerprint: u64::from(id),
        };
        let first = key(1);
        let second = key(2);
        assert!(
            super::super::image_transfer_estimated_size(length)
                < HEADLESS_GRAPHICS_TRANSACTION_BUDGET
        );
        assert!(
            super::super::image_transfer_estimated_size(length).saturating_mul(2)
                > HEADLESS_GRAPHICS_TRANSACTION_BUDGET
        );
        let mut scene = SurfaceGraphicsScene::default();
        let mut delivery = DeliveryCache::default();
        deliver_assets(
            &mut scene,
            &mut delivery,
            HashMap::from([
                (first.clone(), vec![255; length]),
                (second.clone(), vec![0; length]),
            ]),
        );
        assert_eq!(scene.assets.len(), 1);
        assert_eq!(scene.assets[0].key, first);
        assert!(delivery.assets.contains(&first));
        assert!(!delivery.assets.contains(&second));
        assert!(delivery.has_pending());
        let mut next_scene = SurfaceGraphicsScene::default();
        let mut next = DeliveryCache {
            assets: delivery.assets.clone(),
            offscreen: vec![],
            pending: false,
        };
        deliver_assets(
            &mut next_scene,
            &mut next,
            HashMap::from([(second.clone(), vec![0; length])]),
        );
        assert_eq!(next_scene.assets.len(), 1);
        assert_eq!(next_scene.assets[0].key, second);
        assert!(next.assets.contains(&first));
        assert!(next.assets.contains(&second));
        assert!(!next.has_pending());
    }
}
