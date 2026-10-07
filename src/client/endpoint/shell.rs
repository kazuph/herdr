//! Client-owned endpoint projections. Runtime resources stay on their owning server.
use super::cache::EndpointCache;
use super::jobs::EndpointJobsCache;
use super::{ClientEndpointId, ClientEndpointStatus, ResourceKey};
use crate::machine::MachineProfile;
#[cfg(test)]
use crate::protocol::endpoint_wire::ClientShellSnapshot;
use crate::protocol::endpoint_wire::{ClientSurfaceSize, PaneSurfaceFrame};

pub(crate) struct ClientShellEndpoint {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) label: String,
    pub(crate) status: ClientEndpointStatus,
    pub(crate) generation: Option<u64>,
    /// Latest connection diagnostic, shown with the endpoint instead of as a global notice.
    pub(crate) diagnostic: Option<String>,
    pub(crate) cache: EndpointCache,
    pub(crate) jobs: EndpointJobsCache,
}

impl ClientShellEndpoint {
    fn new(endpoint_id: ClientEndpointId, label: String, enabled: bool) -> Self {
        Self {
            endpoint_id,
            label,
            status: if enabled {
                ClientEndpointStatus::Connecting
            } else {
                ClientEndpointStatus::Disabled
            },
            generation: None,
            diagnostic: None,
            cache: EndpointCache::default(),
            jobs: EndpointJobsCache::default(),
        }
    }
}

pub(crate) struct ClientShellState {
    pub(crate) endpoints: Vec<ClientShellEndpoint>,
    pub(crate) active_endpoint_id: ClientEndpointId,
    pub(crate) pane_surface: Option<PaneSurfaceFrame>,
    pub(crate) outer_focused: Option<bool>,
}

impl ClientShellState {
    pub(crate) fn new() -> Self {
        Self {
            endpoints: vec![ClientShellEndpoint::new(
                ClientEndpointId::Local,
                "Local".into(),
                true,
            )],
            active_endpoint_id: ClientEndpointId::Local,
            pane_surface: None,
            outer_focused: None,
        }
    }

    pub(crate) fn set_endpoint_catalog(
        &mut self,
        profiles: &[MachineProfile],
    ) -> Vec<ClientEndpointId> {
        let mut previous = std::mem::take(&mut self.endpoints);
        let local = previous
            .iter()
            .position(|endpoint| endpoint.endpoint_id.is_local())
            .map(|index| previous.remove(index))
            .unwrap_or_else(|| {
                ClientShellEndpoint::new(ClientEndpointId::Local, "Local".into(), true)
            });
        self.endpoints.push(local);
        for profile in profiles {
            let id = ClientEndpointId::Ssh(profile.id.clone());
            let mut endpoint = previous
                .iter()
                .position(|endpoint| endpoint.endpoint_id == id)
                .map(|index| previous.remove(index))
                .unwrap_or_else(|| {
                    ClientShellEndpoint::new(id, profile.label.clone(), profile.enabled)
                });
            endpoint.label.clone_from(&profile.label);
            if !profile.enabled {
                if let Some(generation) = endpoint.generation {
                    endpoint.cache.disconnect(generation);
                }
                endpoint.status = ClientEndpointStatus::Disabled;
            } else if endpoint.status == ClientEndpointStatus::Disabled {
                endpoint.status = ClientEndpointStatus::Connecting;
            }
            self.endpoints.push(endpoint);
        }
        if !self
            .endpoint(&self.active_endpoint_id)
            .is_some_and(|endpoint| endpoint.status != ClientEndpointStatus::Disabled)
        {
            self.active_endpoint_id = ClientEndpointId::Local;
            self.pane_surface = None;
        }
        previous
            .into_iter()
            .map(|endpoint| endpoint.endpoint_id)
            .collect()
    }

