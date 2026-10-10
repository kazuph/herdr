//! Endpoint receive/selection loop. Host effects remain qualified until the TUI applies them.
use std::io;
use std::time::Instant;

use tokio::sync::mpsc;

use super::activation::{
    ActivationBeginError, ActivationCompletion, ActivationRollback, PendingEndpointActivation,
    SurfaceActivationProgress,
};
use super::commands::{EndpointCommandRequest, EndpointCommandResult, EndpointCommands};
use super::handshake::EndpointConnectOptions;
use super::shell::ClientShellState;
use super::supervisor::{EndpointSupervisorEvent, EndpointSupervisors};
use super::transport::{self, EndpointReaderEvent};
use super::{
    ClientEndpointId, ClientEndpointStatus, EndpointRegistry, EndpointSendOutcome, FocusTarget,
    ResourceKey,
};
use crate::protocol::endpoint_wire::{ClientMessage, ServerMessage};

pub(crate) struct QualifiedHostEffect {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) message: ServerMessage,
}

#[derive(Default)]
pub(crate) struct RuntimeUpdate {
    pub(crate) repaint: bool,
    pub(crate) clear_host_effects: bool,
    pub(crate) host_effects: Vec<QualifiedHostEffect>,
    pub(crate) notifications: Vec<QualifiedHostEffect>,
    pub(crate) completed: Vec<EndpointCommandResult>,
    pub(crate) cancelled: Vec<ResourceKey>,
    pub(crate) error: Option<String>,
}

pub(crate) struct EndpointRuntime {
    pub(crate) shell: ClientShellState,
    pub(crate) endpoints: EndpointRegistry,
    pub(crate) supervisors: EndpointSupervisors,
    pub(crate) reader_events: mpsc::Receiver<EndpointReaderEvent>,
    pub(crate) supervisor_events: mpsc::Receiver<EndpointSupervisorEvent>,
    reader_tx: mpsc::Sender<EndpointReaderEvent>,
    supervisor_tx: mpsc::Sender<EndpointSupervisorEvent>,
    commands: EndpointCommands,
    pending: Option<PendingEndpointActivation>,
    presentation_frozen: bool,
    next_serial: u64,
    options: EndpointConnectOptions,
    deferred_catalog: Option<Vec<crate::machine::MachineProfile>>,
    deferred_local: Option<Option<FocusTarget>>,
    restore_selected: Option<(ClientEndpointId, u64)>,
    /// After the window regains focus the server redraws its viewer surface and ignores input
    /// until it has one. Keys stay queued (not dropped) until that surface arrives.
    refocus_hold: Option<Instant>,
}

/// Upper bound for holding keys after a focus return, so input can never stall.
const REFOCUS_HOLD_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

impl EndpointRuntime {
    pub(crate) fn new(
        shell: ClientShellState,
        endpoints: EndpointRegistry,
        supervisors: EndpointSupervisors,
        options: EndpointConnectOptions,
    ) -> Self {
        // The existing fork client event queue is 256; retain its bound for both producers.
        let (reader_tx, reader_events) = mpsc::channel(256);
        let (supervisor_tx, supervisor_events) = mpsc::channel(256);
        let restore_selected = endpoints
            .connection(endpoints.active_id())
            .filter(|connection| !connection.surface_active)
            .map(|connection| (endpoints.active_id().clone(), connection.generation));
        Self {
            shell,
            endpoints,
            supervisors,
            reader_events,
            supervisor_events,
            reader_tx,
            supervisor_tx,
            commands: EndpointCommands::default(),
            pending: None,
            presentation_frozen: true,
            next_serial: 1,
            options,
            deferred_catalog: None,
            deferred_local: None,
            restore_selected,
            refocus_hold: None,
        }
    }

    pub(crate) fn reader_sender(&self) -> mpsc::Sender<EndpointReaderEvent> {
        self.reader_tx.clone()
    }

    fn resize_message(&self) -> ClientMessage {
        ClientMessage::ClientShellResize {
            cell_width_px: self.options.cell_width_px,
            cell_height_px: self.options.cell_height_px,
            surface_size: self.options.surface_size,
            pixel_mouse: self.options.pixel_geometry_exact,
        }
    }

    fn cancel_lane(&mut self, id: &ClientEndpointId, update: &mut RuntimeUpdate) {
        update
            .cancelled
            .extend(
                self.commands
                    .disconnect(id)
                    .into_iter()
                    .map(|request_id| ResourceKey {
                        endpoint: id.clone(),
                        id: request_id,
                    }),
            );
    }

    pub(crate) fn apply_catalog(
        &mut self,
        profiles: Vec<crate::machine::MachineProfile>,
        now: Instant,
    ) -> RuntimeUpdate {
        if self.pending.is_some() {
            self.deferred_catalog = Some(profiles);
            return RuntimeUpdate::default();
        }
        let mut update = RuntimeUpdate {
            repaint: true,
            ..RuntimeUpdate::default()
        };
        let retired = self.supervisors.reconcile_profiles(&profiles, now);
        for id in retired {
            if let Some(generation) = self
                .endpoints
                .connection(&id)
                .map(|connection| connection.generation)
            {
                self.endpoints.disconnect(&id);
                self.shell.disconnect(&id, generation);
            }
            self.cancel_lane(&id, &mut update);
            if self.endpoints.active_id() == &id {
                self.endpoints.select_unavailable_local();
                self.presentation_frozen = true;
                update.clear_host_effects = true;
            }
        }
        self.shell.set_endpoint_catalog(&profiles);
        update
    }

    /// A machine parked at its unapproved Tailscale approval wait retries on
    /// click; any other machine keeps the click's collapse-toggle meaning, so
    /// the caller falls through to the toggle when this returns None. The new
    /// attempt is picked up by the next maintenance tick.
    pub(crate) fn retry_parked_endpoint(
        &mut self,
        id: &ClientEndpointId,
        now: Instant,
    ) -> Option<RuntimeUpdate> {
        let parked = self
            .shell
            .endpoint(id)
            .is_some_and(|endpoint| endpoint.status == ClientEndpointStatus::AwaitingApproval);
        if !parked {
            return None;
        }
        if self.supervisors.retry(id, now) {
            self.shell
                .set_endpoint_status(id, ClientEndpointStatus::Reconnecting);
        }
        Some(RuntimeUpdate {
            repaint: true,
            ..RuntimeUpdate::default()
        })
    }

