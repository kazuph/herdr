use crate::protocol::endpoint_projection::SnapshotJson;
#[cfg(test)]
use crate::protocol::endpoint_wire::ClientShellSnapshot;
use crate::protocol::endpoint_wire::{ClientSurfaceSize, PaneSurfaceFrame};

/// Cached metadata may stay visible offline. Its presence never grants input
/// authority; only a live generation with an exact replacement pair does.
#[derive(Default)]
pub(crate) struct EndpointCache {
    generation: Option<u64>,
    live: bool,
    snapshot: Option<SnapshotJson>,
    surface: Option<PaneSurfaceFrame>,
    boot: Option<String>,
}

impl EndpointCache {
    pub(crate) fn begin_connection(&mut self, generation: u64) -> bool {
        if self.generation.is_some_and(|current| generation <= current) {
            return false;
        }
        self.generation = Some(generation);
        self.live = true;
        // Retain the old snapshot only for dimmed listing, without authority.
        self.boot = None;
        self.surface = None;
        true
    }

    pub(crate) fn disconnect(&mut self, generation: u64) -> bool {
        if self.generation != Some(generation) {
            return false;
        }
        self.live = false;
        self.surface = None;
        true
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.live && self.generation == Some(generation)
    }

    pub(crate) fn snapshot(&self) -> Option<&crate::protocol::endpoint_wire::ClientShellSnapshot> {
        self.snapshot
            .as_ref()
            .map(|projection| &projection.snapshot)
    }

    pub(crate) fn live_snapshot(
        &self,
        generation: u64,
    ) -> Option<&crate::protocol::endpoint_wire::ClientShellSnapshot> {
        let snapshot = self.snapshot.as_ref()?;
        (self.accepts(generation) && self.boot.as_deref() == Some(snapshot.boot_id.as_str()))
            .then_some(&snapshot.snapshot)
    }

    pub(crate) fn notification(
        &self,
        generation: u64,
    ) -> Option<&crate::protocol::endpoint_wire::SemanticNotification> {
        self.live_snapshot(generation)?;
        self.snapshot.as_ref()?.notification.as_ref()
    }

    pub(crate) fn workspace_facts(
        &self,
        generation: u64,
        workspace_id: &str,
    ) -> Option<&crate::protocol::endpoint_projection::WorkspaceFacts> {
        self.live_snapshot(generation)?;
        self.snapshot.as_ref()?.workspace_facts.get(workspace_id)
    }

    /// Last accepted facts are for stale presentation only, never command authorization.
    pub(crate) fn displayed_workspace_facts(
        &self,
        workspace_id: &str,
    ) -> Option<&crate::protocol::endpoint_projection::WorkspaceFacts> {
        self.snapshot.as_ref()?.workspace_facts.get(workspace_id)
    }

    pub(crate) fn displayed_pane_facts(
        &self,
        pane_id: &str,
    ) -> Option<&crate::protocol::endpoint_projection::PaneFacts> {
        self.snapshot.as_ref()?.pane_facts.get(pane_id)
    }

    pub(crate) fn replace_snapshot(
        &mut self,
        generation: u64,
        snapshot: impl Into<SnapshotJson>,
    ) -> bool {
        let snapshot = snapshot.into();
        if !self.accepts(generation) {
            return false;
        }
        if let Some(boot) = &self.boot {
            if *boot != snapshot.boot_id {
                return false;
            }
            if self.snapshot.as_ref().is_some_and(|current| {
                snapshot.revision < current.revision
                    || (snapshot.revision == current.revision && snapshot != *current)
            }) {
                return false;
            }
        } else {
            self.boot = Some(snapshot.boot_id.clone());
            if self
                .surface
                .as_ref()
                .is_some_and(|surface| surface.boot_id != snapshot.boot_id)
            {
                self.surface = None;
            }
        }
        self.snapshot = Some(snapshot);
        true
    }

