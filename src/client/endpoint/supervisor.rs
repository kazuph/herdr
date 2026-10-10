// Adapted from fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::handshake::EndpointConnectOptions;
use super::{ClientEndpointId, ClientEndpointStatus, EndpointNegotiation};

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(120);
const MAX_LOCAL_RETRY_DELAY: Duration = Duration::from_secs(30);
const STABLE_CONNECTION_PERIOD: Duration = Duration::from_secs(60);

pub(crate) enum EndpointSupervisorEvent {
    Status {
        endpoint_id: ClientEndpointId,
        generation: u64,
        status: ClientEndpointStatus,
        message: String,
        /// `interim` updates the machine's display while its connection attempt
        /// is still running; it does not resolve the attempt's retry bookkeeping.
        interim: bool,
    },
    Connected {
        endpoint_id: ClientEndpointId,
        generation: u64,
        stream: crate::ipc::LocalStream,
        lifetime: Box<dyn Send>,
        negotiation: EndpointNegotiation,
    },
}

#[derive(Clone)]
enum ConnectTarget {
    Local(PathBuf),
    Ssh(crate::machine::MachineProfile),
}

struct ReconnectState {
    target: ConnectTarget,
    attempts: u32,
    next_attempt: Option<Instant>,
    in_flight: bool,
    generation: Option<u64>,
    online_since: Option<Instant>,
    /// Cancels the in-flight attempt's blocking ssh work (for example a
    /// Tailscale SSH approval wait) when the endpoint retires or the client
    /// shuts down, so no ssh child is left behind.
    cancel: Option<Arc<AtomicBool>>,
}

impl ReconnectState {
    fn new(target: ConnectTarget, now: Instant) -> Self {
        Self {
            target,
            attempts: 0,
            next_attempt: Some(now),
            in_flight: false,
            generation: None,
            online_since: None,
            cancel: None,
        }
    }
}

pub(crate) struct EndpointSupervisors {
    endpoints: HashMap<ClientEndpointId, ReconnectState>,
    next_generation: u64,
    shutdown: Arc<AtomicBool>,
}

