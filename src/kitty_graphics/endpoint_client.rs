//! Client-owned stable scenes reuse the fork encoder; no remote path is opened here.
use super::*;
use crate::protocol::endpoint_wire::{
    SurfaceGraphicsAssetKey, SurfaceGraphicsFormat, SurfaceGraphicsPlacement, SurfaceGraphicsScene,
    SurfaceGraphicsSource, SurfaceGraphicsTarget,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    Main,
    Popup,
    Hidden,
}

#[derive(Debug, Default)]
pub(crate) struct Occlusion {
    regions: Vec<Rect>,
    popup_start: usize,
}

impl Occlusion {
    pub(crate) fn cover(&mut self, rect: Rect) {
        if !rect.is_empty() {
            self.regions.push(rect);
        }
    }

    pub(crate) fn start_popup(&mut self, rect: Rect) {
        self.cover(rect);
        self.popup_start = self.regions.len();
    }

    #[cfg(test)]
    fn covers(&self, placement: &SurfaceGraphicsPlacement, origin: (u16, u16)) -> bool {
        self.visible_pieces(placement, origin, 1) != Some(vec![GridPiece::whole(placement)])
    }

    /// Visible pieces for each placement in a frame. When cropping would exceed
    /// the frame's piece budget, touched images are hidden whole instead.
    fn frame_pieces(
        &self,
        placements: &[(&SurfaceGraphicsPlacement, (u16, u16))],
    ) -> Vec<Vec<GridPiece>> {
        let mut extra_budget = MAX_EXTRA_CROP_PIECES;
        let mut pieces = Vec::with_capacity(placements.len());
        for &(placement, origin) in placements {
            let Some(found) = self.visible_pieces(placement, origin, extra_budget + 1) else {
                return placements
                    .iter()
                    .map(|&(placement, origin)| {
                        let whole = vec![GridPiece::whole(placement)];
                        if self.visible_pieces(placement, origin, 1).as_ref() == Some(&whole) {
                            whole
                        } else {
                            Vec::new()
                        }
                    })
                    .collect();
            };
            extra_budget -= found.len().saturating_sub(1);
            pieces.push(found);
        }
        pieces
    }

    /// Splits a placement's cell grid into the rectangles no overlay touches,
    /// or returns `None` when that would take more than `limit` pieces.
    fn visible_pieces(
        &self,
        placement: &SurfaceGraphicsPlacement,
        origin: (u16, u16),
        limit: usize,
    ) -> Option<Vec<GridPiece>> {
        let regions = if matches!(
            placement.asset.source,
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            }
        ) {
            &self.regions[self.popup_start..]
        } else {
            &self.regions
        };
        let mut pieces = vec![GridPiece::whole(placement)];
        // Pixel offsets shrink an image inside its cells, so cell overlap is exact.
        let x = u32::from(origin.0.saturating_add(placement.x));
        let y = u32::from(origin.1.saturating_add(placement.y));
        let covered_span = |start: u16, end: u16, image_start: u32, len: u32| {
            let first = u32::from(start).saturating_sub(image_start);
            let last = u32::from(end).saturating_sub(image_start).min(len);
            (first < last).then_some((first, last))
        };
        for rect in regions {
            let Some((col_start, col_end)) = covered_span(rect.x, rect.right(), x, placement.cols)
            else {
                continue;
            };
            let Some((row_start, row_end)) = covered_span(rect.y, rect.bottom(), y, placement.rows)
            else {
                continue;
            };
            pieces = pieces
                .into_iter()
                .flat_map(|piece| piece.subtract(col_start, col_end, row_start, row_end))
                .collect();
            if pieces.is_empty() {
                return Some(pieces);
            }
            if pieces.len() > limit {
                return None;
            }
        }
        Some(pieces)
    }
}

/// Extra placements one frame may add by cropping around overlays. Beyond it,
/// the frame hides touched images whole so the quadratic host encoder stays cheap.
const MAX_EXTRA_CROP_PIECES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridPiece {
    col: u32,
    row: u32,
    cols: u32,
    rows: u32,
}

impl GridPiece {
    fn whole(placement: &SurfaceGraphicsPlacement) -> Self {
        Self {
            col: 0,
            row: 0,
            cols: placement.cols,
            rows: placement.rows,
        }
    }