    pub(crate) fn supervisor_event(
        &mut self,
        event: EndpointSupervisorEvent,
        now: Instant,
    ) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        match event {
            EndpointSupervisorEvent::Status {
                endpoint_id,
                generation,
                status,
                message,
                interim,
            } => {
                // An interim report (for example a pending Tailscale SSH
                // approval URL) only updates the machine's display; resolving
                // the attempt's retry bookkeeping would start a duplicate
                // connection while this one is still running.
                let accepted = if interim {
                    self.supervisors
                        .is_current_attempt(&endpoint_id, generation)
                } else {
                    self.supervisors
                        .record_status(&endpoint_id, generation, status, now)
                };
                if accepted {
                    // Connection progress is shown with the machine, not as a global notice.
                    tracing::info!(endpoint = ?endpoint_id, ?status, %message, "endpoint connection status");
                    self.shell.set_endpoint_status(&endpoint_id, status);
                    if !interim
                        && matches!(
                            status,
                            ClientEndpointStatus::Attention
                                | ClientEndpointStatus::AwaitingApproval
                        )
                    {
                        // Only a state that needs the user's action is also announced.
                        let label = self.shell.endpoint(&endpoint_id).map_or_else(
                            || "machine".to_owned(),
                            |endpoint| endpoint.label.clone(),
                        );
                        update.error = Some(format!("{label}: {message}"));
                    }
                    self.shell.set_endpoint_diagnostic(&endpoint_id, message);
                    update.repaint = true;
                }
            }
            EndpointSupervisorEvent::Connected {
                endpoint_id,
                generation,
                stream,
                lifetime,
                negotiation,
            } => {
                if !self.supervisors.record_status(
                    &endpoint_id,
                    generation,
                    ClientEndpointStatus::Online,
                    now,
                ) {
                    return update;
                }
                if !self.shell.begin_connection(&endpoint_id, generation) {
                    return update;
                }
                self.cancel_lane(&endpoint_id, &mut update);
                match transport::start(
                    stream,
                    lifetime,
                    endpoint_id.clone(),
                    generation,
                    &negotiation,
                    self.reader_tx.clone(),
                ) {
                    Ok(writer) => {
                        if self.shell.endpoint_is_active(&endpoint_id) {
                            self.restore_selected = Some((endpoint_id.clone(), generation));
                        }
                        self.endpoints.insert(
                            endpoint_id.clone(),
                            writer,
                            generation,
                            negotiation,
                            false,
                        );
                        // A config reload can finish while the connection handshake is in flight.
                        self.endpoints.send_to(
                            &endpoint_id,
                            &ClientMessage::ClientShellMouseCapture {
                                enabled: self.options.mouse_capture,
                            },
                        );
                        update.repaint = true;
                    }
                    Err(error) => {
                        self.shell.disconnect(&endpoint_id, generation);
                        self.supervisors.disconnected(&endpoint_id, generation, now);
                        update.error = Some(error.to_string());
                        update.repaint = true;
                    }
                }
            }
        }
        update
    }

    pub(crate) fn activate(
        &mut self,
        id: ClientEndpointId,
        target: Option<FocusTarget>,
        now: Instant,
    ) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        self.deferred_local = None;
        self.restore_selected = None;
        if let Some(pending) = self.pending.as_mut() {
            if pending.can_retarget(&id) {
                if let Err(error) = pending.retarget(target, &mut self.endpoints) {
                    self.rollback(error, false, &mut update);
                }
                return update;
            }
            if pending.retains_source() && pending.source() == &id {
                // Returning to the endpoint that never left the screen cancels the slow switch
                // and continues with the requested navigation there.
                let outcome =
                    pending.rollback(&mut self.endpoints, "switch cancelled".into(), false);
                self.pending = None;
                self.release_retained(outcome, &mut update);
                if target.is_none() {
                    update.repaint = true;
                    return update;
                }
            }
        }
        if let Some(pending) = self.pending.as_mut() {
            // Local navigation must never wait for a remote release acknowledgement.
            if id.is_local() {
                pending.abandon(&mut self.endpoints);
                self.pending = None;
            } else {
                let outcome = pending.supersede(id, target, &mut self.endpoints);
                self.rollback_outcome(outcome, &mut update);
                return update;
            }
        }
        if id.is_local() && !self.shell.endpoint_projection_available(&id) {
            self.deferred_local = Some(target);
            self.endpoints.select_unavailable_local();
            self.shell.active_endpoint_id = ClientEndpointId::Local;
            self.shell.pane_surface = None;
            self.presentation_frozen = true;
            update.clear_host_effects = true;
            update.repaint = true;
            return update;
        }
        self.next_serial = self.next_serial.saturating_add(1);
        match PendingEndpointActivation::prepare(
            &self.shell,
            &self.endpoints,
            id,
            target,
            self.resize_message(),
            self.next_serial,
            now,
        ) {
            Ok(prepared) => {
                if let Some(source) = prepared.source_command_lane() {
                    update
                        .cancelled
                        .extend(
                            self.commands
                                .retire_lane(source)
                                .into_iter()
                                .map(|request_id| ResourceKey {
                                    endpoint: source.clone(),
                                    id: request_id,
                                }),
                        );
                }
                // A target-first switch keeps presenting the source until the target commits.
                self.presentation_frozen = !prepared.retains_source();
                match prepared.start(&mut self.endpoints) {
                    Ok(pending) => self.pending = Some(pending),
                    Err(ActivationBeginError::Partial { activation, error }) => {
                        self.pending = Some(*activation);
                        self.rollback(error, false, &mut update);
                    }
                    Err(ActivationBeginError::Preflight(error)) => update.error = Some(error),
                }
            }
            Err(ActivationBeginError::Preflight(error)) => update.error = Some(error),
            Err(ActivationBeginError::Partial { .. }) => unreachable!("prepare performs no writes"),
        }
        update
    }

    /// Ends a target-first switch whose target failed. The source never stopped presenting.
    fn release_retained(&mut self, outcome: ActivationRollback, update: &mut RuntimeUpdate) {
        if let ActivationRollback::Retained(_) = outcome {
            self.presentation_frozen = false;
            self.endpoints.unfreeze_input();
            update.repaint = true;
        }
    }

    fn rollback_outcome(&mut self, outcome: ActivationRollback, update: &mut RuntimeUpdate) {
        if let ActivationRollback::Retained(error) = outcome {
            let pending = self.pending.take();
            let target = pending.as_ref().map(|pending| pending.target().clone());
            let successor = pending
                .as_ref()
                .and_then(|pending| pending.successor().cloned());
            self.release_retained(ActivationRollback::Retained(String::new()), update);
            if let Some(target) = target {
                let label = self
                    .shell
                    .endpoint(&target)
                    .map_or_else(|| "machine".to_owned(), |endpoint| endpoint.label.clone());
                self.shell.set_endpoint_diagnostic(&target, error.clone());
                update.error = Some(format!("{label} did not respond: {error}"));
            }
            if let Some(next) = successor {
                let next_update = self.activate(next.endpoint_id, next.target, Instant::now());
                update.repaint |= next_update.repaint;
                update.cancelled.extend(next_update.cancelled);
                update.clear_host_effects |= next_update.clear_host_effects;
                update.error = next_update.error.or(update.error.take());
            }
            return;
        }
        self.presentation_frozen = true;
        if let ActivationRollback::Unavailable(error) = outcome {
            if let Some(pending) = self.pending.take() {
                pending.abandon(&mut self.endpoints);
            }
            self.endpoints.freeze_input();
            self.shell.pane_surface = None;
            update.error = Some(error);
            update.clear_host_effects = true;
            update.repaint = true;
        }
    }

    fn rollback(
        &mut self,
        error: String,
        source_release_rejected: bool,
        update: &mut RuntimeUpdate,
    ) {
        if let Some(pending) = self.pending.as_mut() {
            let outcome = pending.rollback(&mut self.endpoints, error, source_release_rejected);
            self.rollback_outcome(outcome, update);
        }
    }

    fn progress(
        &mut self,
        progress: SurfaceActivationProgress,
        now: Instant,
        update: &mut RuntimeUpdate,
    ) {
        match progress {
            SurfaceActivationProgress::Ready => {
                let Some(pending) = self.pending.as_mut() else {
                    return;
                };
                match pending.complete(&mut self.shell, &mut self.endpoints) {
                    Ok(ActivationCompletion::AwaitingPresentationSync { previous, endpoint }) => {
                        if previous != endpoint {
                            update.cancelled.extend(
                                self.commands.retire_lane(&previous).into_iter().map(
                                    |request_id| ResourceKey {
                                        endpoint: previous.clone(),
                                        id: request_id,
                                    },
                                ),
                            );
                        }
                        self.presentation_frozen = false;
                        update.clear_host_effects = true;
                        update.repaint = true;
                    }
                    Ok(ActivationCompletion::AwaitingPresentationEffects) => {}
                    Ok(completion) => {
                        self.pending = None;
                        self.presentation_frozen = false;
                        self.endpoints.unfreeze_input();
                        // Output may advance the cache while the ordered effects fence
                        // is in flight. Present its current coherent surface before input.
                        let active = self.endpoints.active_id().clone();
                        if let Some(connection) = self.endpoints.connection(&active) {
                            self.publish_surface(&active, connection.generation);
                        }
                        update.repaint = true;
                        if let ActivationCompletion::RestoredSource { error, successor } =
                            completion
                        {
                            update.error = Some(error);
                            if let Some(next) = successor {
                                let next_update = self.activate(next.endpoint_id, next.target, now);
                                update.cancelled.extend(next_update.cancelled);
                                update.clear_host_effects |= next_update.clear_host_effects;
                                update.error = next_update.error.or(update.error.take());
                            }
                        }
                    }
                    Err(error) => self.rollback(error, false, update),
                }
            }
            SurfaceActivationProgress::Rejected {
                message,
                source_release_rejected,
            } => self.rollback(message, source_release_rejected, update),
            SurfaceActivationProgress::Pending | SurfaceActivationProgress::Stale => {}
        }
    }

    fn disconnect(
        &mut self,
        id: &ClientEndpointId,
        generation: u64,
        error: io::Error,
        now: Instant,
        update: &mut RuntimeUpdate,
    ) {
        if !self
            .shell
            .endpoint(id)
            .is_some_and(|endpoint| endpoint.cache.accepts(generation))
        {
            return;
        }
        self.endpoints.disconnect(id);
        self.shell.disconnect(id, generation);
        self.supervisors.disconnected(id, generation, now);
        self.cancel_lane(id, update);
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.involves_endpoint(id))
        {
            if let Some(pending) = self.pending.as_mut() {
                let outcome =
                    pending.endpoint_disconnected(&mut self.endpoints, id, error.to_string());
                self.rollback_outcome(outcome, update);
            }
        } else if self.endpoints.active_id() == id {
            self.endpoints.freeze_input();
            self.presentation_frozen = true;
            update.clear_host_effects = true;
        }
        update.repaint = true;
        tracing::warn!(endpoint = ?id, %error, "endpoint connection lost");
        // The machine row and the main-area placeholder carry the reason; other endpoints'
        // presentation is unaffected, so this is not a global notice.
        self.shell.set_endpoint_diagnostic(id, error.to_string());
    }

    pub(crate) fn receive(&mut self, event: EndpointReaderEvent, now: Instant) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        let (id, generation, message) = match event {
            EndpointReaderEvent::Disconnected {
                endpoint_id,
                generation,
                error,
            } => {
                if self.endpoints.accepts(&endpoint_id, generation) {
                    self.disconnect(&endpoint_id, generation, error, now, &mut update);
                }
                return update;
            }
            EndpointReaderEvent::Message {
                endpoint_id,
                generation,
                message,
            } => (endpoint_id, generation, *message),
        };
        if !self.endpoints.accepts(&id, generation) {
            return update;
        }
        self.endpoints.received(&id, generation, now);
        let active = self.endpoints.active_id() == &id
            && self
                .endpoints
                .connection(&id)
                .is_some_and(|connection| connection.surface_active);
        // While a target-first switch is pending, the retained source keeps presenting.
        let presenting = self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.retains_source() && pending.source() == &id);
        let pending_owner = self.pending.as_ref().is_some_and(|pending| {
            pending.accepts_endpoint(&id, generation)
                && !(pending.retains_source() && pending.source() == &id)
        });
        let mut progress = SurfaceActivationProgress::Pending;
        let mut accepted_snapshot = false;
        match message {
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND =>
            {
                match serde_json::from_str::<crate::protocol::endpoint_projection::SnapshotJson>(
                    &data,
                ) {
                    Ok(snapshot) => {
                        if self.shell.receive_snapshot(&id, generation, snapshot) {
                            accepted_snapshot = true;
                            self.endpoints.mark_ready(&id, generation);
                            if let Some(snapshot) = self
                                .shell
                                .endpoint(&id)
                                .and_then(|endpoint| endpoint.cache.live_snapshot(generation))
                            {
                                if let Some(pending) = self.pending.as_mut() {
                                    progress = pending.receive_snapshot(&id, generation, snapshot);
                                }
                            }
                            update.repaint = true;
                            if active && presenting && !self.presentation_frozen {
                                self.publish_surface(&id, generation);
                            }
                        }
                    }
                    Err(error) => self.disconnect(
                        &id,
                        generation,
                        io::Error::new(io::ErrorKind::InvalidData, error),
                        now,
                        &mut update,
                    ),
                }
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint_jobs::JOBS_PROJECTION_KIND =>
            {
                if let Ok(projection) = serde_json::from_str(&data) {
                    if let Some(endpoint) = self.shell.endpoint_mut(&id) {
                        update.repaint =
                            endpoint
                                .jobs
                                .replace(generation, &endpoint.cache, projection);
                    }
                }
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint_decisions::DECISIONS_PROJECTION_KIND =>
            {
                match serde_json::from_str::<
                    crate::protocol::endpoint_decisions::EndpointDecisionsProjection,
                >(&data)
                {
                    Ok(projection) => {
                        if let Some(endpoint) = self.shell.endpoint_mut(&id) {
                            update.repaint |=
                                endpoint
                                    .decisions
                                    .replace(generation, &endpoint.cache, projection);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(endpoint = ?id, %error, "decisions projection parse failed");
                    }
                }
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND =>
            {
                if let Some(pending) = self.pending.as_mut() {
                    progress = pending.receive_presentation_effects_ready(&id, generation, &data);
                }
            }
            ServerMessage::EndpointControl { kind, .. } if kind.starts_with("shell.snapshot.") => {
                self.disconnect(
                    &id,
                    generation,
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("unsupported mandatory endpoint snapshot codec {kind:?}"),
                    ),
                    now,
                    &mut update,
                );
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND
                    || kind == crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND =>
            {
                update.host_effects.push(QualifiedHostEffect {
                    endpoint_id: id.clone(),
                    generation,
                    message: ServerMessage::EndpointControl { kind, data },
                });
            }
            ServerMessage::EndpointControl { .. } => {}
            ServerMessage::PaneSurface(surface) if active || pending_owner => {
                if self.shell.receive_surface(&id, generation, surface.clone()) {
                    if let Some(pending) = self.pending.as_mut().filter(|_| pending_owner) {
                        // Activation must compare the accepted receive-time scene,
                        // including pixels retained from earlier bounded asset batches.
                        if let Some(received) = self
                            .shell
                            .endpoint(&id)
                            .and_then(|endpoint| endpoint.cache.received_surface(generation))
                        {
                            progress = pending.receive_surface(&id, generation, received.clone());
                        }
                    } else if active && presenting && !self.presentation_frozen {
                        self.publish_surface(&id, generation);
                        // Only a full surface proves the server redrew after a focus return;
                        // an older in-flight patch does not.
                        self.refocus_hold = None;
                        update.repaint = true;
                    }
                }
            }
            ServerMessage::PaneSurfacePatch(patch) if active || pending_owner => {
                let surface = self
                    .shell
                    .endpoint_mut(&id)
                    .and_then(|endpoint| endpoint.cache.apply_patch(generation, patch))
                    .map(|surface| pending_owner.then(|| surface.clone()));
                if let Some(surface) = surface {
                    if let (Some(pending), Some(surface)) = (self.pending.as_mut(), surface) {
                        progress = pending.receive_surface(&id, generation, surface);
                    } else if active && presenting && !self.presentation_frozen {
                        self.publish_surface(&id, generation);
                        update.repaint = true;
                    }
                }
            }
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id,
                request_id,
                final_chunk,
                data,
            } => {
                if self.pending.as_ref().is_some_and(|pending| {
                    pending.accepts_response(&id, generation, &boot_id, &request_id)
                }) {
                    if !final_chunk {
                        self.rollback(
                            "endpoint returned a chunked activation acknowledgement".into(),
                            false,
                            &mut update,
                        );
                    } else if let Some(pending) = self.pending.as_mut() {
                        progress = pending.receive_response_for_boot(
                            &id,
                            generation,
                            &boot_id,
                            &request_id,
                            &data,
                            &mut self.endpoints,
                        );
                    }
                } else if !request_id.starts_with("client-shell-surface:") {
                    match self.commands.receive_chunk(
                        &id,
                        generation,
                        &boot_id,
                        &request_id,
                        final_chunk,
                        data,
                    ) {
                        Ok(Some(result)) => update.completed.push(result),
                        Ok(None) => {}
                        Err(error) => self.disconnect(&id, generation, error, now, &mut update),
                    }
                }
            }
            ServerMessage::ServerShutdown { reason } => self.disconnect(
                &id,
                generation,
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    reason.unwrap_or_else(|| "server shut down".into()),
                ),
                now,
                &mut update,
            ),
            ServerMessage::SemanticNotification(notification) => {
                update.notifications.push(QualifiedHostEffect {
                    endpoint_id: id.clone(),
                    generation,
                    message: ServerMessage::SemanticNotification(notification),
                })
            }
            ServerMessage::ClientShellSnapshot(_) | ServerMessage::Welcome { .. } => self
                .disconnect(
                    &id,
                    generation,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unnegotiated endpoint handshake or binary snapshot",
                    ),
                    now,
                    &mut update,
                ),
            // Retirement cancels only its generation's assets, including while inactive.
            message @ ServerMessage::GraphicsTransmissionRetired { .. } => {
                update.host_effects.push(QualifiedHostEffect {
                    endpoint_id: id.clone(),
                    generation,
                    message,
                })
            }
            message if active && !self.presentation_frozen => {
                update.host_effects.push(QualifiedHostEffect {
                    endpoint_id: id.clone(),
                    generation,
                    message,
                })
            }
            _ => {}
        }
        self.progress(progress, now, &mut update);
        if accepted_snapshot
            && self.pending.is_none()
            && self.shell.endpoint_is_active(&id)
            && self.shell.endpoint_projection_available(&id)
            && (self.restore_selected.as_ref() == Some(&(id.clone(), generation))
                || (id.is_local() && self.deferred_local.is_some()))
        {
            let focus = if id.is_local() {
                self.deferred_local.take().flatten()
            } else {
                None
            };
            let next = self.activate(id.clone(), focus, now);
            update.repaint |= next.repaint;
            update.clear_host_effects |= next.clear_host_effects;
            update.cancelled.extend(next.cancelled);
            update.error = next.error.or(update.error);
        }
        if self.pending_allows_source_input() && self.endpoints.active_surface_available() {
            let active = self.endpoints.active_id().clone();
            update.cancelled.extend(
                self.commands
                    .send_next(&active, &mut self.endpoints)
                    .into_iter()
                    .map(|request_id| ResourceKey {
                        endpoint: active.clone(),
                        id: request_id,
                    }),
            );
        }
        update
    }

    fn publish_surface(&mut self, id: &ClientEndpointId, generation: u64) {
        let surface = self
            .shell
            .endpoint(id)
            .and_then(|endpoint| {
                endpoint
                    .cache
                    .coherent_surface(generation, self.options.surface_size)
            })
            .cloned();
        // Snapshot and surface arrive separately. Keep the last presentation until the
        // replacement pair is coherent; input_lease_current still rejects stale input.
        if self.shell.endpoint_is_active(id) && surface.is_some() {
            self.shell.pane_surface = surface;
        }
    }

    /// No switch is pending, or the pending switch still leaves the source in charge.
    fn pending_allows_source_input(&self) -> bool {
        self.pending
            .as_ref()
            .is_none_or(PendingEndpointActivation::retains_source)
    }

    pub(crate) fn awaiting_surface_pair(&self) -> bool {
        if self
            .refocus_hold
            .is_some_and(|since| since.elapsed() < REFOCUS_HOLD_LIMIT)
        {
            return true;
        }
        if !self.pending_allows_source_input() || self.presentation_frozen {
            return false;
        }
        let Some(displayed) = self.shell.pane_surface.as_ref() else {
            return false;
        };
        let Some(endpoint) = self.shell.endpoint(self.endpoints.active_id()) else {
            return false;
        };
        endpoint.generation.is_some_and(|generation| {
            endpoint
                .cache
                .live_snapshot(generation)
                .is_some_and(|snapshot| {
                    snapshot.boot_id == displayed.boot_id
                        && displayed.frame.width == self.options.surface_size.cols
                        && displayed.frame.height == self.options.surface_size.rows
                        && endpoint
                            .cache
                            .coherent_surface(generation, self.options.surface_size)
                            .is_none()
                })
        })
    }

    pub(crate) fn input_lease_current(&self) -> bool {
        if !self.endpoints.active_surface_available()
            || !self.pending_allows_source_input()
            || self.presentation_frozen
        {
            return false;
        }
        let id = self.endpoints.active_id();
        self.endpoints.connection(id).is_some_and(|connection| {
            self.shell
                .endpoint(id)
                .and_then(|endpoint| {
                    endpoint
                        .cache
                        .coherent_surface(connection.generation, self.options.surface_size)
                })
                .is_some_and(|surface| {
                    self.shell
                        .pane_surface
                        .as_ref()
                        .is_some_and(|shown| super::cache::same_surface(shown, surface))
                })
        })
    }

    pub(crate) fn input(
        &mut self,
        key: &ResourceKey,
        events: Vec<crate::protocol::endpoint_wire::ClientPaneInputEvent>,
    ) -> EndpointSendOutcome {
        if !self.input_lease_current() || self.endpoints.active_id() != &key.endpoint {
            tracing::debug!(endpoint = ?key.endpoint, lease_current = self.input_lease_current(), "endpoint input rejected before current presentation lease");
            return EndpointSendOutcome::NotSent;
        }
        let valid = self
            .endpoints
            .connection(&key.endpoint)
            .is_some_and(|connection| {
                self.shell
                    .endpoint(&key.endpoint)
                    .and_then(|endpoint| {
                        endpoint
                            .cache
                            .coherent_surface(connection.generation, self.options.surface_size)
                    })
                    .is_some_and(|surface| {
                        surface.popup.is_none()
                            && surface.panes.iter().any(|pane| pane.pane_id == key.id)
                    })
            });
        if !valid {
            return EndpointSendOutcome::NotSent;
        }
        self.endpoints.send(&ClientMessage::ClientShellPaneInput {
            pane_id: key.id.clone(),
            events,
        })
    }

    pub(crate) fn popup_input(
        &mut self,
        key: &ResourceKey,
        events: Vec<crate::protocol::endpoint_wire::ClientPaneInputEvent>,
    ) -> EndpointSendOutcome {
        if !self.input_lease_current() || self.endpoints.active_id() != &key.endpoint {
            return EndpointSendOutcome::NotSent;
        }
        let valid = self.shell.pane_surface.as_ref().is_some_and(|surface| {
            surface
                .popup
                .as_ref()
                .is_some_and(|popup| popup.terminal_id == key.id)
        });
        if !valid {
            return EndpointSendOutcome::NotSent;
        }
        self.endpoints.send(&ClientMessage::ClientShellPopupInput {
            terminal_id: key.id.clone(),
            events,
        })
    }

    pub(crate) fn clipboard_image(
        &mut self,
        key: &ResourceKey,
        popup: bool,
        image: crate::platform::ClipboardImage,
    ) -> EndpointSendOutcome {
        if !self.input_lease_current() || self.endpoints.active_id() != &key.endpoint {
            return EndpointSendOutcome::NotSent;
        }
        let valid = self.shell.pane_surface.as_ref().is_some_and(|surface| {
            if popup {
                surface
                    .popup
                    .as_ref()
                    .is_some_and(|p| p.terminal_id == key.id)
            } else {
                surface.popup.is_none() && surface.panes.iter().any(|p| p.pane_id == key.id)
            }
        });
        if !valid
            || image.bytes.is_empty()
            || image.bytes.len() > crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD
        {
            return EndpointSendOutcome::NotSent;
        }
        let target = if popup {
            crate::protocol::endpoint_wire::ClientClipboardImageTarget::Popup(key.id.clone())
        } else {
            crate::protocol::endpoint_wire::ClientClipboardImageTarget::Pane(key.id.clone())
        };
        self.endpoints.send(&ClientMessage::ClipboardImage {
            target,
            extension: image.extension.into(),
            data: image.bytes,
        })
    }

    pub(crate) fn issue_method(
        &mut self,
        method: crate::api::schema::Method,
    ) -> Result<RuntimeUpdate, String> {
        self.issue_method_with_id(method).map(|(_, update)| update)
    }

    pub(crate) fn issue_method_with_id(
        &mut self,
        method: crate::api::schema::Method,
    ) -> Result<(String, RuntimeUpdate), String> {
        self.next_serial = self
            .next_serial
            .checked_add(1)
            .ok_or_else(|| "endpoint request serial exhausted".to_owned())?;
        let request = EndpointCommandRequest::try_from(crate::api::schema::Request {
            id: format!("client-shell-command:{}", self.next_serial),
            method,
        })
        .map_err(|error| error.to_string())?;
        let id = request.id.clone();
        self.command(&self.endpoints.active_id().clone(), Box::new(request))
            .map(|update| (id, update))
    }

    pub(crate) fn command(
        &mut self,
        id: &ClientEndpointId,
        request: Box<EndpointCommandRequest>,
    ) -> Result<RuntimeUpdate, String> {
        if id != self.endpoints.active_id() || !self.input_lease_current() {
            return Err("the endpoint has no current input lease".into());
        }
        let connection = self
            .endpoints
            .connection(id)
            .ok_or_else(|| "endpoint is unavailable".to_owned())?;
        let (boot, _) = self
            .shell
            .endpoint_snapshot_identity(id, connection.generation)
            .ok_or_else(|| "endpoint snapshot is unavailable".to_owned())?;
        if !connection.negotiation.supports_method(&request.method) {
            return Err(format!("endpoint does not support {}", request.method));
        }
        self.commands
            .enqueue(id.clone(), connection.generation, boot.to_owned(), request);
        let cancelled = self
            .commands
            .send_next(id, &mut self.endpoints)
            .into_iter()
            .map(|request_id| ResourceKey {
                endpoint: id.clone(),
                id: request_id,
            })
            .collect();
        Ok(RuntimeUpdate {
            cancelled,
            ..RuntimeUpdate::default()
        })
    }

    /// Issue a method on a specific endpoint without requiring the active input
    /// lease. Decision answers target the machine that owns the pending
    /// decision, which may not be the presenting endpoint.
    pub(crate) fn command_on(
        &mut self,
        id: &ClientEndpointId,
        method: crate::api::schema::Method,
    ) -> Result<(String, RuntimeUpdate), String> {
        self.next_serial = self
            .next_serial
            .checked_add(1)
            .ok_or_else(|| "endpoint request serial exhausted".to_owned())?;
        let request = Box::new(
            EndpointCommandRequest::try_from(crate::api::schema::Request {
                id: format!("client-shell-command:{}", self.next_serial),
                method,
            })
            .map_err(|error| error.to_string())?,
        );
        let request_id = request.id.clone();
        let connection = self
            .endpoints
            .connection(id)
            .ok_or_else(|| "endpoint is unavailable".to_owned())?;
        let (boot, _) = self
            .shell
            .endpoint_snapshot_identity(id, connection.generation)
            .ok_or_else(|| "endpoint snapshot is unavailable".to_owned())?;
        if !connection.negotiation.supports_method(&request.method) {
            return Err(format!("endpoint does not support {}", request.method));
        }
        self.commands
            .enqueue(id.clone(), connection.generation, boot.to_owned(), request);
        let cancelled = self
            .commands
            .send_next(id, &mut self.endpoints)
            .into_iter()
            .map(|request_id| ResourceKey {
                endpoint: id.clone(),
                id: request_id,
            })
            .collect();
        Ok((
            request_id,
            RuntimeUpdate {
                cancelled,
                ..RuntimeUpdate::default()
            },
        ))
    }

    pub(crate) fn resize(
        &mut self,
        options: EndpointConnectOptions,
        _now: Instant,
    ) -> RuntimeUpdate {
        self.options = options;
        let mut update = RuntimeUpdate::default();
        let resize = self.resize_message();
        if let Some(pending) = self.pending.as_mut() {
            let retained = pending.retains_source().then(|| pending.source().clone());
            if retained.is_none() {
                self.presentation_frozen = true;
            }
            if let Err(error) = pending.update_resize(resize.clone(), &mut self.endpoints) {
                self.rollback(error, false, &mut update);
            }
            if let Some(source) = retained {
                self.endpoints.send_to(&source, &resize);
            }
            return update;
        }
        // A geometry change is not a handoff. The presenting endpoint redraws at the new size
        // and the previous frame stays on screen until that surface arrives.
        let active = self.endpoints.active_id().clone();
        if self
            .endpoints
            .connection(&active)
            .is_some_and(|connection| connection.surface_active)
        {
            self.endpoints.send_to(&active, &resize);
        }
        update
    }

    pub(crate) fn set_mouse_capture_preference(&mut self, enabled: bool) {
        self.options.mouse_capture = enabled;
        for endpoint in &self.shell.endpoints {
            if self.endpoints.connection(&endpoint.endpoint_id).is_some() {
                self.endpoints.send_to(
                    &endpoint.endpoint_id,
                    &ClientMessage::ClientShellMouseCapture { enabled },
                );
            }
        }
    }

    pub(crate) fn host_focus(&mut self, focused: bool) -> RuntimeUpdate {
        let regained = focused && self.shell.outer_focused != Some(true);
        self.shell.outer_focused = Some(focused);
        let mut update = RuntimeUpdate::default();
        if let Some(pending) = self.pending.as_mut() {
            let retained = pending.retains_source().then(|| pending.source().clone());
            if let Err(error) = pending.update_host_focus(focused, &mut self.endpoints) {
                self.rollback(error, false, &mut update);
            }
            if let Some(source) = retained {
                self.endpoints
                    .send_to(&source, &ClientMessage::ClientShellFocus { focused });
            }
        } else if self
            .endpoints
            .connection(self.endpoints.active_id())
            .is_some_and(|connection| connection.surface_active)
        {
            // Focus regain makes the server resend its viewer surface. That is a plain
            // presentation update on the same endpoint, not a handoff: keystrokes typed
            // right after returning to the window must keep reaching the focused pane.
            if regained {
                self.refocus_hold = Some(Instant::now());
            }
            self.endpoints
                .send(&ClientMessage::ClientShellFocus { focused });
        }
        update
    }

    pub(crate) fn tick(&mut self, now: Instant) -> RuntimeUpdate {
        let mut update = RuntimeUpdate::default();
        if self
            .refocus_hold
            .is_some_and(|since| now.duration_since(since) >= REFOCUS_HOLD_LIMIT)
        {
            self.refocus_hold = None;
            update.repaint = true;
        }
        self.endpoints.tick_health(now);
        for failure in self.endpoints.take_failures() {
            self.disconnect(
                &failure.endpoint_id,
                failure.generation,
                io::Error::new(failure.kind, failure.message),
                now,
                &mut update,
            );
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.expired(now))
        {
            self.rollback("endpoint activation timed out".into(), false, &mut update);
        }
        update.completed.extend(self.commands.expire(now));
        if self.pending.is_none() {
            if let Some(profiles) = self.deferred_catalog.take() {
                let catalog = self.apply_catalog(profiles, now);
                update.repaint |= catalog.repaint;
                update.clear_host_effects |= catalog.clear_host_effects;
                update.cancelled.extend(catalog.cancelled);
            }
        }
        self.supervisors
            .spawn_due(now, self.options, &self.supervisor_tx);
        update
    }
}