    pub(crate) fn replace_surface(
        &mut self,
        generation: u64,
        mut surface: PaneSurfaceFrame,
    ) -> bool {
        if !self.accepts(generation)
            || self
                .boot
                .as_ref()
                .is_some_and(|boot| *boot != surface.boot_id)
            || self.surface.as_ref().is_some_and(|current| {
                surface.projection_revision < current.projection_revision
                    || surface.surface_revision <= current.surface_revision
            })
        {
            return false;
        }
        // Bounded uploads can arrive before activation permits a draw. Preserve received
        // pixels still owned by the next scene, as the upstream receive-time bank does.
        if let Some(previous) = self
            .surface
            .as_ref()
            .filter(|old| old.boot_id == surface.boot_id)
        {
            let desired = surface
                .graphics
                .placements
                .iter()
                .map(|placement| &placement.asset)
                .chain(surface.graphics.retained_assets.iter())
                .collect::<std::collections::HashSet<_>>();
            let received = surface
                .graphics
                .assets
                .iter()
                .map(|asset| asset.key.clone())
                .collect::<std::collections::HashSet<_>>();
            surface.graphics.assets.extend(
                previous
                    .graphics
                    .assets
                    .iter()
                    .filter(|asset| {
                        desired.contains(&asset.key)
                            && !received.contains(&asset.key)
                            && asset.data.len() as u64 == asset.key.data_len
                    })
                    .cloned(),
            );
        }
        self.surface = Some(surface);
        true
    }

    pub(crate) fn received_surface(&self, generation: u64) -> Option<&PaneSurfaceFrame> {
        if !self.accepts(generation) {
            return None;
        }
        self.surface.as_ref()
    }