    fn subtract(self, col_start: u32, col_end: u32, row_start: u32, row_end: u32) -> Vec<Self> {
        let right = self.col + self.cols;
        let bottom = self.row + self.rows;
        if col_end <= self.col || col_start >= right || row_end <= self.row || row_start >= bottom {
            return vec![self];
        }
        let middle_top = row_start.max(self.row);
        let middle_bottom = row_end.min(bottom);
        [
            (self.col, self.row, right, middle_top),
            (self.col, middle_bottom, right, bottom),
            (self.col, middle_top, col_start.max(self.col), middle_bottom),
            (col_end.min(right), middle_top, right, middle_bottom),
        ]
        .into_iter()
        .filter(|(left, top, right, bottom)| left < right && top < bottom)
        .map(|(left, top, right, bottom)| Self {
            col: left,
            row: top,
            cols: right - left,
            rows: bottom - top,
        })
        .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ClientState {
    scope: String,
    scene: SurfaceGraphicsScene,
    display_scene: SurfaceGraphicsScene,
    native_image_ids: HashMap<SurfaceGraphicsAssetKey, u32>,
    assets: HashMap<SurfaceGraphicsAssetKey, Arc<[u8]>>,
    trusted_direct: HashMap<SurfaceGraphicsAssetKey, u32>,
    forced_delete_images: Vec<u32>,
    host: HostGraphicsCache,
    reset_pending: bool,
    stale_images: Vec<u32>,
}
impl ClientState {
    fn image_id(&self, key: &SurfaceGraphicsAssetKey) -> u32 {
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            return self
                .native_image_ids
                .get(key)
                .copied()
                .unwrap_or_else(|| native_host_image_id(&self.scope, key));
        }
        host_image_id(&self.scope, key)
    }
    pub(crate) fn set_scope(&mut self, scope: &str) {
        if self.scope == scope {
            return;
        }
        self.scope = scope.to_owned();
        self.scene = SurfaceGraphicsScene::default();
        self.display_scene = SurfaceGraphicsScene::default();
        self.native_image_ids.clear();
        self.assets.clear();
        self.trusted_direct.clear();
        self.stale_images.clear();
        self.forced_delete_images.clear();
        self.reset_pending = true;
    }
    #[cfg(unix)]
    pub(crate) fn accepts_direct_asset(
        &self,
        key: &SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            return !self.stale_images.contains(&image_id)
                && !self.forced_delete_images.contains(&image_id)
                && (image_id == native_host_image_id(&self.scope, key)
                    || image_id == (native_host_image_id(&self.scope, key) ^ NATIVE_SLOT_BIT))
                && !self
                    .display_scene
                    .placements
                    .iter()
                    .map(|p| &p.asset)
                    .chain(self.display_scene.retained_assets.iter())
                    .any(|visible| {
                        self.image_id(visible) == image_id
                            && self.host.images.get(&image_id)
                                == Some(&image_signature_from_asset(visible))
                    })
                && (self
                    .scene
                    .placements
                    .iter()
                    .any(|placement| &placement.asset == key)
                    || self.scene.retained_assets.contains(key));
        }
        image_id == host_image_id(&self.scope, key)
    }
    #[cfg(unix)]
    pub(crate) fn trust_direct_asset(
        &mut self,
        key: &SurfaceGraphicsAssetKey,
        image_id: u32,
    ) -> bool {
        if !self.accepts_direct_asset(key, image_id) {
            return false;
        }
        if matches!(key.source, SurfaceGraphicsSource::Terminal { .. }) {
            self.native_image_ids.insert(key.clone(), image_id);
        }
        self.host
            .images
            .insert(image_id, image_signature_from_asset(key));
        self.refresh_display_scene();
        if self
            .scene
            .placements
            .iter()
            .all(|placement| &placement.asset != key)
            && !self.scene.retained_assets.contains(key)
        {
            self.trusted_direct.insert(key.clone(), image_id);
        }
        true
    }
    #[cfg(unix)]
    pub(crate) fn retire_direct_image(&mut self, image_id: u32) {
        self.trusted_direct
            .retain(|_, trusted| *trusted != image_id);
        self.forced_delete_images.push(image_id);
    }
    pub(crate) fn take_pending_cleanup(&mut self) -> Vec<u8> {
        let mut bytes = if self.reset_pending {
            self.reset_pending = false;
            self.stale_images.clear();
            self.host.clear_bytes()
        } else {
            Vec::new()
        };
        self.forced_delete_images.sort_unstable();
        self.forced_delete_images.dedup();
        for image_id in self.forced_delete_images.drain(..) {
            self.host.images.remove(&image_id);
            self.host.placements.retain(|(id, _), _| *id != image_id);
            self.host.sources.retain(|_, id| *id != image_id);
            self.host
                .replayed_placements
                .retain(|(id, _)| *id != image_id);
            super::encode_delete_image(&mut bytes, image_id);
        }
        bytes
    }