impl EndpointSupervisors {
    pub(crate) fn new(profiles: &[crate::machine::MachineProfile], now: Instant) -> Self {
        let endpoints = profiles
            .iter()
            .filter(|profile| profile.enabled)
            .map(|profile| {
                (
                    ClientEndpointId::Ssh(profile.id.clone()),
                    ReconnectState::new(ConnectTarget::Ssh(profile.clone()), now),
                )
            })
            .collect();
        Self {
            endpoints,
            next_generation: 2,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn add_local(&mut self, path: PathBuf, generation: Option<u64>, now: Instant) {
        let mut state = ReconnectState::new(ConnectTarget::Local(path), now);
        state.generation = generation;
        if generation.is_some() {
            state.next_attempt = None;
        }
        self.endpoints.insert(ClientEndpointId::Local, state);
    }

    pub(crate) fn reconcile_profiles(
        &mut self,
        profiles: &[crate::machine::MachineProfile],
        now: Instant,
    ) -> Vec<ClientEndpointId> {
        let mut retired = Vec::new();
        self.endpoints.retain(|endpoint_id, state| {
            let ConnectTarget::Ssh(previous) = &state.target else {
                return true;
            };
            let keep = profiles.iter().any(|profile| {
                profile.id == previous.id
                    && profile.enabled
                    && profile.target == previous.target
                    && profile.session == previous.session
            });
            if !keep {
                retired.push(endpoint_id.clone());
                // An attempt still running for the retired endpoint must kill
                // its ssh child rather than waiting out an approval deadline.
                if let Some(cancel) = state.cancel.take() {
                    cancel.store(true, Ordering::Release);
                }
            }
            keep
        });
        for profile in profiles.iter().filter(|profile| profile.enabled) {
            let state = self
                .endpoints
                .entry(ClientEndpointId::Ssh(profile.id.clone()))
                .or_insert_with(|| ReconnectState::new(ConnectTarget::Ssh(profile.clone()), now));
            state.target = ConnectTarget::Ssh(profile.clone());
        }
        retired
    }

    pub(crate) fn spawn_due(
        &mut self,
        now: Instant,
        options: EndpointConnectOptions,
        event_tx: &tokio::sync::mpsc::Sender<EndpointSupervisorEvent>,
    ) {
        for (endpoint_id, state) in &mut self.endpoints {
            if state.in_flight || state.next_attempt.is_none_or(|deadline| deadline > now) {
                continue;
            }
            state.in_flight = true;
            state.next_attempt = None;
            let generation = self.next_generation;
            state.generation = Some(generation);
            self.next_generation = self.next_generation.saturating_add(1);
            let endpoint_id = endpoint_id.clone();
            let target = state.target.clone();
            let event_tx = event_tx.clone();
            let shutdown = self.shutdown.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            state.cancel = Some(Arc::clone(&cancel));
            tokio::spawn(async move {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                let task_endpoint_id = endpoint_id.clone();
                let progress_tx = event_tx.clone();
                let result = tokio::task::spawn_blocking(move || {
                    connect_once(
                        &target,
                        options,
                        endpoint_id,
                        generation,
                        &cancel,
                        &progress_tx,
                    )
                })
                .await;
                let event = match result {
                    Ok(Ok(event)) => event,
                    Ok(Err(error)) => EndpointSupervisorEvent::Status {
                        endpoint_id: task_endpoint_id,
                        generation,
                        status: if crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(
                            &error,
                        ) {
                            ClientEndpointStatus::AwaitingApproval
                        } else if failure_needs_attention(&error) {
                            ClientEndpointStatus::Attention
                        } else {
                            ClientEndpointStatus::Reconnecting
                        },
                        message: error.to_string(),
                        interim: false,
                    },
                    Err(error) => EndpointSupervisorEvent::Status {
                        endpoint_id: task_endpoint_id,
                        generation,
                        status: ClientEndpointStatus::Reconnecting,
                        message: format!("endpoint connection task stopped unexpectedly: {error}"),
                        interim: false,
                    },
                };
                if !shutdown.load(Ordering::Acquire) {
                    let _ = event_tx.send(event).await;
                }
            });
        }
    }

    pub(crate) fn record_status(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        status: ClientEndpointStatus,
        now: Instant,
    ) -> bool {
        let Some(state) = self.endpoints.get_mut(endpoint_id) else {
            return false;
        };
        if state.generation != Some(generation) {
            return false;
        }
        state.in_flight = false;
        state.cancel = None;
        match status {
            ClientEndpointStatus::Online => {
                if endpoint_id.is_local() {
                    state.attempts = 0;
                }
                state.online_since.get_or_insert(now);
                state.next_attempt = None;
            }
            ClientEndpointStatus::Attention => {
                state.online_since = None;
                // Authentication or configuration may be repaired outside this client.
                state.next_attempt =
                    (!endpoint_id.is_local()).then_some(now + Duration::from_secs(30));
            }
            ClientEndpointStatus::AwaitingApproval | ClientEndpointStatus::Disabled => {
                // The unapproved Tailscale wait and a disabled machine both park:
                // only a manual retry (machine click or profile toggle) resumes
                // the endpoint, so a parked wait cannot mint tab after tab.
                state.online_since = None;
                state.next_attempt = None;
            }
            ClientEndpointStatus::Connecting | ClientEndpointStatus::Reconnecting => {
                // A brief maintenance wake can complete a handshake without restoring the link.
                if state.online_since.take().is_some_and(|connected| {
                    now.saturating_duration_since(connected) >= STABLE_CONNECTION_PERIOD
                }) {
                    state.attempts = 0;
                }
                state.attempts = state.attempts.saturating_add(1);
                let delay = retry_delay(state.attempts);
                state.next_attempt = Some(
                    now + if endpoint_id.is_local() {
                        delay.min(MAX_LOCAL_RETRY_DELAY)
                    } else {
                        delay
                    },
                );
            }
        }
        true
    }

    pub(crate) fn disconnected(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        now: Instant,
    ) -> bool {
        self.record_status(
            endpoint_id,
            generation,
            ClientEndpointStatus::Reconnecting,
            now,
        )
    }

    /// Schedules an immediate new attempt for a parked endpoint (a machine
    /// whose Tailscale approval wait ended unapproved and the user asked to
    /// retry). Returns false when an attempt is already running or the
    /// endpoint is unknown.
    pub(crate) fn retry(&mut self, endpoint_id: &ClientEndpointId, now: Instant) -> bool {
        let Some(state) = self.endpoints.get_mut(endpoint_id) else {
            return false;
        };
        if state.in_flight {
            return false;
        }
        state.attempts = 0;
        state.next_attempt = Some(now);
        true
    }

    /// True while the generation's connection attempt is still running; interim
    /// status reports apply only to the live attempt so retired or superseded
    /// work cannot move the machine's status display.
    pub(crate) fn is_current_attempt(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> bool {
        self.endpoints
            .get(endpoint_id)
            .is_some_and(|state| state.in_flight && state.generation == Some(generation))
    }

    /// The next scheduled attempt, exposed so parked-versus-retrying behavior
    /// can be asserted from the runtime layer.
    #[cfg(test)]
    pub(crate) fn next_attempt_for_test(&self, endpoint_id: &ClientEndpointId) -> Option<Instant> {
        self.endpoints
            .get(endpoint_id)
            .and_then(|state| state.next_attempt)
    }

    /// Marks an attempt in flight so interim-status handling can be tested
    /// without spawning a real connection task.
    #[cfg(test)]
    pub(crate) fn mark_attempt_in_flight_for_test(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) {
        if let Some(state) = self.endpoints.get_mut(endpoint_id) {
            state.in_flight = true;
            state.generation = Some(generation);
        }
    }
}

impl Drop for EndpointSupervisors {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for state in self.endpoints.values_mut() {
            if let Some(cancel) = state.cancel.take() {
                cancel.store(true, Ordering::Release);
            }
        }
    }
}

fn connect_once(
    target: &ConnectTarget,
    options: EndpointConnectOptions,
    endpoint_id: ClientEndpointId,
    generation: u64,
    cancel: &AtomicBool,
    progress: &tokio::sync::mpsc::Sender<EndpointSupervisorEvent>,
) -> Result<EndpointSupervisorEvent, std::io::Error> {
    let (mut stream, lifetime): (_, Box<dyn Send>) = match target {
        ConnectTarget::Local(path) => {
            let stream = crate::ipc::connect_local_stream(path).map_err(|error| {
                // An absent Local socket is transient, unlike a missing SSH install.
                if error.kind() == std::io::ErrorKind::NotFound {
                    std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "Local is unavailable; start its server to reconnect",
                    )
                } else {
                    error
                }
            })?;
            (stream, Box::new(()))
        }
        ConnectTarget::Ssh(profile) => {
            let progress_endpoint_id = endpoint_id.clone();
            // An in-flight attempt reports interim detail (for example a
            // Tailscale SSH approval URL) without resolving its own bookkeeping.
            let status = move |message: &str| {
                let _ = progress.try_send(EndpointSupervisorEvent::Status {
                    endpoint_id: progress_endpoint_id.clone(),
                    generation,
                    status: ClientEndpointStatus::Reconnecting,
                    message: message.to_owned(),
                    interim: true,
                });
            };
            crate::remote::connect_saved_ssh(
                &profile.target,
                &profile.session,
                &crate::remote::SavedSshHooks {
                    profile_id: profile.id.as_str(),
                    cancel,
                    status: &status,
                },
            )?
        }
    };
    let handshake = super::handshake::connect(
        &mut stream,
        EndpointConnectOptions {
            surface_active: false,
            ..options
        },
        matches!(target, ConnectTarget::Local(_)),
    )?;
    let negotiation = EndpointNegotiation::new(handshake.methods, handshake.capabilities);
    if !negotiation.supports_surface_interest()
        || (!endpoint_id.is_local() && !negotiation.supports_health_check())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this machine needs a server update before it can participate in multi-machine viewing",
        ));
    }
    Ok(EndpointSupervisorEvent::Connected {
        endpoint_id,
        generation,
        stream,
        lifetime,
        negotiation,
    })
}