    pub(crate) fn coherent_surface(
        &self,
        generation: u64,
        geometry: ClientSurfaceSize,
    ) -> Option<&PaneSurfaceFrame> {
        if !self.accepts(generation) {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        self.surface.as_ref().filter(|surface| {
            self.boot.as_deref() == Some(snapshot.boot_id.as_str())
                && surface.boot_id == snapshot.boot_id
                && surface.projection_revision == snapshot.revision
                && surface.frame.width == geometry.cols
                && surface.frame.height == geometry.rows
                && surface.frame.cells.len()
                    == usize::from(geometry.cols) * usize::from(geometry.rows)
        })
    }

    pub(crate) fn apply_patch(
        &mut self,
        generation: u64,
        patch: crate::protocol::endpoint_wire::PaneSurfacePatch,
    ) -> Option<PaneSurfaceFrame> {
        if !self.accepts(generation) {
            return None;
        }
        let current = self.surface.as_ref()?;
        if patch.boot_id != current.boot_id
            || patch.projection_revision != current.projection_revision
            || patch.base_surface_revision != current.surface_revision
            || patch.surface_revision != current.surface_revision.saturating_add(1)
            || current.popup.is_some()
            || !current.graphics.placements.is_empty()
            || !current.graphics.retained_assets.is_empty()
        {
            return None;
        }
        // Validate the complete update before touching the connection's retained baseline.
        for updated in &patch.panes {
            let existing = current
                .panes
                .iter()
                .find(|pane| pane.pane_id == updated.pane_id)?;
            if existing.rect != updated.rect
                || existing.inner_rect != updated.inner_rect
                || existing.focused != updated.focused
                || existing.pixel_width != updated.pixel_width
                || existing.pixel_height != updated.pixel_height
            {
                return None;
            }
        }
        for row in &patch.rows {
            let width = u16::try_from(row.cells.len()).ok()?;
            let end_x = row.x.checked_add(width)?;
            if width == 0
                || end_x > current.frame.width
                || row.y >= current.frame.height
                || !patch.panes.iter().any(|pane| {
                    let terminal = row.x >= pane.inner_rect.x
                        && row.y >= pane.inner_rect.y
                        && row.y < pane.inner_rect.y.saturating_add(pane.inner_rect.height)
                        && end_x <= pane.inner_rect.x.saturating_add(pane.inner_rect.width);
                    let scrollbar = pane
                        .scrollbar_rect
                        .or_else(|| {
                            current
                                .panes
                                .iter()
                                .find(|old| old.pane_id == pane.pane_id)
                                .and_then(|old| old.scrollbar_rect)
                        })
                        .is_some_and(|rect| {
                            row.x == rect.x
                                && row.y >= rect.y
                                && row.y < rect.y.saturating_add(rect.height)
                                && width == rect.width
                        });
                    terminal || scrollbar
                })
            {
                return None;
            }
            let end = usize::from(row.y) * usize::from(current.frame.width)
                + usize::from(row.x)
                + row.cells.len();
            if end > current.frame.cells.len() {
                return None;
            }
        }
        let mut next = current.clone();
        for row in patch.rows {
            let start = usize::from(row.y) * usize::from(next.frame.width) + usize::from(row.x);
            next.frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
        }
        for updated in patch.panes {
            let existing = next
                .panes
                .iter_mut()
                .find(|pane| pane.pane_id == updated.pane_id)?;
            *existing = updated;
        }
        next.frame.cursor = patch.cursor;
        next.surface_revision = patch.surface_revision;
        self.surface = Some(next.clone());
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint_wire::{FrameData, SurfaceGraphicsScene};

    fn snapshot(revision: u64) -> ClientShellSnapshot {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .expect("fixed upstream projection");
        snapshot.revision = revision;
        snapshot
    }

    fn geometry() -> ClientSurfaceSize {
        ClientSurfaceSize { cols: 80, rows: 24 }
    }

    fn surface(projection_revision: u64, surface_revision: u64) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: snapshot(0).boot_id,
            projection_revision,
            surface_revision,
            frame: FrameData::from_ratatui_buffer(
                &ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(
                    0,
                    0,
                    geometry().cols,
                    geometry().rows,
                )),
                None,
            ),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn activation_requires_an_exact_snapshot_surface_revision_pair() {
        for surface_first in [false, true] {
            let mut cache = EndpointCache::default();
            assert!(cache.begin_connection(1));
            if surface_first {
                assert!(cache.replace_surface(1, surface(7, 1)));
                assert!(cache.coherent_surface(1, geometry()).is_none());
                assert!(cache.replace_snapshot(1, snapshot(7)));
            } else {
                assert!(cache.replace_snapshot(1, snapshot(7)));
                assert!(cache.coherent_surface(1, geometry()).is_none());
                assert!(cache.replace_surface(1, surface(7, 1)));
            }
            assert!(cache.coherent_surface(1, geometry()).is_some());
            assert!(cache.replace_snapshot(1, snapshot(8)));
            assert!(cache.coherent_surface(1, geometry()).is_none());
            assert!(cache.replace_surface(1, surface(8, 2)));
            assert!(cache.coherent_surface(1, geometry()).is_some());
            assert!(cache
                .coherent_surface(1, ClientSurfaceSize { cols: 81, rows: 24 })
                .is_none());
        }
    }

    #[test]
    fn reconnect_same_endpoint_accepts_new_generation_surface_revision() {
        let mut cache = EndpointCache::default();
        assert!(cache.begin_connection(1));
        assert!(cache.replace_snapshot(1, snapshot(99)));
        assert!(cache.replace_surface(1, surface(99, 100)));
        assert!(cache.disconnect(1));
        assert!(cache.coherent_surface(1, geometry()).is_none());
        assert!(cache.snapshot().is_some());
        assert!(cache.begin_connection(2));
        assert!(cache.coherent_surface(2, geometry()).is_none());
        assert!(cache.replace_snapshot(2, snapshot(1)));
        assert!(cache.replace_surface(2, surface(1, 1)));
        assert!(cache.coherent_surface(2, geometry()).is_some());
        assert!(!cache.disconnect(1));
        assert!(!cache.replace_surface(1, surface(99, 101)));
        assert!(!cache.replace_snapshot(1, snapshot(100)));
        assert!(!cache.begin_connection(1));
        assert!(cache.coherent_surface(2, geometry()).is_some());
    }

    #[test]
    fn wrong_boot_and_regressing_revisions_cannot_change_a_committed_surface() {
        let mut cache = EndpointCache::default();
        cache.begin_connection(4);
        cache.replace_snapshot(4, snapshot(6));
        cache.replace_surface(4, surface(6, 9));
        let mut stale = surface(6, 10);
        stale.boot_id = "previous-boot".into();
        assert!(!cache.replace_surface(4, stale));
        assert!(!cache.replace_snapshot(4, snapshot(5)));
        assert!(!cache.replace_surface(4, surface(6, 8)));
        assert_eq!(
            cache
                .coherent_surface(4, geometry())
                .expect("current")
                .surface_revision,
            9
        );
    }

    #[test]
    fn received_graphics_batches_survive_surface_replacement_before_presentation() {
        use crate::protocol::endpoint_wire::{
            SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
            SurfaceGraphicsSource, SurfaceGraphicsTarget,
        };
        let asset = |id, data: Vec<u8>| SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey {
                source: SurfaceGraphicsSource::Terminal {
                    target: SurfaceGraphicsTarget::Pane {
                        pane_id: "opaque-pane".into(),
                    },
                    image_id: id,
                },
                image_width: 1,
                image_height: 1,
                format: SurfaceGraphicsFormat::Rgba,
                data_len: data.len() as u64,
                data_fingerprint: u64::from(id),
            },
            data,
        };
        let first = asset(1, vec![255, 0, 0, 255]);
        let second = asset(2, vec![0, 255, 0, 255]);
        let mut cache = EndpointCache::default();
        assert!(cache.begin_connection(1));
        assert!(cache.replace_snapshot(1, snapshot(7)));
        let mut batch = surface(7, 1);
        batch.graphics.retained_assets = vec![first.key.clone(), second.key.clone()];
        batch.graphics.assets = vec![first.clone()];
        assert!(cache.replace_surface(1, batch.clone()));
        batch.surface_revision = 2;
        batch.graphics.assets = vec![second.clone()];
        assert!(cache.replace_surface(1, batch.clone()));
        let pixels = &cache
            .coherent_surface(1, geometry())
            .unwrap()
            .graphics
            .assets;
        assert_eq!(pixels.len(), 2);
        assert!(pixels.contains(&first));
        assert!(pixels.contains(&second));
        assert_eq!(
            cache.received_surface(1),
            cache.coherent_surface(1, geometry())
        );
        assert!(cache.received_surface(2).is_none());
        batch.surface_revision = 3;
        batch.graphics.assets.clear();
        assert!(cache.replace_surface(1, batch.clone()));
        assert_eq!(
            cache
                .coherent_surface(1, geometry())
                .unwrap()
                .graphics
                .assets
                .len(),
            2
        );
        batch.surface_revision = 4;
        batch.graphics.retained_assets = vec![second.key.clone()];
        assert!(cache.replace_surface(1, batch.clone()));
        assert_eq!(
            cache
                .coherent_surface(1, geometry())
                .unwrap()
                .graphics
                .assets,
            vec![second]
        );
        let mut wrong_boot = batch.clone();
        wrong_boot.boot_id = "other-boot".into();
        wrong_boot.surface_revision = 5;
        wrong_boot.graphics.assets = vec![first];
        assert!(!cache.replace_surface(1, wrong_boot));
        assert!(!cache.replace_surface(1, batch.clone()));
        assert!(cache.begin_connection(2));
        assert!(cache.replace_snapshot(2, snapshot(7)));
        assert!(cache.replace_surface(2, batch));
        assert!(cache
            .coherent_surface(2, geometry())
            .unwrap()
            .graphics
            .assets
            .is_empty());
    }

    #[test]
    fn same_resource_id_is_distinct_on_each_endpoint() {
        use crate::client::endpoint::{ClientEndpointId, ResourceKey};
        let keys = std::collections::HashSet::from([
            ResourceKey {
                endpoint: ClientEndpointId::Local,
                id: "p1".into(),
            },
            ResourceKey {
                endpoint: ClientEndpointId::Ssh("m1".into()),
                id: "p1".into(),
            },
            ResourceKey {
                endpoint: ClientEndpointId::Ssh("m2".into()),
                id: "p1".into(),
            },
        ]);
        assert_eq!(keys.len(), 3);
    }
}