    pub(crate) fn set_scene(&mut self, mut scene: SurfaceGraphicsScene) {
        let desired = scene
            .placements
            .iter()
            .map(|placement| placement.asset.clone())
            .chain(scene.retained_assets.iter().cloned())
            .collect::<HashSet<_>>();
        let unclaimed = self
            .trusted_direct
            .iter()
            .filter_map(|(key, image_id)| (!desired.contains(key)).then_some(*image_id))
            .collect::<Vec<_>>();
        self.stale_images.extend(unclaimed);
        self.trusted_direct.clear();
        let placed = scene
            .placements
            .iter()
            .map(|placement| placement.asset.clone())
            .collect::<HashSet<_>>();
        for asset in std::mem::take(&mut scene.assets) {
            if asset.data.len() as u64 == asset.key.data_len && placed.contains(&asset.key) {
                self.assets.insert(asset.key, Arc::from(asset.data));
            }
        }
        self.scene = scene;
        self.refresh_display_scene();
    }
    fn refresh_display_scene(&mut self) {
        let mut next = self.scene.clone();
        let previous_placements = self
            .display_scene
            .placements
            .iter()
            .map(|p| ((&p.asset.source, p.logical_placement_id, p.x, p.y), p))
            .collect::<HashMap<_, _>>();
        let previous_assets = self
            .display_scene
            .retained_assets
            .iter()
            .chain(self.display_scene.placements.iter().map(|p| &p.asset))
            .filter(|key| {
                self.host.images.get(&self.image_id(key)) == Some(&image_signature_from_asset(key))
            })
            .map(|key| (&key.source, key))
            .collect::<HashMap<_, _>>();
        for placement in &mut next.placements {
            let key = &placement.asset;
            if !matches!(key.source, SurfaceGraphicsSource::Terminal { .. })
                || self.assets.contains_key(key)
                || self.host.images.get(&self.image_id(key))
                    == Some(&image_signature_from_asset(key))
            {
                continue;
            }
            // A metadata-only replacement is not visible until its upload is ACKed.
            // Reuse only an identical placement, never stale resize/scroll geometry.
            if let Some(previous) = previous_placements
                .get(&(
                    &key.source,
                    placement.logical_placement_id,
                    placement.x,
                    placement.y,
                ))
                .copied()
                .filter(|previous| {
                    if self.host.images.get(&self.image_id(&previous.asset))
                        != Some(&image_signature_from_asset(&previous.asset))
                    {
                        return false;
                    }
                    let mut candidate = (*previous).clone();
                    candidate.asset = key.clone();
                    candidate == *placement
                })
            {
                placement.asset = previous.asset.clone();
            }
        }
        for key in &mut next.retained_assets {
            if matches!(key.source, SurfaceGraphicsSource::Terminal { .. })
                && !self.assets.contains_key(key)
                && self.host.images.get(&self.image_id(key))
                    != Some(&image_signature_from_asset(key))
            {
                if let Some(previous) = previous_assets.get(&key.source) {
                    *key = (*previous).clone();
                }
            }
        }
        let desired = next
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(next.retained_assets.iter())
            .collect::<HashSet<_>>();
        let stale = self
            .display_scene
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(self.display_scene.retained_assets.iter())
            .filter(|key| !desired.contains(key))
            .map(|key| self.image_id(key))
            .collect::<Vec<_>>();
        self.stale_images.extend(stale);
        let owned = self
            .scene
            .placements
            .iter()
            .map(|p| &p.asset)
            .chain(self.scene.retained_assets.iter())
            .chain(desired.iter().copied())
            .collect::<HashSet<_>>();
        self.assets.retain(|key, _| owned.contains(key));
        self.native_image_ids.retain(|key, _| owned.contains(key));
        self.display_scene = next;
    }

    pub(crate) fn encode(
        &mut self,
        visibility: Visibility,
        main_origin: (u16, u16),
        popup_origin: Option<(u16, u16)>,
        cell_size: HostCellSize,
        occlusion: &Occlusion,
    ) -> Vec<u8> {
        let mut bytes = self.take_pending_cleanup();
        self.stale_images.sort_unstable();
        self.stale_images.dedup();
        for image_id in self.stale_images.drain(..) {
            if self.host.images.remove(&image_id).is_some() {
                super::encode_delete_image(&mut bytes, image_id);
            }
            self.host.placements.retain(|(id, _), _| *id != image_id);
            self.host.sources.retain(|_, id| *id != image_id);
            self.host
                .replayed_placements
                .retain(|(id, _)| *id != image_id);
        }
        if !cell_size.is_known() || self.scope.is_empty() {
            bytes.extend(self.host.clear_bytes());
            return bytes;
        }
        let visible = self
            .display_scene
            .placements
            .iter()
            .filter_map(|placement| {
                client_placement_origin(placement, visibility, main_origin, popup_origin)
                    .map(|origin| (placement, origin))
            })
            .collect::<Vec<_>>();
        let pieces = occlusion.frame_pieces(&visible);
        let this = &*self;
        let placements = visible
            .iter()
            .zip(pieces)
            .flat_map(|(&(placement, origin), pieces)| {
                pieces.into_iter().enumerate().map(move |(index, piece)| {
                    let mut host = client_host_piece(
                        &this.scope,
                        placement,
                        origin,
                        cell_size,
                        piece,
                        index as u32,
                    );
                    host.host_image_id = Some(this.image_id(&placement.asset));
                    if this.host.images.get(&this.image_id(&placement.asset))
                        != Some(&image_signature_from_asset(&placement.asset))
                    {
                        if let Some(data) = this.assets.get(&placement.asset) {
                            host.placement.data = data.to_vec();
                        }
                    }
                    host
                })
            })
            .collect::<Vec<_>>();
        self.host.request_placement_replay();
        loop {
            let encoded = super::encode_graphics_update_incremental(
                &mut self.host,
                &placements,
                &HashSet::new(),
                None,
            );
            bytes.extend(encoded.bytes);
            if !encoded.incomplete {
                break;
            }
        }
        bytes
    }
}
#[cfg(unix)]
pub(crate) const NATIVE_TRANSFER_BIT: u64 = 1 << 63;
#[cfg(unix)]
pub(crate) const NATIVE_SLOT_BIT: u32 = 1 << 29;