fn failure_needs_attention(error: &std::io::Error) -> bool {
    crate::remote::saved_ssh_failure_needs_attention(error)
}

fn retry_delay(attempt: u32) -> Duration {
    INITIAL_RETRY_DELAY
        .saturating_mul(
            1_u32
                .checked_shl(attempt.saturating_sub(1).min(8))
                .unwrap_or(u32::MAX),
        )
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str) -> crate::machine::MachineProfile {
        crate::machine::MachineProfile {
            id: id.into(),
            label: id.into(),
            target: "no-ssh".into(),
            session: "saved".into(),
            enabled: true,
        }
    }

    #[test]
    fn endpoint_supervisor_catalog_changes_retire_only_changed_destinations_and_fence_old_generations(
    ) {
        let now = Instant::now();
        let mut first = profile("m-fork-id");
        let second = profile("opaque/id");
        let a = ClientEndpointId::Ssh(first.id.clone());
        let b = ClientEndpointId::Ssh(second.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[first.clone(), second.clone()], now);
        supervisors.add_local(PathBuf::from("local.sock"), Some(1), now);
        supervisors.endpoints.get_mut(&a).unwrap().generation = Some(2);
        supervisors.endpoints.get_mut(&b).unwrap().generation = Some(3);
        first.label = "Renamed".into();
        assert!(supervisors
            .reconcile_profiles(&[first.clone(), second.clone()], now)
            .is_empty());
        assert_eq!(supervisors.endpoints[&a].generation, Some(2));
        first.session = "another".into();
        assert_eq!(
            supervisors.reconcile_profiles(&[first.clone(), second.clone()], now),
            vec![a.clone()]
        );
        assert!(!supervisors.record_status(&a, 2, ClientEndpointStatus::Online, now));
        assert_eq!(supervisors.endpoints[&b].generation, Some(3));
        first.enabled = false;
        assert_eq!(
            supervisors.reconcile_profiles(&[first.clone(), second.clone()], now),
            vec![a.clone()]
        );
        first.enabled = true;
        supervisors.reconcile_profiles(&[first, second], now);
        assert!(!supervisors.disconnected(&a, 2, now));
        assert_eq!(
            supervisors.endpoints[&ClientEndpointId::Local].generation,
            Some(1)
        );
    }

    #[test]
    fn endpoint_supervisor_flapping_backoff_attention_and_local_recovery_are_independent() {
        let now = Instant::now();
        let profile = profile("m-fork-id");
        let remote = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        supervisors.add_local(PathBuf::from("local.sock"), Some(1), now);
        supervisors.endpoints.get_mut(&remote).unwrap().generation = Some(2);
        let brief = STABLE_CONNECTION_PERIOD / 2;
        let mut current = now;
        for attempt in 1..=5 {
            assert!(supervisors.record_status(&remote, 2, ClientEndpointStatus::Online, current));
            current += brief;
            assert!(supervisors.disconnected(&remote, 2, current));
            assert_eq!(
                supervisors.endpoints[&remote].next_attempt,
                Some(current + retry_delay(attempt))
            );
        }
        assert!(supervisors.record_status(&remote, 2, ClientEndpointStatus::Online, current));
        current += STABLE_CONNECTION_PERIOD;
        assert!(supervisors.disconnected(&remote, 2, current));
        assert_eq!(
            supervisors.endpoints[&remote].next_attempt,
            Some(current + INITIAL_RETRY_DELAY)
        );
        assert!(!supervisors.disconnected(&remote, 1, current));
        assert!(supervisors.record_status(&remote, 2, ClientEndpointStatus::Attention, current));
        assert_eq!(
            supervisors.endpoints[&remote].next_attempt,
            Some(current + Duration::from_secs(30))
        );
        assert!(supervisors.endpoints[&ClientEndpointId::Local]
            .next_attempt
            .is_none());
        assert!(supervisors.disconnected(&ClientEndpointId::Local, 1, current));
        assert_eq!(
            supervisors.endpoints[&ClientEndpointId::Local].next_attempt,
            Some(current + INITIAL_RETRY_DELAY)
        );
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    #[cfg(unix)]
    fn options() -> EndpointConnectOptions {
        EndpointConnectOptions {
            surface_size: crate::protocol::endpoint_wire::ClientSurfaceSize { cols: 80, rows: 24 },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
        }
    }

    /// One endpoint parked in a Tailscale SSH approval wait must not stop
    /// another endpoint's connection attempt, and retiring the waiting
    /// endpoint kills its ssh child.
    #[cfg(unix)]
    #[tokio::test]
    async fn another_endpoint_progresses_while_one_waits_for_tailscale_approval() {
        let fake = crate::remote::test_fakes::FakeSsh::new("supervisor-concurrent")
            .with_banner("https://login.tailscale.com/a/concurrent")
            .with_approve_gate()
            .with_env_text("FAKE_SSH_FAIL_TARGET", "fail-host");
        let now = Instant::now();
        let mut waiting = profile("waiting");
        waiting.target = "gate-host".into();
        let mut fast = profile("fast");
        fast.target = "fail-host".into();
        let waiting_id = ClientEndpointId::Ssh("waiting".into());
        let fast_id = ClientEndpointId::Ssh("fast".into());
        let mut supervisors = EndpointSupervisors::new(&[waiting, fast.clone()], now);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        supervisors.spawn_due(now, options(), &tx);

        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            let mut fast_reported = false;
            let mut waiting_auth = false;
            while !(fast_reported && waiting_auth) {
                match rx.recv().await {
                    Some(EndpointSupervisorEvent::Status {
                        endpoint_id,
                        interim,
                        message,
                        ..
                    }) => {
                        if endpoint_id == fast_id {
                            fast_reported = true;
                        }
                        if endpoint_id == waiting_id
                            && interim
                            && message.contains("https://login.tailscale.com/a/concurrent")
                        {
                            waiting_auth = true;
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            (fast_reported, waiting_auth)
        })
        .await;
        let Ok((fast_reported, waiting_auth)) = outcome else {
            panic!("endpoint progress stalled during the auth wait: {outcome:?}");
        };
        assert!(
            fast_reported,
            "the healthy endpoint made no progress during the auth wait"
        );
        assert!(
            waiting_auth,
            "the auth-waiting endpoint never reported its interim status"
        );
        assert!(supervisors.endpoints[&waiting_id].in_flight);

        // Retiring the waiting endpoint cancels the attempt and kills its ssh.
        let pids = fake.wait_for_pid(Duration::from_secs(10));
        assert!(!pids.is_empty(), "the waiting endpoint never spawned ssh");
        supervisors.reconcile_profiles(&[fast], now);
        assert!(
            fake.all_dead(Duration::from_secs(5)),
            "retire left an ssh child behind: {pids:?}"
        );
    }

    /// An unapproved Tailscale approval wait ends in AwaitingApproval with no
    /// automatic retry scheduled; a manual retry starts a fresh attempt whose
    /// banner opens the browser exactly once more.
    #[cfg(unix)]
    #[tokio::test]
    async fn unapproved_tailscale_wait_parks_until_a_manual_retry() {
        let fake = crate::remote::test_fakes::FakeSsh::new("supervisor-parked")
            .with_banner("https://login.tailscale.com/a/parked")
            .with_approve_gate();
        crate::remote::test_hooks::set_auth_wait(Some(Duration::from_millis(300)));
        let opened: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        crate::remote::test_hooks::set_open_url_hook(Some(std::sync::Arc::new({
            let opened = std::sync::Arc::clone(&opened);
            move |url: &str| {
                opened
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(url.to_owned());
                Ok(())
            }
        })));
        let now = Instant::now();
        let mut waiting = profile("waiting");
        waiting.target = "gate-host".into();
        let waiting_id = ClientEndpointId::Ssh("waiting".into());
        let mut supervisors = EndpointSupervisors::new(&[waiting], now);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        supervisors.spawn_due(now, options(), &tx);

        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match rx.recv().await {
                    Some(EndpointSupervisorEvent::Status {
                        endpoint_id,
                        generation,
                        status,
                        interim: false,
                        ..
                    }) if endpoint_id == waiting_id => break (generation, status),
                    Some(_) => {}
                    None => panic!("event channel closed before the final status"),
                }
            }
        })
        .await;
        let Ok((generation, status)) = outcome else {
            panic!("no final status arrived: {outcome:?}");
        };
        assert_eq!(status, ClientEndpointStatus::AwaitingApproval);
        assert!(supervisors.record_status(&waiting_id, generation, status, now));
        assert_eq!(
            supervisors.endpoints[&waiting_id].next_attempt, None,
            "an unapproved approval wait must not schedule an automatic retry"
        );
        // Even far past the wait, nothing spawns on its own and the parked wait
        // opened the URL exactly once.
        supervisors.spawn_due(now + Duration::from_secs(60), options(), &tx);
        assert_eq!(
            opened
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            1
        );

        // The manual retry starts a fresh attempt whose wait opens the new URL
        // exactly once, then parks the same way.
        assert!(supervisors.retry(&waiting_id, now));
        supervisors.spawn_due(now, options(), &tx);
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match rx.recv().await {
                    Some(EndpointSupervisorEvent::Status {
                        endpoint_id,
                        status,
                        interim: false,
                        ..
                    }) if endpoint_id == waiting_id => break status,
                    Some(_) => {}
                    None => panic!("event channel closed before the retry status"),
                }
            }
        })
        .await;
        assert!(
            matches!(outcome, Ok(ClientEndpointStatus::AwaitingApproval)),
            "the retry attempt did not park again: {outcome:?}"
        );
        assert_eq!(
            opened
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            [
                "https://login.tailscale.com/a/parked",
                "https://login.tailscale.com/a/parked"
            ]
        );
        assert!(
            fake.all_dead(Duration::from_secs(5)),
            "parked attempts left an ssh child behind"
        );
        drop(rx);
    }

    /// Dropping the supervisors (client shutdown) cancels every in-flight
    /// attempt, so no auth-waiting ssh process survives.
    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_shutdown_kills_a_waiting_tailscale_ssh_child() {
        let fake = crate::remote::test_fakes::FakeSsh::new("supervisor-shutdown")
            .with_banner("https://login.tailscale.com/a/shutdown")
            .with_approve_gate();
        let now = Instant::now();
        let mut waiting = profile("waiting");
        waiting.target = "gate-host".into();
        let mut supervisors = EndpointSupervisors::new(&[waiting], now);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        supervisors.spawn_due(now, options(), &tx);
        // Wait until the ssh child is inside its approval wait.
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match rx.recv().await {
                    Some(EndpointSupervisorEvent::Status { interim: true, .. }) => break,
                    Some(_) => {}
                    None => panic!("event channel closed before the interim status"),
                }
            }
        })
        .await;
        assert!(outcome.is_ok(), "no interim auth-wait status arrived");
        let pids = fake.wait_for_pid(Duration::from_secs(10));
        assert!(!pids.is_empty(), "the waiting endpoint never spawned ssh");
        drop(supervisors);
        assert!(
            fake.all_dead(Duration::from_secs(5)),
            "shutdown left an ssh child behind: {pids:?}"
        );
        drop(rx);
    }
}