    pub(crate) fn endpoint(&self, id: &ClientEndpointId) -> Option<&ClientShellEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == *id)
    }

    pub(crate) fn endpoint_mut(
        &mut self,
        id: &ClientEndpointId,
    ) -> Option<&mut ClientShellEndpoint> {
        self.endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_id == *id)
    }

    pub(crate) fn begin_connection(&mut self, id: &ClientEndpointId, generation: u64) -> bool {
        let Some(endpoint) = self.endpoint_mut(id) else {
            return false;
        };
        if endpoint.status == ClientEndpointStatus::Disabled
            || !endpoint.cache.begin_connection(generation)
        {
            return false;
        }
        endpoint.jobs.begin_connection(generation);
        endpoint.generation = Some(generation);
        endpoint.status = ClientEndpointStatus::Connecting;
        if self.endpoint_is_active(id) {
            self.pane_surface = None;
        }
        true
    }

    pub(crate) fn disconnect(&mut self, id: &ClientEndpointId, generation: u64) -> bool {
        let Some(endpoint) = self.endpoint_mut(id) else {
            return false;
        };
        if !endpoint.cache.disconnect(generation) {
            return false;
        }
        if endpoint.status != ClientEndpointStatus::Disabled {
            endpoint.status = ClientEndpointStatus::Reconnecting;
        }
        if self.endpoint_is_active(id) {
            self.pane_surface = None;
        }
        true
    }

    pub(crate) fn receive_snapshot(
        &mut self,
        id: &ClientEndpointId,
        generation: u64,
        snapshot: impl Into<crate::protocol::endpoint_projection::SnapshotJson>,
    ) -> bool {
        let Some(endpoint) = self.endpoint_mut(id) else {
            return false;
        };
        if !endpoint.cache.replace_snapshot(generation, snapshot) {
            return false;
        }
        endpoint.status = ClientEndpointStatus::Online;
        endpoint.diagnostic = None;
        true
    }

    pub(crate) fn receive_surface(
        &mut self,
        id: &ClientEndpointId,
        generation: u64,
        surface: PaneSurfaceFrame,
    ) -> bool {
        self.endpoint_mut(id)
            .is_some_and(|endpoint| endpoint.cache.replace_surface(generation, surface))
    }

    pub(crate) fn endpoint_snapshot_identity(
        &self,
        id: &ClientEndpointId,
        generation: u64,
    ) -> Option<(&str, u64)> {
        let snapshot = self.endpoint(id)?.cache.live_snapshot(generation)?;
        Some((&snapshot.boot_id, snapshot.revision))
    }

    pub(crate) fn endpoint_boot_id(&self, id: &ClientEndpointId) -> Option<&str> {
        Some(&self.endpoint(id)?.cache.snapshot()?.boot_id)
    }

    pub(crate) fn endpoint_snapshot_matches(
        &self,
        id: &ClientEndpointId,
        generation: u64,
        boot_id: &str,
        revision: u64,
    ) -> bool {
        self.endpoint_snapshot_identity(id, generation) == Some((boot_id, revision))
    }

    pub(crate) fn endpoint_surface_matches(
        &self,
        id: &ClientEndpointId,
        generation: u64,
        surface: &PaneSurfaceFrame,
    ) -> bool {
        let size = ClientSurfaceSize {
            cols: surface.frame.width,
            rows: surface.frame.height,
        };
        self.endpoint(id)
            .and_then(|endpoint| endpoint.cache.coherent_surface(generation, size))
            .is_some_and(|current| super::cache::same_surface(current, surface))
    }

    pub(crate) fn endpoint_projection_available(&self, id: &ClientEndpointId) -> bool {
        self.endpoint(id).is_some_and(|endpoint| {
            endpoint.status == ClientEndpointStatus::Online
                && endpoint
                    .generation
                    .is_some_and(|generation| endpoint.cache.live_snapshot(generation).is_some())
        })
    }

    pub(crate) fn activate_endpoint_projection(&mut self, id: &ClientEndpointId) -> bool {
        if !self.endpoint_projection_available(id) {
            return false;
        }
        if !self.endpoint_is_active(id) {
            self.pane_surface = None;
        }
        self.active_endpoint_id = id.clone();
        true
    }

    pub(crate) fn set_endpoint_status(
        &mut self,
        id: &ClientEndpointId,
        status: ClientEndpointStatus,
    ) {
        if let Some(endpoint) = self.endpoint_mut(id) {
            endpoint.status = status;
        }
    }

    pub(crate) fn set_endpoint_diagnostic(&mut self, id: &ClientEndpointId, message: String) {
        if let Some(endpoint) = self.endpoint_mut(id) {
            endpoint.diagnostic = (!message.is_empty()).then_some(message);
        }
    }

    pub(crate) fn host_focus_baseline(&self) -> bool {
        self.outer_focused.unwrap_or(true)
    }

    pub(crate) fn endpoint_is_active(&self, id: &ClientEndpointId) -> bool {
        self.active_endpoint_id == *id
    }

    pub(crate) fn set_pane_surface(&mut self, surface: PaneSurfaceFrame) {
        let valid = self
            .endpoint(&self.active_endpoint_id)
            .and_then(|endpoint| endpoint.generation)
            .is_some_and(|generation| {
                self.endpoint_surface_matches(&self.active_endpoint_id, generation, &surface)
            });
        self.pane_surface = valid.then_some(surface);
    }

    pub(crate) fn aggregate_workspaces(&self) -> Vec<ResourceKey> {
        self.endpoints
            .iter()
            .filter(|endpoint| endpoint.status != ClientEndpointStatus::Disabled)
            .flat_map(|endpoint| {
                endpoint
                    .cache
                    .snapshot()
                    .into_iter()
                    .flat_map(move |snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .map(move |workspace| ResourceKey {
                                endpoint: endpoint.endpoint_id.clone(),
                                id: workspace.workspace_id.clone(),
                            })
                    })
            })
            .collect()
    }

    pub(crate) fn aggregate_agents(&self) -> Vec<ResourceKey> {
        self.endpoints
            .iter()
            .filter(|endpoint| endpoint.status != ClientEndpointStatus::Disabled)
            .flat_map(|endpoint| {
                endpoint
                    .cache
                    .snapshot()
                    .into_iter()
                    .flat_map(move |snapshot| {
                        snapshot
                            .agent_order
                            .iter()
                            .filter(|id| snapshot.agents.iter().any(|agent| agent.pane_id == **id))
                            .map(move |id| ResourceKey {
                                endpoint: endpoint.endpoint_id.clone(),
                                id: id.clone(),
                            })
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap()
    }

    fn profile(id: &str, enabled: bool) -> MachineProfile {
        MachineProfile {
            id: id.into(),
            label: id.into(),
            target: "no-ssh".into(),
            session: "original-session".into(),
            enabled,
        }
    }

    #[test]
    fn endpoint_shell_reconnect_retains_listing_without_stealing_selection_or_old_authority() {
        let mut shell = ClientShellState::new();
        let other = ClientEndpointId::Ssh("m-old-fork-id".into());
        shell.set_endpoint_catalog(&[profile("m-old-fork-id", true)]);
        for id in [&ClientEndpointId::Local, &other] {
            assert!(shell.begin_connection(id, 1));
            assert!(shell.receive_snapshot(id, 1, snapshot()));
        }
        let keys = shell.aggregate_workspaces();
        assert!(!keys.is_empty());
        assert!(shell.disconnect(&other, 1));
        assert_eq!(shell.aggregate_workspaces(), keys);
        assert!(shell.begin_connection(&other, 2));
        assert!(shell.endpoint_snapshot_identity(&other, 2).is_none());
        assert!(!shell.receive_snapshot(&other, 1, snapshot()));
        assert!(shell.receive_snapshot(&other, 2, snapshot()));
        assert_eq!(shell.active_endpoint_id, ClientEndpointId::Local);
        assert!(shell.activate_endpoint_projection(&other));
        assert!(shell.disconnect(&ClientEndpointId::Local, 1));
        assert!(shell.begin_connection(&ClientEndpointId::Local, 2));
        assert!(shell.receive_snapshot(&ClientEndpointId::Local, 2, snapshot()));
        assert_eq!(shell.active_endpoint_id, other);
        assert!(!shell.disconnect(&other, 1));
        assert!(shell.endpoint_projection_available(&other));
    }

    #[test]
    fn endpoint_shell_catalog_order_disabled_and_removed_ids_keep_fork_profiles_opaque() {
        let mut shell = ClientShellState::new();
        let first = ClientEndpointId::Ssh("m1".into());
        let second = ClientEndpointId::Ssh("custom:opaque/id".into());
        shell.set_endpoint_catalog(&[profile("m1", true), profile("custom:opaque/id", true)]);
        for id in [&ClientEndpointId::Local, &first, &second] {
            shell.begin_connection(id, 1);
            shell.receive_snapshot(id, 1, snapshot());
        }
        let keys = shell.aggregate_workspaces();
        let local_count = snapshot().workspaces.len();
        assert!(local_count > 0);
        assert_eq!(keys[0].endpoint, ClientEndpointId::Local);
        assert_eq!(keys[local_count].endpoint, first);
        assert_eq!(keys[local_count * 2].endpoint, second);
        assert!(shell.activate_endpoint_projection(&first));
        shell.set_endpoint_catalog(&[profile("custom:opaque/id", true), profile("m1", false)]);
        assert_eq!(shell.active_endpoint_id, ClientEndpointId::Local);
        assert!(!shell.begin_connection(&first, 2));
        assert!(!shell.endpoint_projection_available(&first));
        assert_eq!(shell.aggregate_workspaces()[local_count].endpoint, second);
        let removed = shell.set_endpoint_catalog(&[profile("custom:opaque/id", true)]);
        assert_eq!(removed, vec![first]);
        assert!(shell
            .endpoint(&second)
            .unwrap()
            .cache
            .live_snapshot(1)
            .is_some());
    }
}