pub(crate) fn native_host_image_id(scope: &str, key: &SurfaceGraphicsAssetKey) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    key.source.hash(&mut hasher);
    // Keep native IDs separate from the direct API namespace.
    0x4000_0000 | (hasher.finish() as u32 & 0x1fff_ffff)
}

pub(crate) fn host_image_id(scope: &str, key: &SurfaceGraphicsAssetKey) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    key.hash(&mut hasher);
    10_000 + ((hasher.finish() as u32) % 900_000)
}

fn image_signature_from_asset(key: &SurfaceGraphicsAssetKey) -> ImageSignature {
    ImageSignature {
        image_width: key.image_width,
        image_height: key.image_height,
        format_code: format_code(key.format),
        data_len: usize::try_from(key.data_len).unwrap_or(usize::MAX),
        data_fingerprint: key.data_fingerprint,
    }
}

fn client_placement_origin(
    placement: &SurfaceGraphicsPlacement,
    visibility: Visibility,
    main_origin: (u16, u16),
    popup_origin: Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    let origin = match (&placement.asset.source, visibility) {
        (
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Popup { .. },
                ..
            },
            Visibility::Popup,
        ) => popup_origin?,
        (
            SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane { .. },
                ..
            },
            Visibility::Main | Visibility::Popup,
        )
        | (SurfaceGraphicsSource::PaneLayer { .. }, Visibility::Main | Visibility::Popup) => {
            main_origin
        }
        _ => return None,
    };
    Some(origin)
}

