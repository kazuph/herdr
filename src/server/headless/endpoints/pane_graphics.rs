use super::*;

impl HeadlessServer {
    pub(in crate::server::headless) fn endpoint_has_active_graphics_surface(&self) -> bool {
        self.endpoint_clients.values().any(|client| client.active)
    }

    fn single_endpoint_graphics_owner(&self) -> Option<u64> {
        if self.app_client_count() != 0 {
            return None;
        }
        let mut active = self.endpoint_clients.iter().filter(|(_, c)| c.active);
        let (id, client) = active.next()?;
        if active.next().is_some()
            || !client.cell_size.is_known()
            || client.surface.is_none()
            || client.published_projection_revision != Some(client.projection_revision)
        {
            return None;
        }
        Some(*id)
    }

    pub(in crate::server::headless) fn endpoint_graphics_info_runtime(
        &self,
        pane_id: &str,
    ) -> Option<crate::app::pane_graphics::InfoRuntime> {
        let id = self.single_endpoint_graphics_owner()?;
        let client = self.endpoint_clients.get(&id)?;
        let visible = client
            .surface
            .as_ref()
            .is_some_and(|surface| surface.panes.iter().any(|pane| pane.pane_id == pane_id));
        Some(crate::app::pane_graphics::InfoRuntime {
            cell_size: client.cell_size,
            pane_visible: visible,
            direct_available: self.endpoint_pane_direct_owner(pane_id).is_some(),
            pixel_mouse: client.pixel_mouse,
        })
    }

    pub(in crate::server::headless) fn endpoint_pane_direct_owner(
        &self,
        pane_id: &str,
    ) -> Option<u64> {
        #[cfg(unix)]
        {
            let id = self.single_endpoint_graphics_owner()?;
            let client = self.endpoint_clients.get(&id)?;
            let tab = client.location.focused_tab_id()?;
            if !client.direct_graphics
                || self.endpoint_tab_geometry.get(tab) != Some(&id)
                || self.endpoint_native.busy()
                || !client
                    .surface
                    .as_ref()?
                    .panes
                    .iter()
                    .any(|p| p.pane_id == pane_id)
            {
                return None;
            }
            Some(id)
        }
        #[cfg(not(unix))]
        {
            let _ = pane_id;
            None
        }
    }

    pub(in crate::server::headless) fn request_endpoint_graphics(&mut self, id: u64) {
        if let Some(client) = self.endpoint_clients.get_mut(&id) {
            client.write_pending = true;
        }
    }

    pub(in crate::server::headless) fn endpoint_has_direct_host(&self) -> bool {
        #[cfg(unix)]
        {
            self.single_endpoint_graphics_owner()
                .is_some_and(|id| self.endpoint_clients[&id].direct_graphics)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(in crate::server::headless) fn retire_endpoint_pane_graphics(
        &mut self,
        id: u64,
        transfer: u64,
        image: u32,
    ) -> bool {
        let Some(client) = self.endpoint_clients.get_mut(&id) else {
            return false;
        };
        let sent = framed(&wire::ServerMessage::GraphicsTransmissionRetired {
            transfer_id: transfer,
            image_id: image,
        })
        .ok()
        .is_some_and(|bytes| client.writer.control.send(bytes).is_ok());
        if sent {
            #[cfg(unix)]
            {
                client.direct_graphics = false;
            }
            client.graphics_delivery = Default::default();
            client.write_pending = true;
        }
        sent
    }

    pub(super) fn prepare_endpoint_pane_graphics(
        &self,
        id: u64,
        scene: &mut wire::SurfaceGraphicsScene,
    ) -> Option<(crate::app::pane_graphics::Key, u32, wire::ServerMessage)> {
        let (key, slot) = self.app.pane_graphics.slots.iter().find(|(_, slot)| {
            slot.stream_is_active()
                && slot
                    .direct_gate
                    .as_ref()
                    .is_some_and(|g| g.client_id == id && g.endpoint_image_id.is_none())
        })?;
        let gate = slot.direct_gate.as_ref()?;
        let lease = slot.layer.as_ref()?.direct_lease()?;
        let public = self
            .app
            .state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws, workspace)| {
                workspace
                    .pane_state(key.0)
                    .and_then(|_| self.app.public_pane_id(ws, key.0))
            })?;
        let index = scene.assets.iter().position(|asset| {
            asset.key.source
                == wire::SurfaceGraphicsSource::PaneLayer {
                    pane_id: public.clone(),
                    layer_id: key.1.clone(),
                }
        })?;
        let asset = scene.assets.remove(index).key;
        let image_id =
            crate::kitty_graphics::endpoint_client::host_image_id(&self.endpoint_boot_id, &asset);
        let message = wire::ServerMessage::GraphicsFile {
            path: lease.path().to_string_lossy().into_owned(),
            expected_len: lease.len() as u64,
            image_id,
            transfer_id: gate.transfer_id,
            leading: Vec::new(),
            control: format!(
                "a=t,f=32,s={},v={},i={image_id},q=0",
                asset.image_width, asset.image_height
            ),
            surface_asset: Some(asset),
        };
        Some((key.clone(), image_id, message))
    }

    #[cfg(unix)]
    fn endpoint_pane_direct_image(&self, id: u64, transfer: u64, image: u32) -> Option<u32> {
        self.app.pane_graphics.slots.values().find_map(|slot| {
            slot.direct_gate
                .as_ref()
                .filter(|gate| {
                    gate.client_id == id
                        && gate.transfer_id == transfer
                        && gate.endpoint_image_id == Some(image)
                })
                .map(|_| slot.host_image_id)
        })
    }

    #[cfg(unix)]
    pub(super) fn endpoint_pane_direct_started(
        &mut self,
        id: u64,
        transfer: u64,
        image: u32,
    ) -> bool {
        self.endpoint_pane_direct_image(id, transfer, image)
            .is_some_and(|original| self.start_direct_graphics_response(id, transfer, original))
    }

    #[cfg(unix)]
    pub(super) fn endpoint_pane_direct_result(
        &mut self,
        id: u64,
        transfer: u64,
        image: u32,
        success: bool,
    ) -> bool {
        let Some(original) = self.endpoint_pane_direct_image(id, transfer, image) else {
            return false;
        };
        if !success {
            if let Some(client) = self.endpoint_clients.get_mut(&id) {
                client.direct_graphics = false;
                client.graphics_delivery = Default::default();
                client.write_pending = true;
            }
        }
        self.complete_direct_graphics(id, transfer, original, success)
    }
}
