//! Fixed 5da native bank/export state over the fork's decoded scene collector.
use super::*;
use crate::kitty_graphics::endpoint_client::{NATIVE_SLOT_BIT, NATIVE_TRANSFER_BIT as NATIVE_BIT};
use crate::kitty_graphics::endpoint_scene::DeliveryCache;
use crate::pane_graphics_files::{FileStore, OwnedExport};
use crate::protocol::endpoint_wire::{
    ServerMessage, SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
    SurfaceGraphicsScene, SurfaceGraphicsSource,
};
use std::collections::HashSet;
use std::sync::Arc;
const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
pub(super) struct Pending {
    export: Arc<OwnedExport>,
    transfer_id: u64,
    image_id: u32,
    source: SurfaceGraphicsSource,
    asset: SurfaceGraphicsAssetKey,
    slot: bool,
    deadline: Instant,
    written: bool,
    refresh_needed: bool,
    scene: SurfaceGraphicsScene,
    delivery: DeliveryCache,
}

struct AcknowledgedSlot {
    asset: SurfaceGraphicsAssetKey,
    slot: bool,
}

pub(in crate::server::headless) struct NativeGraphics {
    store: FileStore,
    pending: HashMap<u64, Pending>,
    disabled: HashSet<u64>,
    next_transfer: u64,
    // The key and bank currently addressed for each logical source. Native state
    // advances on ACK; inline state and source pruning advance on queued scenes.
    acknowledged_slots: HashMap<u64, HashMap<SurfaceGraphicsSource, AcknowledgedSlot>>,
}
impl Default for NativeGraphics {
    fn default() -> Self {
        Self {
            store: FileStore::default(),
            pending: HashMap::new(),
            disabled: HashSet::new(),
            next_transfer: 1,
            acknowledged_slots: HashMap::new(),
        }
    }
}
impl NativeGraphics {
    pub(super) fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub(super) fn can_hold(&self, client: u64, scene: &SurfaceGraphicsScene) -> bool {
        let Some(pending) = self.pending.get(&client) else {
            return true;
        };
        let same_image =
            |a: &crate::protocol::endpoint_wire::SurfaceGraphicsAssetKey,
             b: &crate::protocol::endpoint_wire::SurfaceGraphicsAssetKey| {
                a.source == b.source
                    && a.image_width == b.image_width
                    && a.image_height == b.image_height
                    && a.format == b.format
                    && a.data_len == b.data_len
            };
        scene.placements.len() == pending.scene.placements.len()
            && scene
                .placements
                .iter()
                .zip(&pending.scene.placements)
                .all(|(current, held)| {
                    if !same_image(&current.asset, &held.asset) {
                        return false;
                    }
                    let mut geometry = current.clone();
                    geometry.asset = held.asset.clone();
                    geometry == *held
                })
            && scene.retained_assets.len() == pending.scene.retained_assets.len()
            && scene
                .retained_assets
                .iter()
                .zip(&pending.scene.retained_assets)
                .all(|(current, held)| same_image(current, held))
    }

