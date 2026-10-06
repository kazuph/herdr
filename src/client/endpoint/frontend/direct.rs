//! Local exports retain the fixed asset, terminal ACK, and retirement ownership.
use super::*;
use crate::client::direct_graphics::{Response, ResponseMatcher};
use crate::protocol::endpoint_wire::SurfaceGraphicsAssetKey;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

// Fixed 5da client/state.rs; never evict a delayed transfer tombstone.
const MAX_RETIRED_DIRECT_GRAPHICS: usize = 64;
#[derive(Default)]
pub(super) struct Direct {
    pub(super) matcher: Arc<Mutex<ResponseMatcher>>,
    pending: Option<Pending>,
    retired: HashMap<(ClientEndpointId, u64), Vec<(u64, u32)>>,
    saturated: HashSet<(ClientEndpointId, u64)>,
    disabled: HashSet<(ClientEndpointId, u64)>,
}
struct Pending {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    transfer: u64,
    image: u32,
    asset: SurfaceGraphicsAssetKey,
}
fn boot(f: &ClientFrontend, id: &ClientEndpointId, generation: u64) -> Option<String> {
    f.runtime
        .shell
        .endpoint(id)?
        .cache
        .live_snapshot(generation)
        .map(|s| s.boot_id.clone())
}
fn send(
    f: &mut ClientFrontend,
    id: &ClientEndpointId,
    generation: u64,
    message: wire::ClientMessage,
) {
    if f.runtime.endpoints.accepts(id, generation) {
        f.runtime.endpoints.send_to(id, &message);
    }
}
fn remember(d: &mut Direct, id: &ClientEndpointId, generation: u64, transfer: u64, image: u32) {
    let owner = (id.clone(), generation);
    let items = d.retired.entry(owner.clone()).or_default();
    if d.saturated.contains(&owner) || items.contains(&(transfer, image)) {
        return;
    }
    if items.len() == MAX_RETIRED_DIRECT_GRAPHICS {
        d.saturated.insert(owner);
    } else {
        items.push((transfer, image));
    }
}
pub(super) fn cancel(f: &mut ClientFrontend) {
    if let Some(p) = f.direct.pending.take() {
        remember(
            &mut f.direct,
            &p.endpoint,
            p.generation,
            p.transfer,
            p.image,
        );
        if let Ok(mut m) = f.direct.matcher.lock() {
            m.retire(p.transfer);
        }
        f.graphics.retire_direct_image(p.image);
        send(
            f,
            &p.endpoint,
            p.generation,
            wire::ClientMessage::GraphicsTransmissionResult {
                transfer_id: p.transfer,
                image_id: p.image,
                success: false,
            },
        );
    }
}
pub(super) fn effect(
    f: &mut ClientFrontend,
    effect: &super::super::runtime::QualifiedHostEffect,
) -> io::Result<bool> {
    let id = &effect.endpoint_id;
    let generation = effect.generation;
    match &effect.message {
        wire::ServerMessage::GraphicsTransmissionRetired {
            transfer_id,
            image_id,
        } => {
            if !f.runtime.endpoints.accepts(id, generation) {
                return Ok(true);
            }
            remember(&mut f.direct, id, generation, *transfer_id, *image_id);
            if transfer_id & crate::kitty_graphics::endpoint_client::NATIVE_TRANSFER_BIT != 0 {
                f.direct.disabled.insert((id.clone(), generation));
            }
            if f.direct.pending.as_ref().is_some_and(|p| {
                p.endpoint == *id
                    && p.generation == generation
                    && p.transfer == *transfer_id
                    && p.image == *image_id
            }) {
                f.direct.pending = None;
                if let Ok(mut m) = f.direct.matcher.lock() {
                    m.retire(*transfer_id);
                }
                f.graphics.retire_direct_image(*image_id);
                f.force_redraw = true;
            }
            Ok(true)
        }
        wire::ServerMessage::GraphicsFile {
            path,
            expected_len,
            image_id,
            transfer_id,
            leading,
            control,
            surface_asset,
        } => {
            // Check transport before any remote path can reach the host filesystem.
            let local = id.is_local() && !crate::client::is_remote_client_process();
            let owner = (id.clone(), generation);
            if f.direct
                .retired
                .get_mut(&owner)
                .and_then(|items| {
                    items
                        .iter()
                        .position(|tuple| *tuple == (*transfer_id, *image_id))
                        .map(|i| items.swap_remove(i))
                })
                .is_some()
            {
                return Ok(true);
            }
            let owner_current = f.runtime.endpoints.accepts(id, generation)
                && f.runtime.endpoints.active_id() == id;
            let lease_before_draw = f.runtime.input_lease_current();
            if owner_current {
                f.draw()?;
            }
            let current = owner_current && f.runtime.input_lease_current();
            let native = surface_asset
                .as_ref()
                .is_some_and(|a| matches!(a.source, wire::SurfaceGraphicsSource::Terminal { .. }));
            let valid = local
                && current
                && !f.direct.saturated.contains(&owner)
                && (!native || !f.direct.disabled.contains(&owner))
                && f.direct.pending.is_none()
                && surface_asset.as_ref().is_some_and(|a| {
                    f.graphics.accepts_direct_asset(a, *image_id)
                        && (!native
                            || (leading.is_empty()
                                && a.format == wire::SurfaceGraphicsFormat::Rgba
                                && *expected_len == a.data_len
                                && *control
                                    == format!(
                                        "a=t,f=32,s={},v={},i={image_id},q=0",
                                        a.image_width, a.image_height
                                    )))
                })
                && usize::try_from(*expected_len).ok().is_some_and(|len| {
                    let path = std::path::Path::new(path);
                    (crate::pane_graphics_files::validate_direct_source(path, len).is_ok()
                        || (native
                            && crate::pane_graphics_files::validate_native_source(path, len)
                                .is_ok()))
                        && crate::client::direct_graphics::valid_control(control, *image_id, len)
                })
                && f.direct
                    .matcher
                    .lock()
                    .is_ok_and(|mut m| m.arm(*transfer_id, *image_id));
            tracing::debug!(endpoint = ?id, generation, transfer_id, image_id,
                local, current, native, valid, lease_before_draw, lease = f.runtime.input_lease_current(),
                asset_accepted = surface_asset.as_ref().is_some_and(|a| f.graphics.accepts_direct_asset(a, *image_id)),
                "endpoint direct file eligibility");
            let written = if valid {
                use std::io::Write as _;
                let mut bytes = Vec::new();
                crate::kitty_graphics::encode_kitty_regular_file(
                    &mut bytes, leading, control, path,
                );
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(&f.graphics.take_pending_cleanup())
                    .and_then(|()| stdout.write_all(&bytes))
                    .and_then(|()| stdout.flush())
                    .map(|()| crate::client::record_received_kitty_graphics(&bytes))
                    .is_ok()
            } else {
                false
            };
            if written {
                if let (Some(asset), Some(boot)) = (surface_asset.clone(), boot(f, id, generation))
                {
                    f.direct.pending = Some(Pending {
                        endpoint: id.clone(),
                        generation,
                        boot,
                        transfer: *transfer_id,
                        image: *image_id,
                        asset,
                    });
                    if let Ok(mut m) = f.direct.matcher.lock() {
                        m.start(*transfer_id);
                    }
                    send(
                        f,
                        id,
                        generation,
                        wire::ClientMessage::GraphicsTransmissionStarted {
                            transfer_id: *transfer_id,
                            image_id: *image_id,
                        },
                    );
                }
            } else {
                if let Ok(mut m) = f.direct.matcher.lock() {
                    if valid {
                        m.retire(*transfer_id);
                    } else {
                        m.cancel(*transfer_id);
                    }
                }
                send(
                    f,
                    id,
                    generation,
                    wire::ClientMessage::GraphicsTransmissionResult {
                        transfer_id: *transfer_id,
                        image_id: *image_id,
                        success: false,
                    },
                );
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}
pub(super) fn response(f: &mut ClientFrontend, response: Response) -> io::Result<()> {
    if !f
        .direct
        .pending
        .as_ref()
        .is_some_and(|p| p.transfer == response.transfer_id && p.image == response.image_id)
    {
        return Ok(());
    }
    let p = f.direct.pending.take().expect("matching pending transfer");
    let eligible = response.success
        && f.runtime.endpoints.accepts(&p.endpoint, p.generation)
        && f.runtime.endpoints.active_id() == &p.endpoint
        && boot(f, &p.endpoint, p.generation).as_ref() == Some(&p.boot)
        && f.runtime.input_lease_current()
        && f.graphics.accepts_direct_asset(&p.asset, p.image);
    let checkpoint = f.graphics.clone();
    let accepted = eligible && f.graphics.trust_direct_asset(&p.asset, p.image) && f.draw().is_ok();
    if !accepted {
        f.graphics = checkpoint;
        f.graphics.retire_direct_image(p.image);
        f.force_redraw = true;
    }
    send(
        f,
        &p.endpoint,
        p.generation,
        wire::ClientMessage::GraphicsTransmissionResult {
            transfer_id: p.transfer,
            image_id: p.image,
            success: accepted,
        },
    );
    Ok(())
}