fn client_host_piece(
    scope: &str,
    placement: &SurfaceGraphicsPlacement,
    origin: (u16, u16),
    cell_size: HostCellSize,
    piece: GridPiece,
    piece_index: u32,
) -> HostPlacement {
    let source_width = if placement.source_width == 0 {
        placement.asset.image_width
    } else {
        placement.source_width
    };
    let source_height = if placement.source_height == 0 {
        placement.asset.image_height
    } else {
        placement.source_height
    };
    let (source_x, piece_source_width) = piece_source_span(
        piece.col,
        piece.cols,
        placement.cols,
        cell_size.width_px,
        placement.x_offset,
        source_width,
    );
    let (source_y, piece_source_height) = piece_source_span(
        piece.row,
        piece.rows,
        placement.rows,
        cell_size.height_px,
        placement.y_offset,
        source_height,
    );
    let source_key = HostSourceKey::ClientSurface {
        scope: scope.to_owned(),
        source: placement.asset.source.clone(),
    };
    let signature = ImageSignature {
        image_width: placement.asset.image_width,
        image_height: placement.asset.image_height,
        format_code: format_code(placement.asset.format),
        data_len: usize::try_from(placement.asset.data_len).unwrap_or(usize::MAX),
        data_fingerprint: placement.asset.data_fingerprint,
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    placement.asset.source.hash(&mut hasher);
    signature.hash(&mut hasher);
    let raw = hasher.finish();
    let pane_id = PaneId::from_raw((raw as u32).max(1));
    let host_image_id = host_image_id(scope, &placement.asset);
    let cols = piece.cols.min(u32::from(u16::MAX)) as u16;
    let rows = piece.rows.min(u32::from(u16::MAX)) as u16;
    HostPlacement {
        pane_id,
        host_image_id: Some(host_image_id),
        area: Rect::new(
            origin
                .0
                .saturating_add(placement.x)
                .saturating_add(piece.col.min(u32::from(u16::MAX)) as u16),
            origin
                .1
                .saturating_add(placement.y)
                .saturating_add(piece.row.min(u32::from(u16::MAX)) as u16),
            cols,
            rows,
        ),
        cell_size,
        source_key,
        placement: KittyImagePlacement {
            // Only feeds the host placement id, so each piece gets its own.
            image_id: 1 + piece_index,
            placement_id: placement.logical_placement_id,
            z: placement.z,
            x_offset: if piece.col == 0 {
                placement.x_offset
            } else {
                0
            },
            y_offset: if piece.row == 0 {
                placement.y_offset
            } else {
                0
            },
            image_width: placement.asset.image_width,
            image_height: placement.asset.image_height,
            format: match placement.asset.format {
                SurfaceGraphicsFormat::Rgb => KittyImageFormat::Rgb,
                SurfaceGraphicsFormat::Rgba => KittyImageFormat::Rgba,
                SurfaceGraphicsFormat::Png => KittyImageFormat::Png,
            },
            data_len: usize::try_from(placement.asset.data_len).unwrap_or(usize::MAX),
            data_fingerprint: placement.asset.data_fingerprint,
            data: Vec::new(),
            render: KittyPlacementRenderInfo {
                pixel_width: piece.cols.saturating_mul(cell_size.width_px),
                pixel_height: piece.rows.saturating_mul(cell_size.height_px),
                grid_cols: piece.cols,
                grid_rows: piece.rows,
                viewport_col: 0,
                viewport_row: 0,
                source_x: placement.source_x.saturating_add(source_x),
                source_y: placement.source_y.saturating_add(source_y),
                source_width: piece_source_width,
                source_height: piece_source_height,
            },
        },
        scrollback_offset: placement.scrollback_offset,
    }
}

/// Maps a piece's cells onto the source pixels shown there. The leading pixel
/// offset shrinks the image inside its cells, and cuts use absolute boundaries so
/// adjacent pieces share exact source edges.
fn piece_source_span(
    start_cell: u32,
    cells: u32,
    total_cells: u32,
    cell_px: u32,
    offset: u32,
    source_len: u32,
) -> (u32, u32) {
    let display = u64::from(total_cells) * u64::from(cell_px);
    let offset = u64::from(offset).min(display.saturating_sub(1));
    let edge = |cell: u32| {
        let pixel = (u64::from(cell) * u64::from(cell_px)).max(offset);
        ((pixel - offset) * u64::from(source_len) / (display - offset).max(1)) as u32
    };
    let start = edge(start_cell);
    let end = edge(start_cell + cells);
    if end > start {
        (start, end - start)
    } else {
        // An upscaled sliver still needs one source pixel; zero means "whole image".
        (start.min(source_len.saturating_sub(1)), 1)
    }
}

fn format_code(format: SurfaceGraphicsFormat) -> u32 {
    match format {
        SurfaceGraphicsFormat::Rgb => 24,
        SurfaceGraphicsFormat::Rgba => 32,
        SurfaceGraphicsFormat::Png => 100,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint_wire::SurfaceGraphicsAsset;

    fn asset(
        target: SurfaceGraphicsTarget,
        fingerprint: u64,
        data: Vec<u8>,
    ) -> SurfaceGraphicsAsset {
        SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey {
                source: SurfaceGraphicsSource::Terminal {
                    target,
                    image_id: 7,
                },
                image_width: 1,
                image_height: 1,
                format: SurfaceGraphicsFormat::Rgba,
                data_len: data.len() as u64,
                data_fingerprint: fingerprint,
            },
            data,
        }
    }

    fn scene(asset: SurfaceGraphicsAsset, x: u16, y: u16) -> SurfaceGraphicsScene {
        SurfaceGraphicsScene {
            placements: vec![SurfaceGraphicsPlacement {
                asset: asset.key.clone(),
                logical_placement_id: 3,
                x,
                y,
                cols: 1,
                rows: 1,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                x_offset: 0,
                y_offset: 0,
                z: 0,
                scrollback_offset: 0,
            }],
            assets: vec![asset],
            retained_assets: Vec::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_replacements_use_two_ids_without_blank_pending_frames() {
        let mut state = ClientState::default();
        state.set_scope("double-buffer");
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let mut previous = None;
        let mut ids = HashSet::new();
        for revision in 0..32 {
            let image = asset(
                SurfaceGraphicsTarget::Pane {
                    pane_id: "pane".into(),
                },
                revision,
                vec![revision as u8, 2, 3, 255],
            );
            let base = native_host_image_id(&state.scope, &image.key);
            let id = if revision % 2 == 0 {
                base ^ NATIVE_SLOT_BIT
            } else {
                base
            };
            ids.insert(id);
            let mut metadata = scene(image.clone(), 0, 0);
            metadata.assets.clear();
            state.set_scene(metadata.clone());
            for _ in 0..3 {
                state.set_scene(metadata.clone());
                let pending = String::from_utf8(state.encode(
                    Visibility::Main,
                    (0, 0),
                    None,
                    cell,
                    &Occlusion::default(),
                ))
                .unwrap();
                assert!(!pending.contains("a=d,"), "{pending:?}");
                if let Some(previous) = previous {
                    assert!(pending.contains(&format!("a=p,i={previous},")));
                    assert!(!state.accepts_direct_asset(&image.key, previous));
                }
            }
            assert!(state.trust_direct_asset(&image.key, id));
            let swapped = String::from_utf8(state.encode(
                Visibility::Main,
                (0, 0),
                None,
                cell,
                &Occlusion::default(),
            ))
            .unwrap();
            assert!(swapped.contains(&format!("a=p,i={id},")));
            assert!(!swapped.contains(&format!("a=d,d=I,i={id},")));
            if let Some(previous) = previous {
                assert!(swapped.contains(&format!("a=d,d=I,i={previous},")));
            }
            assert_eq!(state.host.images.len(), 1);
            assert_eq!(state.native_image_ids.len(), 1);
            assert!(!state.accepts_direct_asset(&image.key, id));
            assert_eq!(id & 0x8000_0000, 0, "native IDs must not alias API IDs");
            previous = Some(id);
        }
        assert_eq!(ids.len(), 2);
        state.set_scene(SurfaceGraphicsScene::default());
        let cleanup = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(cleanup.contains(&format!("a=d,d=I,i={},", previous.unwrap())));
        assert!(state.host.images.is_empty());
        assert!(state.native_image_ids.is_empty());
    }

    #[test]
    fn retained_terminal_pixels_replay_without_upload_and_retire_when_authority_removes_them() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-boot-generation");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "opaque".into(),
            },
            7,
            vec![255, 0, 0, 255],
        );
        let visible = scene(image.clone(), 0, 0);
        let cell_size = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        state.set_scene(visible.clone());
        let uploaded = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell_size,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(uploaded.contains("a=t,t=d"));
        state.set_scene(SurfaceGraphicsScene {
            assets: vec![],
            placements: vec![],
            retained_assets: vec![image.key.clone()],
        });
        let hidden = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell_size,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(!hidden.contains("d=I"));
        assert!(!hidden.contains("a=t"));
        let mut replay = visible;
        replay.assets.clear();
        state.set_scene(replay);
        let restored = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell_size,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(restored.contains("a=p,"));
        assert!(!restored.contains("a=t"));
        state.set_scene(SurfaceGraphicsScene::default());
        let deleted = String::from_utf8(state.encode(
            Visibility::Main,
            (0, 0),
            None,
            cell_size,
            &Occlusion::default(),
        ))
        .unwrap();
        assert!(deleted.contains("d=I"));
        assert!(state.host.images.is_empty());
    }

    #[test]
    fn legacy_pane_layer_scene_still_uploads_and_places() {
        let mut state = ClientState::default();
        state.set_scope("old-server-pane-layer");
        let mut image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 255],
        );
        image.key.source = SurfaceGraphicsSource::PaneLayer {
            pane_id: "pane".into(),
            layer_id: "primary".into(),
        };
        state.set_scene(scene(image, 2, 1));

        let bytes = String::from_utf8(state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        ))
        .unwrap();

        assert!(bytes.contains("a=t,t=d"), "{bytes:?}");
        assert!(bytes.contains("a=p,"), "{bytes:?}");
        assert!(bytes.contains("\u{1b}[7;13H"), "{bytes:?}");
    }

    #[test]
    fn occlusion_uses_placement_cells_and_strict_overlap() {
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![1, 2, 3, 4],
        );
        let mut placement = scene(image, 1, 2).placements.remove(0);
        let mut cover = Occlusion::default();
        cover.cover(Rect::new(12, 8, 1, 1));
        // The image at (11, 7) just touches the cover's top-left corner.
        assert!(!cover.covers(&placement, (10, 5)));
        // Pixel offsets shrink the image inside its cells instead of bleeding.
        placement.x_offset = 1;
        placement.y_offset = 1;
        assert!(!cover.covers(&placement, (10, 5)));
        placement.cols = 2;
        placement.rows = 2;
        assert!(cover.covers(&placement, (10, 5)));
        assert!(!cover.covers(&placement, (0, 0)));
        cover = Occlusion::default();
        cover.cover(Rect::new(11, 7, 0, 1));
        assert!(!cover.covers(&placement, (10, 5)));
    }

    #[test]
    fn client_encodes_final_main_origin_upload_once_and_replays_placement() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            11,
            vec![1, 2, 3, 4],
        );
        state.set_scene(scene(image, 1, 2));

        let first = state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&first).contains("a=t,t=d"));
        assert!(String::from_utf8_lossy(&first).contains("\u{1b}[8;12H"));

        let second = state.encode(
            Visibility::Main,
            (10, 5),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        let second = String::from_utf8_lossy(&second);
        assert!(!second.contains("a=t,t=d"));
        assert!(second.contains("a=p"));
        assert!(second.contains("\u{1b}[8;12H"));
    }

    #[test]
    fn client_hides_and_restores_without_reuploading_pixels() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            12,
            vec![4, 3, 2, 1],
        );
        state.set_scene(scene(image, 0, 0));
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let _ = state.encode(Visibility::Main, (4, 2), None, cell, &Occlusion::default());

        let hidden = state.encode(
            Visibility::Hidden,
            (4, 2),
            None,
            cell,
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&hidden).contains("a=d,d=i"));

        let restored = state.encode(Visibility::Main, (4, 2), None, cell, &Occlusion::default());
        let restored = String::from_utf8_lossy(&restored);
        assert!(restored.contains("a=p"));
        assert!(!restored.contains("a=t,t=d"));
    }

    #[test]
    fn popup_candidates_use_client_resolved_popup_inner_origin() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Popup {
                terminal_id: "terminal-popup".into(),
            },
            13,
            vec![9, 8, 7, 6],
        );
        state.set_scene(scene(image, 2, 1));

        let bytes = state.encode(
            Visibility::Popup,
            (20, 4),
            Some((30, 10)),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&bytes).contains("\u{1b}[12;33H"));
    }

    #[test]
    fn replacement_scene_without_repeated_asset_bytes_keeps_resident_data() {
        let mut state = ClientState::default();
        state.set_scope("endpoint-a:boot-1");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "w1:p1".into(),
            },
            14,
            vec![1, 1, 1, 1],
        );
        let first_scene = scene(image, 0, 0);
        let mut replacement = first_scene.clone();
        replacement.assets.clear();
        state.set_scene(first_scene);
        state.set_scene(replacement);

        let bytes = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&bytes).contains("a=t,t=d"));
    }
    #[test]
    fn owner_generation_change_retires_old_image_before_new_upload() {
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let mut state = ClientState::default();
        state.set_scope("Local:1:boot");
        let image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "p1".into(),
            },
            1,
            vec![1, 2, 3, 4],
        );
        state.set_scene(scene(image.clone(), 0, 0));
        let first = state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert!(String::from_utf8_lossy(&first).contains("a=t,t=d"));
        state.set_scope("Ssh(machine):2:boot");
        state.set_scene(scene(image, 0, 0));
        let second = state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        let second = String::from_utf8_lossy(&second);
        assert!(second.find("a=d,d=I").unwrap() < second.find("a=t,t=d").unwrap());
        state.set_scope("");
        let retired = state.encode(
            Visibility::Hidden,
            (0, 0),
            None,
            cell,
            &Occlusion::default(),
        );
        assert!(String::from_utf8_lossy(&retired).contains("a=d,d=I"));
        assert!(state.host.images.is_empty());
        assert!(state.assets.is_empty());
    }
    fn grid_image(cols: u32, rows: u32) -> SurfaceGraphicsScene {
        let mut image = asset(
            SurfaceGraphicsTarget::Pane {
                pane_id: "pane".into(),
            },
            1,
            vec![0; (cols * 10 * rows * 10 * 4) as usize],
        );
        image.key.image_width = cols * 10;
        image.key.image_height = rows * 10;
        let mut graphics = scene(image, 0, 0);
        let placement = &mut graphics.placements[0];
        placement.cols = cols;
        placement.rows = rows;
        placement.source_width = cols * 10;
        placement.source_height = rows * 10;
        graphics
    }

    fn placed_pieces(bytes: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(bytes);
        let mut pieces = text
            .split("\u{1b}[")
            .filter(|chunk| chunk.contains("a=p,"))
            .map(|chunk| {
                let (cursor, rest) = chunk.split_once('H').unwrap();
                let control = rest
                    .trim_start_matches("\u{1b}_G")
                    .split(';')
                    .next()
                    .unwrap()
                    .split(',')
                    .filter(|field| {
                        ["c=", "r=", "x=", "y=", "w=", "h=", "X=", "Y="]
                            .iter()
                            .any(|key| field.starts_with(key))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{cursor} {control}")
            })
            .collect::<Vec<_>>();
        pieces.sort();
        pieces
    }

    fn encode_with_cover(
        graphics: SurfaceGraphicsScene,
        covers: &[Rect],
    ) -> (ClientState, Vec<String>) {
        let mut state = ClientState::default();
        state.set_scope("crop");
        state.set_scene(graphics);
        let mut occlusion = Occlusion::default();
        for rect in covers {
            occlusion.cover(*rect);
        }
        let bytes = state.encode(
            Visibility::Main,
            (0, 0),
            None,
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            &occlusion,
        );
        (state, placed_pieces(&bytes))
    }

    #[test]
    fn overlay_on_image_corner_keeps_the_rest_visible() {
        // A 10x4-cell image with 10px source pixels per cell; a toast covers
        // the top-right 4x2 cells.
        let (_, pieces) = encode_with_cover(grid_image(10, 4), &[Rect::new(6, 0, 4, 2)]);
        assert_eq!(
            pieces,
            vec![
                "1;1 c=6,r=2,w=60,h=20".to_owned(),
                "3;1 c=10,r=2,y=20,w=100,h=20".to_owned(),
            ]
        );
    }

    #[test]
    fn overlay_inside_image_keeps_four_surrounding_pieces() {
        let (_, pieces) = encode_with_cover(grid_image(10, 6), &[Rect::new(3, 2, 4, 2)]);
        assert_eq!(
            pieces,
            vec![
                "1;1 c=10,r=2,w=100,h=20".to_owned(),
                "3;1 c=3,r=2,y=20,w=30,h=20".to_owned(),
                "3;8 c=3,r=2,x=70,y=20,w=30,h=20".to_owned(),
                "5;1 c=10,r=2,y=40,w=100,h=20".to_owned(),
            ]
        );
    }

    #[test]
    fn overlay_across_image_edge_crops_only_the_covered_side() {
        let (_, pieces) = encode_with_cover(grid_image(10, 4), &[Rect::new(7, 0, 20, 20)]);
        assert_eq!(pieces, vec!["1;1 c=7,r=4,w=70,h=40".to_owned()]);
    }

    #[test]
    fn multiple_overlays_crop_around_each_other() {
        let (_, pieces) = encode_with_cover(
            grid_image(10, 4),
            &[Rect::new(0, 0, 2, 1), Rect::new(8, 3, 2, 1)],
        );
        assert_eq!(
            pieces,
            vec![
                "1;3 c=8,r=1,x=20,w=80,h=10".to_owned(),
                "2;1 c=10,r=2,y=10,w=100,h=20".to_owned(),
                "4;1 c=8,r=1,y=30,w=80,h=10".to_owned(),
            ]
        );
    }

    #[test]
    fn pixel_offsets_stay_on_the_leading_pieces_without_seams() {
        let mut graphics = grid_image(10, 4);
        graphics.placements[0].x_offset = 4;
        graphics.placements[0].y_offset = 8;
        // Offsets shrink the image inside its cells: 100x40 source pixels span
        // 76x56 display pixels starting at (4, 8).
        let (_, pieces) = encode_with_cover(graphics.clone(), &[Rect::new(1, 0, 1, 4)]);
        assert_eq!(
            pieces,
            vec![
                "1;1 c=1,r=4,w=5,h=40,X=4,Y=8".to_owned(),
                "1;3 c=8,r=4,x=15,w=85,h=40,Y=8".to_owned(),
            ]
        );
        let (_, pieces) = encode_with_cover(graphics, &[Rect::new(0, 1, 10, 1)]);
        assert_eq!(
            pieces,
            vec![
                "1;1 c=10,r=1,w=100,h=5,X=4,Y=8".to_owned(),
                "3;1 c=10,r=2,y=17,w=100,h=23,X=4".to_owned(),
            ]
        );
    }

    #[test]
    fn upscaled_slivers_never_fall_back_to_the_whole_source() {
        let mut graphics = grid_image(10, 1);
        let placement = &mut graphics.placements[0];
        placement.source_x = 30;
        placement.source_width = 2;
        let (_, pieces) = encode_with_cover(graphics, &[Rect::new(1, 0, 9, 1)]);
        assert_eq!(pieces, vec!["1;1 c=1,r=1,x=30,w=1,h=10".to_owned()]);
    }

    #[test]
    fn uneven_source_scaling_leaves_no_seam_between_pieces() {
        let mut graphics = grid_image(3, 1);
        graphics.placements[0].source_width = 10;
        let (_, pieces) = encode_with_cover(graphics, &[Rect::new(1, 0, 1, 1)]);
        assert_eq!(
            pieces,
            vec![
                "1;1 c=1,r=1,w=3,h=10".to_owned(),
                "1;3 c=1,r=1,x=6,w=4,h=10".to_owned(),
            ]
        );
    }

    #[test]
    fn over_budget_frames_hide_touched_images_whole() {
        // A 40x4 image with holes in every other cell of two rows needs 42
        // pieces, so the frame falls back to hiding every touched image.
        let mut graphics = grid_image(40, 4);
        let fragmented = graphics.placements[0].clone();
        let mut cropped = fragmented.clone();
        cropped.logical_placement_id = 4;
        cropped.y = 10;
        cropped.cols = 10;
        let mut untouched = cropped.clone();
        untouched.logical_placement_id = 5;
        untouched.x = 20;
        graphics.placements.extend([cropped, untouched]);
        let mut covers = (0..40)
            .map(|index| Rect::new(index % 20 * 2, index / 20 * 2 + 1, 1, 1))
            .collect::<Vec<_>>();
        covers.push(Rect::new(6, 10, 4, 2));
        let (state, pieces) = encode_with_cover(graphics.clone(), &covers);
        assert_eq!(pieces, vec!["11;21 c=10,r=4,w=400,h=40".to_owned()]);
        assert_eq!(state.host.placements.len(), 1);

        // Without the fragmenting holes the same frame crops normally.
        let (_, pieces) = encode_with_cover(graphics, &covers[40..]);
        assert_eq!(pieces.len(), 4, "{pieces:?}");
    }

    #[test]
    fn moving_overlay_replaces_and_retires_pieces() {
        let mut state = ClientState::default();
        state.set_scope("crop");
        state.set_scene(grid_image(10, 6));
        let cell = HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let mut cover = Occlusion::default();
        cover.cover(Rect::new(3, 2, 4, 2));
        let _ = state.encode(Visibility::Main, (0, 0), None, cell, &cover);
        assert_eq!(state.host.placements.len(), 4);
        let mut cover = Occlusion::default();
        cover.cover(Rect::new(6, 0, 4, 2));
        let moved = state.encode(Visibility::Main, (0, 0), None, cell, &cover);
        assert!(!String::from_utf8_lossy(&moved).contains("a=t"));
        assert_eq!(state.host.placements.len(), 2);
        let restored = state.encode(Visibility::Main, (0, 0), None, cell, &Occlusion::default());
        assert_eq!(
            placed_pieces(&restored),
            vec!["1;1 c=10,r=6,w=100,h=60".to_owned()]
        );
        assert_eq!(state.host.placements.len(), 1);
    }
}