    pub(super) fn hold(
        &mut self,
        client: u64,
        current: &SurfaceGraphicsScene,
    ) -> Option<(SurfaceGraphicsScene, DeliveryCache)> {
        if let Some(p) = self.pending.get_mut(&client) {
            p.refresh_needed |= !current.assets.is_empty()
                || current.placements != p.scene.placements
                || current.retained_assets != p.scene.retained_assets;
        }
        self.pending
            .get(&client)
            .map(|p| (p.scene.clone(), p.delivery.clone()))
    }
    /// Records bank state only after the scene metadata has been queued.
    ///
    /// `inline_assets` contains only payloads actually queued on the wire; a
    /// selected native upload has already been removed and changes bank only
    /// after its ACK. A new inline key addresses bank 0, while replaying the
    /// exact native key keeps the client's existing key-to-bank mapping.
    /// Pruning is committed here as well: a scene rejected by the writer must
    /// not mutate bank history for pixels the client is still displaying.
    pub(super) fn commit_scene(
        &mut self,
        client: u64,
        scene: &SurfaceGraphicsScene,
        inline_assets: &[SurfaceGraphicsAssetKey],
    ) {
        let present_sources: HashSet<_> = scene
            .placements
            .iter()
            .map(|placement| &placement.asset.source)
            .chain(scene.retained_assets.iter().map(|asset| &asset.source))
            .cloned()
            .collect();
        let slots = self.acknowledged_slots.entry(client).or_default();
        slots.retain(|source, _| present_sources.contains(source));
        for asset in inline_assets
            .iter()
            .filter(|asset| matches!(asset.source, SurfaceGraphicsSource::Terminal { .. }))
        {
            let same_resident_key = slots
                .get(&asset.source)
                .is_some_and(|resident| resident.asset == *asset);
            if !same_resident_key {
                slots.insert(
                    asset.source.clone(),
                    AcknowledgedSlot {
                        asset: asset.clone(),
                        slot: false,
                    },
                );
            }
        }
        if slots.is_empty() {
            self.acknowledged_slots.remove(&client);
        }
    }
    pub(super) fn prepare(
        &mut self,
        client: u64,
        scope: &str,
        scene: &mut SurfaceGraphicsScene,
        delivery: &DeliveryCache,
    ) -> Option<(Pending, ServerMessage)> {
        if self.disabled.contains(&client)
            || self.pending.contains_key(&client)
            || self.pending.len() >= 8
        {
            return None;
        }
        let index = scene.assets.iter().position(eligible)?;
        let asset = &scene.assets[index];
        if self.pending.values().map(|p| p.export.len()).sum::<usize>()
            + asset.key.data_len as usize
            > MAX_BYTES
            || self.next_transfer >= NATIVE_BIT
        {
            return None;
        }
        let export = Arc::new(self.store.export(&asset.data).ok()?);
        let transfer_id = NATIVE_BIT | self.next_transfer;
        self.next_transfer += 1;
        let asset_key = asset.key.clone();
        let source = asset_key.source.clone();
        // Start in bank 1: an older client may still be displaying an inline image
        // under the unchanged logical base ID. Thereafter always stage into the
        // bank opposite the one this client currently addresses.
        let slot = !self
            .acknowledged_slots
            .get(&client)
            .and_then(|slots| slots.get(&source))
            .map(|resident| resident.slot)
            .unwrap_or(false);
        let base = crate::kitty_graphics::endpoint_client::native_host_image_id(scope, &asset.key);
        let image_id = base ^ if slot { NATIVE_SLOT_BIT } else { 0 };
        let message = ServerMessage::GraphicsFile {
            path: export.path().to_string_lossy().into_owned(),
            expected_len: export.len() as u64,
            image_id,
            transfer_id,
            leading: Vec::new(),
            control: format!(
                "a=t,f=32,s={},v={},i={image_id},q=0",
                asset.key.image_width, asset.key.image_height
            ),
            surface_asset: Some(asset_key.clone()),
        };
        scene.assets.remove(index);
        let mut held = scene.clone();
        held.assets.clear();
        Some((
            Pending {
                export,
                transfer_id,
                image_id,
                source,
                asset: asset_key,
                slot,
                deadline: Instant::now() + DELIVERY_TIMEOUT,
                written: false,
                refresh_needed: delivery.has_pending(),
                scene: held,
                delivery: delivery.clone(),
            },
            message,
        ))
    }
    pub(super) fn commit(&mut self, client: u64, pending: Pending) {
        self.pending.insert(client, pending);
    }
    fn matches(&self, client: u64, transfer: u64, image: u32) -> bool {
        self.pending
            .get(&client)
            .is_some_and(|p| p.transfer_id == transfer && p.image_id == image)
    }
}
fn eligible(asset: &SurfaceGraphicsAsset) -> bool {
    matches!(asset.key.source, SurfaceGraphicsSource::Terminal { .. })
        && asset.key.format == SurfaceGraphicsFormat::Rgba
        && asset.key.data_len > 0
        && asset.key.data_len <= MAX_FILE as u64
        && asset.key.data_len == asset.data.len() as u64
        && u64::from(asset.key.image_width)
            .checked_mul(u64::from(asset.key.image_height))
            .and_then(|pixels| pixels.checked_mul(4))
            == Some(asset.key.data_len)
}
impl HeadlessServer {
    pub(super) fn endpoint_native_started(
        &mut self,
        client: u64,
        transfer: u64,
        image: u32,
    ) -> bool {
        if transfer & NATIVE_BIT == 0 {
            return false;
        }
        if !self.endpoint_native.disabled.contains(&client)
            && self.endpoint_native.matches(client, transfer, image)
        {
            let pending = self
                .endpoint_native
                .pending
                .get_mut(&client)
                .expect("matched");
            pending.written = true;
            pending.deadline = Instant::now() + RESPONSE_TIMEOUT;
        }
        false
    }
    pub(super) fn endpoint_native_result(
        &mut self,
        client: u64,
        transfer: u64,
        image: u32,
        success: bool,
    ) -> bool {
        if transfer & NATIVE_BIT == 0 || !self.endpoint_native.matches(client, transfer, image) {
            return false;
        }
        if success && !self.endpoint_native.disabled.contains(&client) {
            if !self.endpoint_native.pending[&client].written {
                return false;
            }
            let pending = self
                .endpoint_native
                .pending
                .remove(&client)
                .expect("matched");
            self.endpoint_native
                .acknowledged_slots
                .entry(client)
                .or_default()
                .insert(
                    pending.source,
                    AcknowledgedSlot {
                        asset: pending.asset,
                        slot: pending.slot,
                    },
                );
            if let Some(client) = self.endpoint_clients.get_mut(&client) {
                client.write_pending |= pending.refresh_needed;
            }
            return pending.refresh_needed;
        }
        self.endpoint_native.disabled.insert(client);
        self.endpoint_native
            .pending
            .get_mut(&client)
            .expect("matched")
            .deadline = Instant::now();
        self.expire_endpoint_native(Instant::now())
    }
    pub(in crate::server::headless) fn expire_endpoint_native(&mut self, now: Instant) -> bool {
        let expired: Vec<_> = self
            .endpoint_native
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(id, p)| (*id, p.transfer_id, p.image_id))
            .collect();
        let mut changed = false;
        for (id, transfer_id, image_id) in expired {
            self.endpoint_native.disabled.insert(id);
            let Some(client) = self.endpoint_clients.get_mut(&id) else {
                continue;
            };
            let message = ServerMessage::GraphicsTransmissionRetired {
                transfer_id,
                image_id,
            };
            let sent = super::framed(&message)
                .ok()
                .is_some_and(|bytes| client.writer.control.send(bytes).is_ok());
            // Keep the export alive until retirement is queued or disconnect cleans it.
            if !sent {
                continue;
            }
            self.endpoint_native.pending.remove(&id);
            client.graphics_delivery = DeliveryCache::default();
            client.write_pending = true;
            changed = true;
        }
        changed
    }
    pub(super) fn retire_endpoint_native(&mut self, client: u64) {
        if let Some(pending) = self.endpoint_native.pending.get_mut(&client) {
            pending.deadline = Instant::now();
            self.endpoint_native.disabled.insert(client);
            self.expire_endpoint_native(Instant::now());
        }
    }
    pub(super) fn disconnect_endpoint_native(&mut self, client: u64) {
        self.endpoint_native.pending.remove(&client);
        self.endpoint_native.disabled.remove(&client);
        self.endpoint_native.acknowledged_slots.remove(&client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint_wire::{SurfaceGraphicsAssetKey, SurfaceGraphicsTarget};

    fn asset() -> SurfaceGraphicsAsset {
        SurfaceGraphicsAsset {
            key: SurfaceGraphicsAssetKey {
                source: SurfaceGraphicsSource::Terminal {
                    target: SurfaceGraphicsTarget::Pane {
                        pane_id: "native-test".into(),
                    },
                    image_id: 1,
                },
                image_width: 1,
                image_height: 1,
                format: SurfaceGraphicsFormat::Rgba,
                data_len: 4,
                data_fingerprint: 123,
            },
            data: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn pending_guard_and_client_identity_are_independent() {
        let mut state = NativeGraphics::default();
        let mut scene = SurfaceGraphicsScene {
            assets: vec![asset()],
            ..Default::default()
        };
        let (pending, _) = state
            .prepare(7, "scope", &mut scene, &DeliveryCache::default())
            .unwrap();
        let path = pending.export.path().to_owned();
        let transfer = pending.transfer_id;
        let image = pending.image_id;
        assert_ne!(transfer & NATIVE_BIT, 0);
        assert!(scene.assets.is_empty());
        assert!(!state.busy()); // prepare is not a delivery commit
        state.commit(7, pending);
        assert!(state.matches(7, transfer, image));
        assert!(!state.matches(8, transfer, image));
        assert!(!state.matches(7, transfer + 1, image));
        assert!(!state.matches(7, transfer, image + 1));
        assert!(path.exists());
        assert!(state.hold(7, &scene).unwrap().0.assets.is_empty());
        state.pending.remove(&7);
        assert!(!path.exists());
        state.disabled.insert(7);
        scene.assets.push(asset());
        assert!(state
            .prepare(7, "scope", &mut scene, &DeliveryCache::default())
            .is_none());
        assert_eq!(scene.assets.len(), 1); // native fallback keeps authoritative inline bytes
    }
}