#[cfg(test)]
mod display_regression_tests {
    use super::*;
    use crate::protocol::endpoint_wire::{
        ClientShellSnapshot, ClientSurfaceSize, FrameData, PaneSurfaceFrame, SurfaceGraphicsScene,
    };

    #[test]
    fn endpoint_display_retains_pixels_between_snapshot_and_surface_without_authority() {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        let size = ClientSurfaceSize { cols: 80, rows: 24 };
        let options = EndpointConnectOptions {
            surface_size: size,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        };
        let mut runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        let id = ClientEndpointId::Local;
        assert!(runtime.shell.begin_connection(&id, 1));
        assert!(runtime.shell.receive_snapshot(&id, 1, snapshot.clone()));
        let mut buffer =
            ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, size.cols, size.rows));
        buffer[(0, 0)].set_symbol("A");
        let mut surface = PaneSurfaceFrame {
            boot_id: snapshot.boot_id.clone(),
            projection_revision: snapshot.revision,
            surface_revision: 1,
            frame: FrameData::from_ratatui_buffer(&buffer, None),
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        };
        assert!(runtime.shell.receive_surface(&id, 1, surface.clone()));
        runtime.publish_surface(&id, 1);
        assert_eq!(runtime.shell.pane_surface.as_ref(), Some(&surface));
        runtime.presentation_frozen = false;
        assert!(!runtime.awaiting_surface_pair());
        snapshot.revision += 1;
        assert!(runtime.shell.receive_snapshot(&id, 1, snapshot.clone()));
        runtime.publish_surface(&id, 1);
        assert_eq!(runtime.shell.pane_surface.as_ref(), Some(&surface));
        assert!(runtime
            .shell
            .endpoint(&id)
            .unwrap()
            .cache
            .coherent_surface(1, size)
            .is_none());
        assert!(!runtime.shell.endpoint_surface_matches(&id, 1, &surface));
        assert!(runtime.awaiting_surface_pair());
        surface.projection_revision = snapshot.revision;
        surface.surface_revision += 1;
        surface.frame.cells[0].symbol = "B".into();
        assert!(runtime.shell.receive_surface(&id, 1, surface.clone()));
        runtime.publish_surface(&id, 1);
        assert_eq!(runtime.shell.pane_surface.as_ref(), Some(&surface));
        assert!(runtime.shell.endpoint_surface_matches(&id, 1, &surface));
        assert!(!runtime.awaiting_surface_pair());
        let previous = surface.clone();
        surface.projection_revision += 1;
        surface.surface_revision += 1;
        assert!(runtime.shell.receive_surface(&id, 1, surface.clone()));
        runtime.publish_surface(&id, 1);
        assert_eq!(runtime.shell.pane_surface.as_ref(), Some(&previous));
        assert!(runtime.awaiting_surface_pair());
        snapshot.revision = surface.projection_revision;
        assert!(runtime.shell.receive_snapshot(&id, 1, snapshot));
        runtime.publish_surface(&id, 1);
        assert_eq!(runtime.shell.pane_surface.as_ref(), Some(&surface));
        assert!(!runtime.awaiting_surface_pair());
        assert!(runtime.shell.disconnect(&id, 1));
        assert!(!runtime.awaiting_surface_pair());
        assert!(runtime.shell.pane_surface.is_none());
        assert!(runtime.shell.begin_connection(&id, 2));
        assert!(!runtime.shell.receive_surface(&id, 1, surface));
        runtime.publish_surface(&id, 2);
        assert!(runtime.shell.pane_surface.is_none());
    }

    struct Recording(std::sync::Arc<std::sync::Mutex<Vec<ClientMessage>>>);

    impl super::super::EndpointTransport for Recording {
        fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
            self.0.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn negotiation() -> super::super::EndpointNegotiation {
        super::super::EndpointNegotiation::new(
            vec!["client_shell.surface.set".into()],
            vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
            ],
        )
    }

    fn surface_for(snapshot: &ClientShellSnapshot, size: ClientSurfaceSize) -> PaneSurfaceFrame {
        let buffer =
            ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, size.cols, size.rows));
        PaneSurfaceFrame {
            boot_id: snapshot.boot_id.clone(),
            projection_revision: snapshot.revision,
            surface_revision: 1,
            frame: FrameData::from_ratatui_buffer(&buffer, None),
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn switching_to_an_unresponsive_machine_keeps_the_source_presenting_and_typing() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        let size = ClientSurfaceSize { cols: 80, rows: 24 };
        let options = EndpointConnectOptions {
            surface_size: size,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        };
        let now = Instant::now();
        let mut runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], now),
            options,
        );
        let local = ClientEndpointId::Local;
        let remote = ClientEndpointId::Ssh("mini".into());
        runtime
            .shell
            .set_endpoint_catalog(&[crate::machine::MachineProfile {
                id: "mini".into(),
                label: "Mac mini".into(),
                target: "no-ssh".into(),
                session: "remote".into(),
                enabled: true,
            }]);
        let local_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let remote_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for (id, sent, active) in [(&local, &local_sent, true), (&remote, &remote_sent, false)] {
            assert!(runtime.shell.begin_connection(id, 1));
            assert!(runtime.shell.receive_snapshot(id, 1, snapshot.clone()));
            assert!(runtime
                .shell
                .receive_surface(id, 1, surface_for(&snapshot, size)));
            runtime.endpoints.insert(
                id.clone(),
                Recording(sent.clone()),
                1,
                negotiation(),
                active,
            );
        }
        assert!(runtime.endpoints.set_active(&local));
        runtime.endpoints.unfreeze_input();
        runtime.presentation_frozen = false;
        runtime.publish_surface(&local, 1);
        assert!(runtime.input_lease_current());

        let update = runtime.activate(remote.clone(), None, now);
        assert!(update.error.is_none());
        let pending = runtime.pending.as_ref().expect("switch is pending");
        assert!(pending.retains_source());
        // Local still owns the screen and the keyboard while the machine has not answered.
        assert!(runtime.input_lease_current());
        assert!(!local_sent.lock().unwrap().iter().any(|message| matches!(
            message,
            ClientMessage::ClientShellFocus { focused: false }
                | ClientMessage::ClientShellEndpointRequest { .. }
        )));
        assert!(remote_sent
            .lock()
            .unwrap()
            .iter()
            .any(|message| matches!(message, ClientMessage::ClientShellEndpointRequest { .. })));

        // The machine never answers: the switch is abandoned and Local was never interrupted.
        let update = runtime.tick(now + std::time::Duration::from_secs(6));
        assert!(runtime.pending.is_none());
        assert!(runtime.input_lease_current());
        assert_eq!(runtime.shell.active_endpoint_id, local);
        assert!(update
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("Mac mini did not respond")));
        assert!(runtime
            .shell
            .endpoint(&remote)
            .and_then(|endpoint| endpoint.diagnostic.as_deref())
            .is_some());
    }

    #[test]
    fn a_resize_is_sent_to_the_presenting_endpoint_without_a_handoff() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        let size = ClientSurfaceSize { cols: 80, rows: 24 };
        let mut options = EndpointConnectOptions {
            surface_size: size,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        };
        let now = Instant::now();
        let mut runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], now),
            options,
        );
        let local = ClientEndpointId::Local;
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        assert!(runtime.shell.begin_connection(&local, 1));
        assert!(runtime.shell.receive_snapshot(&local, 1, snapshot.clone()));
        assert!(runtime
            .shell
            .receive_surface(&local, 1, surface_for(&snapshot, size)));
        runtime.endpoints.insert(
            local.clone(),
            Recording(sent.clone()),
            1,
            negotiation(),
            true,
        );
        assert!(runtime.endpoints.set_active(&local));
        runtime.endpoints.unfreeze_input();
        runtime.presentation_frozen = false;
        runtime.publish_surface(&local, 1);

        options.surface_size = ClientSurfaceSize { cols: 70, rows: 24 };
        runtime.resize(options, now);
        assert!(runtime.pending.is_none());
        assert!(!runtime.presentation_frozen);
        // The previous frame stays presented until the endpoint answers at the new size.
        assert!(runtime.shell.pane_surface.is_some());
        let sent = sent.lock().unwrap();
        assert!(matches!(
            sent.as_slice(),
            [ClientMessage::ClientShellResize { surface_size, .. }]
                if *surface_size == options.surface_size
        ));
    }

    #[test]
    fn interim_endpoint_status_updates_the_machine_without_resolving_the_attempt() {
        let options = EndpointConnectOptions {
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        };
        let now = Instant::now();
        let profile = crate::machine::MachineProfile {
            id: "mini".into(),
            label: "Mac mini".into(),
            target: "tailscaled-host".into(),
            session: "saved".into(),
            enabled: true,
        };
        let remote = ClientEndpointId::Ssh("mini".into());
        let mut supervisors = EndpointSupervisors::new(std::slice::from_ref(&profile), now);
        supervisors.mark_attempt_in_flight_for_test(&remote, 7);
        let mut runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            supervisors,
            options,
        );
        runtime.shell.set_endpoint_catalog(&[profile]);

        // A stale interim report for a retired or superseded attempt is ignored.
        let update = runtime.supervisor_event(
            EndpointSupervisorEvent::Status {
                endpoint_id: remote.clone(),
                generation: 6,
                status: ClientEndpointStatus::Reconnecting,
                message: "stale".into(),
                interim: true,
            },
            now,
        );
        assert!(!update.repaint);
        assert!(update.error.is_none());
        assert!(runtime
            .shell
            .endpoint(&remote)
            .and_then(|endpoint| endpoint.diagnostic.as_deref())
            .is_none());

        // The live attempt's interim report updates its status and diagnostic,
        // including the full Tailscale SSH approval URL.
        let url = "https://login.tailscale.com/a/abc123";
        let update = runtime.supervisor_event(
            EndpointSupervisorEvent::Status {
                endpoint_id: remote.clone(),
                generation: 7,
                status: ClientEndpointStatus::Reconnecting,
                message: format!("waiting for Tailscale SSH approval: {url}"),
                interim: true,
            },
            now,
        );
        assert!(update.repaint);
        assert!(update.error.is_none());
        let endpoint = runtime.shell.endpoint(&remote).unwrap();
        assert_eq!(endpoint.status, ClientEndpointStatus::Reconnecting);
        assert_eq!(
            endpoint.diagnostic.as_deref(),
            Some(format!("waiting for Tailscale SSH approval: {url}").as_str())
        );
        // The attempt is still in flight; no duplicate retry was scheduled.
        assert!(runtime.supervisors.is_current_attempt(&remote, 7));

        // The attempt's final status still resolves the bookkeeping normally.
        let update = runtime.supervisor_event(
            EndpointSupervisorEvent::Status {
                endpoint_id: remote.clone(),
                generation: 7,
                status: ClientEndpointStatus::Reconnecting,
                message: "still offline".into(),
                interim: false,
            },
            now,
        );
        assert!(update.repaint);
        assert!(!runtime.supervisors.is_current_attempt(&remote, 7));
    }

    #[test]
    fn awaiting_approval_status_parks_and_a_machine_click_retries_it() {
        let options = EndpointConnectOptions {
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        };
        let now = Instant::now();
        let profile = crate::machine::MachineProfile {
            id: "mini".into(),
            label: "Mac mini".into(),
            target: "tailscaled-host".into(),
            session: "saved".into(),
            enabled: true,
        };
        let remote = ClientEndpointId::Ssh("mini".into());
        let mut supervisors = EndpointSupervisors::new(std::slice::from_ref(&profile), now);
        supervisors.mark_attempt_in_flight_for_test(&remote, 7);
        let mut runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            supervisors,
            options,
        );
        runtime.shell.set_endpoint_catalog(&[profile]);

        // The unapproved wait's final status parks the machine, keeps the
        // timeout detail with it, and announces the needed user action.
        let message = "noninteractive SSH command timed out waiting for Tailscale SSH approval; click the machine to retry (last approval URL: https://login.tailscale.com/a/parked)";
        let update = runtime.supervisor_event(
            EndpointSupervisorEvent::Status {
                endpoint_id: remote.clone(),
                generation: 7,
                status: ClientEndpointStatus::AwaitingApproval,
                message: message.to_owned(),
                interim: false,
            },
            now,
        );
        assert!(update.repaint);
        assert_eq!(
            update.error.as_deref(),
            Some(format!("Mac mini: {message}").as_str())
        );
        let endpoint = runtime.shell.endpoint(&remote).unwrap();
        assert_eq!(endpoint.status, ClientEndpointStatus::AwaitingApproval);
        assert_eq!(endpoint.diagnostic.as_deref(), Some(message));
        assert_eq!(
            runtime.supervisors.next_attempt_for_test(&remote),
            None,
            "an unapproved approval wait must not auto-reconnect"
        );

        // A machine in any other status keeps the click's collapse-toggle
        // meaning (the click path returns None and the caller toggles).
        runtime
            .shell
            .set_endpoint_status(&remote, ClientEndpointStatus::Reconnecting);
        assert!(runtime.retry_parked_endpoint(&remote, now).is_none());

        // Park it again, then the click schedules an immediate retry and shows
        // the machine as reconnecting until the new attempt reports.
        runtime
            .shell
            .set_endpoint_status(&remote, ClientEndpointStatus::AwaitingApproval);
        let update = runtime
            .retry_parked_endpoint(&remote, now)
            .expect("a parked machine answers a click with a retry");
        assert!(update.repaint);
        assert_eq!(
            runtime.supervisors.next_attempt_for_test(&remote),
            Some(now)
        );
        assert_eq!(
            runtime.shell.endpoint(&remote).unwrap().status,
            ClientEndpointStatus::Reconnecting
        );
    }
}
