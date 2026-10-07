use super::*;
use crate::protocol::endpoint_wire::{self as wire, ClientMessage, ServerMessage};
use crate::server::endpoint_transport::EndpointTransportEvent;
use interprocess::local_socket::traits::Stream as _;

const TIMEOUT: Duration = crate::server::client_transport::HANDSHAKE_TIMEOUT;
/// Real PTY shells can take several seconds to print on loaded CI runners
/// (macOS runners running the whole suite in parallel); waiting longer only
/// matters when the output is late, so passing runs stay fast.
const PTY_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn endpoint_notifications_actual_two_viewers_route_sound_and_toast_to_latest_owner() {
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "NOTIFICATION-OWNER").await;
    let child_pid = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .child_pid()
        .unwrap();
    let (first_id, mut first) = connect_with_interest(&mut server, true).await;
    let (second_id, mut second) = connect_with_interest(&mut server, true).await;
    eprintln!("notification owner test child={child_pid} phase=initial-views");
    server.stream_endpoint_views();
    let _ = receive_view(&mut first);
    let _ = receive_view(&mut second);
    for stream in [&mut first, &mut second] {
        eprintln!("notification owner phase=presentation-commit");
        protocol::write_message(
            stream,
            &ClientMessage::EndpointControl {
                kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND.into(),
                data: "notification-owner-commit".into(),
            },
        )
        .unwrap();
        dispatch_input(&mut server).await;
        loop {
            if matches!(receive(stream), ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND
                    && data == "notification-owner-commit")
            {
                break;
            }
        }
    }
    let public_focus = |server: &HeadlessServer| {
        let workspace = server.app.state.active.unwrap();
        let tab = server.app.state.workspaces[workspace].active_tab_index();
        let pane = server.app.state.workspaces[workspace]
            .focused_pane_id()
            .unwrap();
        [
            server.app.public_workspace_id(workspace),
            server.app.public_tab_id(workspace, tab).unwrap(),
            server.app.public_pane_id(workspace, pane).unwrap(),
        ]
    };
    let before = public_focus(&server);
    assert!(before.iter().all(|id| !id.is_empty()));
    let mut deliveries = Vec::new();
    for (first_owner, kind, message) in [
        (false, protocol::NotifyKind::Sound, "agent done"),
        (true, protocol::NotifyKind::Toast, "OWNED-NOTIFICATION"),
        (
            false,
            protocol::NotifyKind::SystemToast,
            "OWNED-NOTIFICATION-CONTEXT",
        ),
    ] {
        let (owner_id, owner, other) = if first_owner {
            (first_id, &mut first, &mut second)
        } else {
            (second_id, &mut second, &mut first)
        };
        protocol::write_message(other, &ClientMessage::ClientShellFocus { focused: false })
            .unwrap();
        dispatch_input(&mut server).await;
        protocol::write_message(owner, &ClientMessage::ClientShellFocus { focused: true }).unwrap();
        dispatch_input(&mut server).await;
        assert!(server.send_notify_to_foreground_client(kind.clone(), message, None, None));
        eprintln!("notification owner phase=delivery owner={owner_id} kind={kind:?}");
        let ServerMessage::EndpointControl {
            kind: control_kind,
            data,
        } = receive(owner)
        else {
            panic!("selected owner must receive notification control");
        };
        assert_eq!(
            control_kind,
            crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND
        );
        let payload: crate::protocol::endpoint_projection::ForwardedNotification =
            serde_json::from_str(&data).unwrap();
        assert_eq!(payload.boot_id, server.endpoint_boot_id);
        assert!(
            matches!(&payload.notification, protocol::ServerMessage::Notify {
            kind: received_kind, message: received_message, target_pane_id: None, ..
        } if *received_kind == kind && received_message == message)
        );
        protocol::write_message(
            other,
            &ClientMessage::EndpointControl {
                kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
                data: "notification-other-barrier".into(),
            },
        )
        .unwrap();
        eprintln!("notification owner phase=other-barrier owner={owner_id}");
        assert!(
            matches!(receive(other), ServerMessage::EndpointControl { kind, data }
            if kind == crate::protocol::endpoint::HEALTH_PONG_KIND && data == "notification-other-barrier")
        );
        deliveries.push(serde_json::json!({"owner":owner_id,"payload":payload}));
    }
    let after = public_focus(&server);
    assert_eq!(before, after);
    drop(first);
    drop(second);
    shutdown_test_runtimes(&mut server);
    assert!(!crate::platform::process_exists(child_pid));
    std::fs::write(server.client_socket_path.with_file_name("notification-owners-evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"actual two sockets latest owner controls; no audio/OS display/normal multi-host/SSH",
            "deliveries":deliveries,"public_before":before,"public_after":after,
            "owned_child_pid":child_pid,"test_pid":std::process::id()
        })).unwrap()).unwrap();
}

async fn pump_endpoint_runtime(
    runtime: &mut crate::client::endpoint::runtime::EndpointRuntime,
    local: &mut HeadlessServer,
    other: &mut HeadlessServer,
    ready: impl Fn(&crate::client::endpoint::runtime::EndpointRuntime) -> bool,
) -> Vec<crate::client::endpoint::runtime::RuntimeUpdate> {
    tokio::time::timeout(TIMEOUT, async {
        let mut updates = Vec::new();
        while !ready(runtime) {
            tokio::select! {
                event = local.endpoint_event_rx.recv() => {
                    local.handle_endpoint_event(event.unwrap());
                    local.stream_endpoint_views();
                }
                event = other.endpoint_event_rx.recv() => {
                    other.handle_endpoint_event(event.unwrap());
                    other.stream_endpoint_views();
                }
                event = runtime.reader_events.recv() => {
                    updates.push(runtime.receive(event.unwrap(), Instant::now()));
                }
            }
        }
        updates
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn endpoint_runtime_actual_two_servers_routes_input_updates_inactive_and_rejects_old_generation(
) {
    use crate::client::endpoint::{
        handshake::EndpointConnectOptions, runtime::EndpointRuntime, shell::ClientShellState,
        supervisor::EndpointSupervisors, transport, ClientEndpointId, EndpointNegotiation,
        EndpointRegistry, EndpointSendOutcome, FocusTarget, ResourceKey,
    };
    let mut local_server = test_headless_server();
    let mut other_server = test_headless_server();
    let local_terminal = owned_activation_pty(&mut local_server, "RUNTIME-LOCAL").await;
    let other_terminal = owned_activation_pty(&mut other_server, "RUNTIME-OTHER").await;
    let (_, mut local_stream) = connect_with_interest(&mut local_server, true).await;
    let (_, mut other_stream) = connect_with_interest(&mut other_server, false).await;
    local_server.stream_endpoint_views();
    other_server.stream_endpoint_views();
    let (local_snapshot, local_surface) = receive_view(&mut local_stream);
    let (other_snapshot, _) = receive_jobs(&mut other_stream);
    let local = ClientEndpointId::Local;
    let other = ClientEndpointId::Ssh("m-preserved-runtime-profile".into());
    let local_key = ResourceKey {
        endpoint: local.clone(),
        id: local_snapshot.focused_pane_id.clone().unwrap(),
    };
    let other_key = ResourceKey {
        endpoint: other.clone(),
        id: other_snapshot.focused_pane_id.clone().unwrap(),
    };
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize { cols: 50, rows: 10 },
        cell_width_px: 8,
        cell_height_px: 16,
        pixel_geometry_exact: true,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let mut shell = ClientShellState::new();
    shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
        id: "m-preserved-runtime-profile".into(),
        label: "Other".into(),
        target: "never-contacted".into(),
        session: "original".into(),
        enabled: true,
    }]);
    for id in [&local, &other] {
        assert!(shell.begin_connection(id, 1));
    }
    shell.receive_snapshot(&local, 1, local_snapshot);
    shell.receive_snapshot(&other, 1, other_snapshot);
    shell.receive_surface(&local, 1, local_surface.clone());
    shell.set_pane_surface(local_surface);
    // All transports below connect to the two owned local listeners. No SSH is invoked.
    let mut runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let negotiation = || {
        EndpointNegotiation::new(
            crate::server::headless::endpoints::supported_methods()
                .iter()
                .map(|method| (*method).into())
                .collect(),
            vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
            ],
        )
    };
    for (id, stream, active) in [
        (local.clone(), local_stream, true),
        (other.clone(), other_stream, false),
    ] {
        let writer = transport::start(
            stream,
            (),
            id.clone(),
            1,
            &negotiation(),
            runtime.reader_sender(),
        )
        .unwrap();
        runtime
            .endpoints
            .insert(id, writer, 1, negotiation(), active);
    }
    assert!(runtime
        .activate(local.clone(), None, Instant::now())
        .error
        .is_none());
    assert_eq!(
        runtime.input(
            &local_key,
            vec![wire::ClientPaneInputEvent::Paste("BLOCKED\n".into())]
        ),
        EndpointSendOutcome::NotSent
    );
    let initial = tokio::time::timeout(TIMEOUT, async {
        let mut updates = Vec::new();
        let mut output_during_fence = false;
        while !runtime.input_lease_current() {
            tokio::select! {
                event = local_server.endpoint_event_rx.recv() => {
                    let event = event.unwrap();
                    if matches!(&event, EndpointTransportEvent::Message {
                        message: ClientMessage::EndpointControl { kind, .. }, ..
                    } if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND) {
                        assert!(!output_during_fence);
                        assert!(!runtime.input_lease_current());
                        // Real PTY output advances the surface before the real ordered
                        // fence response. It must replace the displayed activation baseline.
                        local_server.app.terminal_runtimes.get(&local_terminal).unwrap()
                            .send_paste("OUTPUT-DURING-FENCE\n".into()).await.unwrap();
                        wait_output(&local_server, &local_terminal, "ACK:OUTPUT-DURING-FENCE").await;
                        local_server.stream_endpoint_views();
                        output_during_fence = true;
                    }
                    local_server.handle_endpoint_event(event);
                    local_server.stream_endpoint_views();
                }
                event = other_server.endpoint_event_rx.recv() => {
                    other_server.handle_endpoint_event(event.unwrap());
                    other_server.stream_endpoint_views();
                }
                event = runtime.reader_events.recv() => {
                    updates.push(runtime.receive(event.unwrap(), Instant::now()));
                }
            }
        }
        assert!(output_during_fence);
        assert!(text(runtime.shell.pane_surface.as_ref().unwrap())
            .contains("ACK:OUTPUT-DURING-FENCE"), "actual displayed surface: {:?}", runtime.shell.pane_surface);
        updates
    }).await.unwrap();
    assert!(initial.iter().all(|update| update.error.is_none()));
    let selected_before = runtime.shell.pane_surface.clone();
    let other_workspace = other_server.app.state.workspaces[0].id.clone();
    other_server.app.state.workspaces[0].set_custom_name("Inactive Updated".into());
    other_server.stream_endpoint_views();
    let inactive = pump_endpoint_runtime(
        &mut runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime
                .shell
                .endpoint(&other)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .workspaces
                .iter()
                .any(|workspace| workspace.label == "Inactive Updated")
        },
    )
    .await;
    assert!(inactive.iter().all(|update| update
        .host_effects
        .iter()
        .all(|effect| effect.endpoint_id == local)));
    assert_eq!(runtime.shell.active_endpoint_id, local);
    // The local shell may still print its prompt after the fenced output, so
    // the displayed surface can advance; it must still be the local server's
    // projection and never the inactive endpoint's.
    let before = selected_before.as_ref().unwrap();
    let after = runtime.shell.pane_surface.as_ref().unwrap();
    assert_eq!(
        (&after.boot_id, after.projection_revision),
        (&before.boot_id, before.projection_revision)
    );
    assert!(after.surface_revision >= before.surface_revision);
    assert!(runtime
        .shell
        .aggregate_workspaces()
        .iter()
        .any(|key| key.endpoint == other && key.id == other_workspace));
    assert!(runtime
        .activate(
            other.clone(),
            Some(FocusTarget::Pane(other_key.id.clone())),
            Instant::now()
        )
        .error
        .is_none());
    assert_eq!(
        runtime.input(
            &other_key,
            vec![wire::ClientPaneInputEvent::Paste("BLOCKED\n".into())]
        ),
        EndpointSendOutcome::NotSent
    );
    let switching = pump_endpoint_runtime(
        &mut runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.endpoints.active_id() == &other && runtime.endpoints.active_surface_available()
        },
    )
    .await;
    assert!(switching.iter().all(|update| update.error.is_none()));
    assert!(switching.iter().any(|update| update.clear_host_effects));
    assert!(switching
        .iter()
        .flat_map(|update| &update.host_effects)
        .all(|effect| effect.endpoint_id == other));
    let surface = runtime.shell.pane_surface.as_ref().unwrap();
    assert_eq!(
        (
            surface.frame.width,
            surface.frame.height,
            surface.frame.cells.len()
        ),
        (50, 10, 500)
    );
    assert_eq!(
        runtime.input(
            &local_key,
            vec![wire::ClientPaneInputEvent::Paste("WRONG-ENDPOINT\n".into())]
        ),
        EndpointSendOutcome::NotSent
    );
    assert_eq!(
        runtime.input(
            &other_key,
            vec![wire::ClientPaneInputEvent::Paste("RUNTIME-TARGET\n".into())]
        ),
        EndpointSendOutcome::Sent
    );
    dispatch_input(&mut other_server).await;
    wait_output(&other_server, &other_terminal, "ACK:RUNTIME-TARGET").await;
    assert!(!local_server
        .app
        .terminal_runtimes
        .get(&local_terminal)
        .unwrap()
        .visible_text()
        .contains("RUNTIME-TARGET"));
    assert!(!other_server
        .app
        .terminal_runtimes
        .get(&other_terminal)
        .unwrap()
        .visible_text()
        .contains("BLOCKED"));
    assert!(runtime
        .activate(
            local.clone(),
            Some(FocusTarget::Pane(local_key.id.clone())),
            Instant::now()
        )
        .error
        .is_none());
    pump_endpoint_runtime(
        &mut runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.endpoints.active_id() == &local && runtime.endpoints.active_surface_available()
        },
    )
    .await;
    let stale_surface = runtime
        .shell
        .endpoint(&other)
        .unwrap()
        .cache
        .coherent_surface(1, options.surface_size)
        .cloned();
    let disconnect = runtime.receive(
        transport::EndpointReaderEvent::Disconnected {
            endpoint_id: other.clone(),
            generation: 1,
            error: std::io::ErrorKind::UnexpectedEof.into(),
        },
        Instant::now(),
    );
    assert!(disconnect.repaint);
    assert!(!disconnect.clear_host_effects);
    assert!(runtime.endpoints.active_surface_available());
    let (_, replacement) = connect_with_interest(&mut other_server, false).await;
    assert!(runtime.shell.begin_connection(&other, 2));
    let writer = transport::start(
        replacement,
        (),
        other.clone(),
        2,
        &negotiation(),
        runtime.reader_sender(),
    )
    .unwrap();
    runtime
        .endpoints
        .insert(other.clone(), writer, 2, negotiation(), false);
    other_server.stream_endpoint_views();
    pump_endpoint_runtime(
        &mut runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime
                .shell
                .endpoint_snapshot_identity(&other, 2)
                .is_some()
        },
    )
    .await;
    assert_eq!(runtime.shell.active_endpoint_id, local);
    assert!(runtime.endpoints.active_surface_available());
    if let Some(surface) = stale_surface {
        assert!(
            !runtime
                .receive(
                    transport::EndpointReaderEvent::Message {
                        endpoint_id: other.clone(),
                        generation: 1,
                        message: Box::new(ServerMessage::PaneSurface(surface))
                    },
                    Instant::now()
                )
                .repaint
        );
    }
    let stale_disconnect = runtime.receive(
        transport::EndpointReaderEvent::Disconnected {
            endpoint_id: other.clone(),
            generation: 1,
            error: std::io::ErrorKind::UnexpectedEof.into(),
        },
        Instant::now(),
    );
    assert!(!stale_disconnect.repaint);
    assert!(runtime.endpoints.accepts(&other, 2));
    assert_eq!(
        runtime.input(
            &local_key,
            vec![wire::ClientPaneInputEvent::Paste(
                "LOCAL-CONTINUES\n".into()
            )]
        ),
        EndpointSendOutcome::Sent
    );
    dispatch_input(&mut local_server).await;
    wait_output(&local_server, &local_terminal, "ACK:LOCAL-CONTINUES").await;
    let settings = crate::client::endpoint::chrome::ChromeSettings::from_config(
        &crate::config::Config::default(),
        crate::app::state::Palette::catppuccin(),
        None,
    );
    let mut chrome = crate::client::endpoint::chrome::ClientChrome::new(settings);
    let initial_view = chrome.compute_view(&runtime.shell, 80, 40);
    let chrome_size = wire::ClientSurfaceSize {
        cols: initial_view.layout.pane_surface.width,
        rows: initial_view.layout.pane_surface.height,
    };
    assert!(runtime
        .resize(
            EndpointConnectOptions {
                surface_size: chrome_size,
                ..options
            },
            Instant::now()
        )
        .error
        .is_none());
    // A resize is not a handoff; wait until the frame for the new geometry is presented.
    pump_endpoint_runtime(
        &mut runtime,
        &mut local_server,
        &mut other_server,
        |runtime| runtime.input_lease_current(),
    )
    .await;
    let view = chrome.compute_view(&runtime.shell, 80, 40);
    let frame = chrome.render(&view);
    assert_eq!(
        (frame.width, frame.height, frame.cells.len()),
        (80, 40, 3200)
    );
    assert!(frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
        .contains("LOCAL-CONTINUES"));
    let remote_hit = view.hits.iter().find(|hit| matches!(&hit.target, crate::client::endpoint::chrome::ChromeTarget::Workspace(key) if key.endpoint == other)).unwrap();
    let Some(crate::client::endpoint::chrome::ChromeTarget::Workspace(target)) =
        chrome.hit(&view, remote_hit.rect.x, remote_hit.rect.y)
    else {
        panic!("drawn machine-qualified workspace must be clickable");
    };
    assert_eq!(target.endpoint, other);
    let point = (remote_hit.rect.x, remote_hit.rect.y);
    let mut frontend = crate::client::endpoint::frontend::ClientFrontend::from_runtime(
        runtime,
        &crate::config::Config::default(),
        chrome.settings,
        (80, 40),
        EndpointConnectOptions {
            surface_size: chrome_size,
            ..options
        },
    );
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: point.0,
                row: point.1,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        ))
        .unwrap();
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.endpoints.active_id() == &other && runtime.endpoints.active_surface_available()
        },
    )
    .await;
    assert_eq!(frontend.runtime.shell.active_endpoint_id, other);
    let remote_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, 80, 40);
    let remote_frame = frontend.chrome.render(&remote_view);
    assert_eq!(remote_frame.cells.len(), 3200);
    let text = remote_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("RUNTIME-TARGET"));
    assert!(!text.contains("LOCAL-CONTINUES"));
    assert!(frontend
        .chrome
        .hit(
            &remote_view,
            remote_view.layout.pane_actions.x,
            remote_view.layout.pane_actions.y
        )
        .is_none());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "FRONTEND-PASTE\n".into(),
        ))
        .unwrap();
    dispatch_input(&mut other_server).await;
    wait_output(&other_server, &other_terminal, "ACK:FRONTEND-PASTE").await;
    for ch in "FRONTEND-KEY".chars() {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char(ch),
                    crossterm::event::KeyModifiers::empty(),
                ),
            ))
            .unwrap();
        dispatch_input(&mut other_server).await;
    }
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ),
        ))
        .unwrap();
    dispatch_input(&mut other_server).await;
    wait_output(&other_server, &other_terminal, "ACK:FRONTEND-KEY").await;
    let local_output = local_server
        .app
        .terminal_runtimes
        .get(&local_terminal)
        .unwrap()
        .visible_text();
    assert!(!local_output.contains("FRONTEND-PASTE"));
    assert!(!local_output.contains("FRONTEND-KEY"));
    other_server.app.state.default_shell = "/bin/sh".into();
    frontend
        .runtime
        .issue_method(crate::api::schema::Method::PaneSplit(
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(other_key.id.clone()),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                env: Default::default(),
            },
        ))
        .unwrap();
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.input_lease_current()
                && runtime
                    .shell
                    .endpoint(&other)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .panes
                    .len()
                    == 2
        },
    )
    .await;
    let prefix = crate::config::Config::default().prefix_key();
    fn prefix_action(
        frontend: &mut crate::client::endpoint::frontend::ClientFrontend,
        prefix: (crossterm::event::KeyCode, crossterm::event::KeyModifiers),
        code: crossterm::event::KeyCode,
    ) {
        for key in [
            crate::input::TerminalKey::new(prefix.0, prefix.1),
            crate::input::TerminalKey::new(code, crossterm::event::KeyModifiers::empty()),
        ] {
            frontend
                .dispatch_input(crate::raw_input::RawInputEvent::Key(key))
                .unwrap();
        }
    }
    prefix_action(&mut frontend, prefix, crossterm::event::KeyCode::Char('z'));
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.input_lease_current()
                && runtime
                    .shell
                    .endpoint(&other)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .tabs
                    .iter()
                    .any(|tab| tab.focused && tab.zoomed)
        },
    )
    .await;
    assert!(!local_server.app.state.workspaces[0].tabs[0].zoomed);
    assert!(other_server.app.state.workspaces[0].tabs[0].zoomed);
    prefix_action(&mut frontend, prefix, crossterm::event::KeyCode::Char('z'));
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.input_lease_current()
                && runtime
                    .shell
                    .endpoint(&other)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .tabs
                    .iter()
                    .any(|tab| tab.focused && !tab.zoomed)
        },
    )
    .await;
    prefix_action(&mut frontend, prefix, crossterm::event::KeyCode::Tab);
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut local_server,
        &mut other_server,
        |runtime| {
            runtime.input_lease_current()
                && runtime
                    .shell
                    .endpoint(&other)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .focused_pane_id
                    .as_ref()
                    .is_some_and(|pane| pane != &other_key.id)
        },
    )
    .await;
    let selected = frontend
        .runtime
        .shell
        .endpoint(&other)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .focused_pane_id
        .clone()
        .unwrap();
    assert_ne!(selected, other_key.id);
    assert_eq!(frontend.runtime.shell.active_endpoint_id, other);
    assert_eq!(frontend.runtime.endpoints.active_id(), &other);
    assert!(other_server
        .app
        .terminal_runtimes
        .iter()
        .all(|(_, runtime)| runtime.child_pid().is_some()));
    drop(frontend);
    shutdown_test_runtimes(&mut local_server);
    shutdown_test_runtimes(&mut other_server);
}

#[tokio::test]
async fn endpoint_supervisor_actual_server_local_recovery_never_bootstraps_missing_socket() {
    use crate::client::endpoint::{
        handshake::EndpointConnectOptions, supervisor::*, transport, ClientEndpointId,
        ClientEndpointStatus,
    };
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "SUPERVISOR-OWNED").await;
    let missing = server.client_socket_path.with_file_name("absent.sock");
    assert!(!missing.exists());
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize { cols: 40, rows: 8 },
        cell_width_px: 8,
        cell_height_px: 16,
        pixel_geometry_exact: true,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let mut supervisors = EndpointSupervisors::new(&[], Instant::now());
    supervisors.add_local(missing.clone(), None, Instant::now());
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    supervisors.spawn_due(Instant::now(), options, &tx);
    let failed = tokio::time::timeout(TIMEOUT, rx.recv())
        .await
        .unwrap()
        .unwrap();
    let EndpointSupervisorEvent::Status {
        endpoint_id,
        generation: failed_generation,
        status,
        ..
    } = failed
    else {
        panic!("missing socket cannot connect");
    };
    assert_eq!(endpoint_id, ClientEndpointId::Local);
    assert_eq!(status, ClientEndpointStatus::Reconnecting);
    assert!(supervisors.record_status(&endpoint_id, failed_generation, status, Instant::now()));
    assert!(
        !missing.exists(),
        "the connector cannot launch or restore a missing Local server"
    );
    supervisors.add_local(server.client_socket_path.clone(), None, Instant::now());
    supervisors.spawn_due(Instant::now(), options, &tx);
    let mut accept = tokio::time::interval(CLIENT_ACCEPT_POLL_INTERVAL);
    let connected = tokio::time::timeout(TIMEOUT, async {
        loop {
            tokio::select! {
                _ = accept.tick() => { server.accept_client_connections().unwrap(); }
                event = server.endpoint_event_rx.recv() => {
                    server.handle_endpoint_event(event.unwrap());
                    server.stream_endpoint_views();
                }
                event = rx.recv() => { break event.unwrap(); }
            }
        }
    })
    .await
    .unwrap();
    let EndpointSupervisorEvent::Connected {
        endpoint_id,
        generation,
        stream,
        lifetime,
        negotiation,
    } = connected
    else {
        panic!("actual owned Herdr server must connect");
    };
    assert!(generation > failed_generation);
    assert!(!supervisors.disconnected(&endpoint_id, failed_generation, Instant::now()));
    assert!(supervisors.record_status(
        &endpoint_id,
        generation,
        ClientEndpointStatus::Online,
        Instant::now()
    ));
    let (event_tx, mut events) = tokio::sync::mpsc::channel(8);
    let writer = transport::start(
        stream,
        lifetime,
        endpoint_id.clone(),
        generation,
        &negotiation,
        event_tx,
    )
    .unwrap();
    let snapshot = tokio::time::timeout(TIMEOUT, async {
        loop {
            tokio::select! {
                event = server.endpoint_event_rx.recv() => {
                    server.handle_endpoint_event(event.unwrap());
                    server.stream_endpoint_views();
                }
                event = events.recv() => {
                    let transport::EndpointReaderEvent::Message { endpoint_id: received, generation: received_generation, message } = event.unwrap() else {
                        panic!("owned supervisor socket disconnected");
                    };
                    assert_eq!(received, endpoint_id);
                    assert_eq!(received_generation, generation);
                    if let ServerMessage::EndpointControl { kind, data } = *message {
                        if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
                            break serde_json::from_str::<wire::ClientShellSnapshot>(&data).unwrap();
                        }
                    }
                }
            }
        }
    }).await.unwrap();
    assert_eq!(snapshot.boot_id, server.endpoint_boot_id);
    assert_eq!(snapshot.workspaces[0].label, "SUPERVISOR-OWNED");
    assert!(server.clients.is_empty());
    assert!(
        server
            .endpoint_clients
            .values()
            .all(|client| client.surface.is_none()),
        "reconnect negotiates metadata before selected surface activation"
    );
    assert!(server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .child_pid()
        .is_some());
    drop(writer);
    shutdown_test_runtimes(&mut server);
}

async fn owned_activation_pty(
    server: &mut HeadlessServer,
    label: &str,
) -> crate::terminal::TerminalId {
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new(label)];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let tab = &server.app.state.workspaces[0].tabs[0];
    let pane = tab.root_pane;
    let terminal = tab.terminal_id(pane).unwrap().clone();
    let command = format!(
        "printf '{label}\\n'; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done"
    );
    let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
        pane,
        24,
        80,
        std::env::current_dir().unwrap(),
        &["/bin/sh".into(), "-c".into(), command],
        &crate::pane::PaneLaunchEnv::default(),
        crate::pane::AgentDetection::Disabled,
        0,
        crate::terminal_theme::TerminalTheme::default(),
        server.app.event_tx.clone(),
        server.app.render_notify.clone(),
        server.app.render_dirty.clone(),
    )
    .unwrap();
    assert!(runtime
        .child_pid()
        .is_some_and(|pid| pid != std::process::id()));
    server
        .app
        .terminal_runtimes
        .insert(terminal.clone(), runtime);
    wait_output(server, &terminal, label).await;
    terminal
}

#[tokio::test]
async fn endpoint_activation_two_real_servers_fences_input_and_preserves_local_namespace() {
    use crate::client::endpoint::{
        activation::*, shell::ClientShellState, transport, ClientEndpointId, EndpointNegotiation,
        EndpointRegistry, EndpointSendOutcome, FocusTarget,
    };
    let mut local_server = test_headless_server();
    let mut other_server = test_headless_server();
    assert_ne!(
        local_server.client_socket_path,
        other_server.client_socket_path
    );
    assert_ne!(local_server.endpoint_boot_id, other_server.endpoint_boot_id);
    let local_terminal = owned_activation_pty(&mut local_server, "LOCAL-OWNED").await;
    let other_terminal = owned_activation_pty(&mut other_server, "OTHER-OWNED").await;
    // The in-process allocator is shared; seed the independent server states with the
    // same restored workspace ID before either real endpoint publishes metadata.
    other_server.app.state.workspaces[0].id = local_server.app.state.workspaces[0].id.clone();
    local_server.app.state.assert_invariants_for_test();
    other_server.app.state.assert_invariants_for_test();
    let (local_view, mut local_stream) = connect_with_interest(&mut local_server, true).await;
    let (other_view, mut other_stream) = connect_with_interest(&mut other_server, false).await;
    local_server.stream_endpoint_views();
    other_server.stream_endpoint_views();
    let (local_snapshot, local_surface) = receive_view(&mut local_stream);
    let (other_snapshot, _) = receive_jobs(&mut other_stream);
    let local = ClientEndpointId::Local;
    let other = ClientEndpointId::Ssh("existing-fork-machine-id".into());
    let pane = other_snapshot.focused_pane_id.clone().unwrap();
    // Both real servers may expose the same fork ID; ownership is carried by the endpoint.
    assert_eq!(
        local_snapshot.workspaces[0].workspace_id,
        other_snapshot.workspaces[0].workspace_id
    );
    let mut shell = ClientShellState::new();
    shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
        id: "existing-fork-machine-id".into(),
        label: "Other".into(),
        target: "unused-no-ssh".into(),
        session: "retained".into(),
        enabled: true,
    }]);
    assert!(shell.begin_connection(&local, 1));
    assert!(shell.begin_connection(&other, 1));
    assert!(shell.receive_snapshot(&local, 1, local_snapshot));
    assert!(shell.receive_surface(&local, 1, local_surface.clone()));
    assert!(shell.receive_snapshot(&other, 1, other_snapshot));
    shell.set_pane_surface(local_surface);
    let keys = shell.aggregate_workspaces();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].id, keys[1].id);
    assert_ne!(keys[0], keys[1]);
    assert_eq!(shell.active_endpoint_id, local);
    let negotiation = || {
        EndpointNegotiation::new(
            crate::server::headless::endpoints::supported_methods()
                .iter()
                .map(|method| (*method).into())
                .collect(),
            vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
            ],
        )
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let local_writer = transport::start(
        local_stream,
        (),
        local.clone(),
        1,
        &negotiation(),
        tx.clone(),
    )
    .unwrap();
    let other_writer =
        transport::start(other_stream, (), other.clone(), 1, &negotiation(), tx).unwrap();
    let mut registry = EndpointRegistry::empty();
    registry.insert(local.clone(), local_writer, 1, negotiation(), true);
    registry.insert(other.clone(), other_writer, 1, negotiation(), false);
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut registry,
        other.clone(),
        Some(FocusTarget::Pane(pane.clone())),
        ClientMessage::ClientShellResize {
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: wire::ClientSurfaceSize { cols: 50, rows: 10 },
            pixel_mouse: true,
        },
        1,
        Instant::now(),
    )
    .unwrap();
    // Target-first: the source keeps its surface and input until the target commits.
    assert!(activation.retains_source());
    assert!(registry.connection(&local).unwrap().surface_active);
    let mut stages = Vec::new();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if stages.is_empty() {
                assert_eq!(registry.active_id(), &local, "the source owns input until the target commits");
                assert!(registry.connection(&local).unwrap().surface_active, "the source keeps its surface while the target prepares");
            } else {
                assert!(!registry.active_surface_available(), "input remains frozen through the presentation fence");
            }
            let progress = tokio::select! {
                event = local_server.endpoint_event_rx.recv() => {
                    local_server.handle_endpoint_event(event.unwrap());
                    local_server.stream_endpoint_views();
                    None
                }
                event = other_server.endpoint_event_rx.recv() => {
                    other_server.handle_endpoint_event(event.unwrap());
                    other_server.stream_endpoint_views();
                    if other_server.endpoint_clients[&other_view].surface.is_some() && stages.is_empty() {
                        assert!(local_server.endpoint_clients[&local_view].surface.is_some(), "the source is not released before the target commits");
                    }
                    None
                }
                event = rx.recv() => {
                    let transport::EndpointReaderEvent::Message { endpoint_id, generation, message } = event.unwrap() else {
                        panic!("owned activation transport disconnected");
                    };
                    Some(match *message {
                        ServerMessage::EndpointControl { kind, data } if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND => {
                            let snapshot: wire::ClientShellSnapshot = serde_json::from_str(&data).unwrap();
                            assert!(shell.receive_snapshot(&endpoint_id, generation, snapshot.clone()));
                            activation.receive_snapshot(&endpoint_id, generation, &snapshot)
                        }
                        ServerMessage::PaneSurface(surface) => {
                            assert!(shell.receive_surface(&endpoint_id, generation, surface.clone()));
                            activation.receive_surface(&endpoint_id, generation, surface)
                        }
                        ServerMessage::ClientShellEndpointResponseChunk { boot_id, request_id, final_chunk, data } => {
                            assert!(final_chunk, "small lifecycle and focus acknowledgements");
                            assert_eq!(activation.receive_response_for_boot(&endpoint_id, generation + 1, &boot_id, &request_id, &data, &mut registry), SurfaceActivationProgress::Stale);
                            activation.receive_response_for_boot(&endpoint_id, generation, &boot_id, &request_id, &data, &mut registry)
                        }
                        ServerMessage::EndpointControl { kind, data } if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND => {
                            assert_eq!(activation.receive_presentation_effects_ready(&endpoint_id, generation + 1, &data), SurfaceActivationProgress::Stale);
                            activation.receive_presentation_effects_ready(&endpoint_id, generation, &data)
                        }
                        _ => SurfaceActivationProgress::Pending,
                    })
                }
            };
            match progress {
                Some(SurfaceActivationProgress::Rejected { message, .. }) => panic!("actual activation rejected: {message}"),
                Some(SurfaceActivationProgress::Ready) => {
                    let completion = activation.complete(&mut shell, &mut registry).unwrap();
                    stages.push(completion.clone());
                    if completion == ActivationCompletion::Activated { break; }
                }
                _ => {}
            }
        }
    }).await.unwrap();
    assert_eq!(
        stages,
        vec![
            ActivationCompletion::AwaitingPresentationSync {
                previous: local.clone(),
                endpoint: other.clone()
            },
            ActivationCompletion::AwaitingPresentationEffects,
            ActivationCompletion::Activated,
        ]
    );
    assert!(!registry.active_surface_available());
    registry.unfreeze_input();
    assert!(registry.active_surface_available());
    assert_eq!(shell.active_endpoint_id, other);
    let surface = shell.pane_surface.as_ref().unwrap();
    assert_eq!(
        (
            surface.frame.width,
            surface.frame.height,
            surface.frame.cells.len()
        ),
        (50, 10, 500)
    );
    assert_eq!(
        (
            surface.panes[0].inner_rect.width,
            surface.panes[0].inner_rect.height
        ),
        (47, 8)
    );
    assert_eq!(
        registry.send(&ClientMessage::ClientShellPaneInput {
            pane_id: pane,
            events: vec![wire::ClientPaneInputEvent::Paste(
                "ACTIVATION-TARGET\n".into()
            )],
        }),
        EndpointSendOutcome::Sent
    );
    dispatch_input(&mut other_server).await;
    wait_output(&other_server, &other_terminal, "ACK:ACTIVATION-TARGET").await;
    assert!(!local_server
        .app
        .terminal_runtimes
        .get(&local_terminal)
        .unwrap()
        .visible_text()
        .contains("ACTIVATION-TARGET"));
    registry.disconnect(&other);
    assert!(shell.disconnect(&other, 1));
    assert!(registry.accepts(&local, 1));
    assert!(!registry.active_surface_available());
    assert!(shell.pane_surface.is_none());
    assert_eq!(
        shell.aggregate_workspaces(),
        keys,
        "offline metadata retains endpoint-qualified IDs"
    );
    assert!(shell.begin_connection(&other, 2));
    assert_eq!(
        shell.active_endpoint_id, other,
        "replacement generation does not invent a new selection"
    );
    assert!(
        shell.endpoint_snapshot_identity(&other, 2).is_none(),
        "cached boot cannot authorize the replacement generation"
    );
    registry.disconnect(&local);
    shutdown_test_runtimes(&mut local_server);
    shutdown_test_runtimes(&mut other_server);
}

async fn next_event(server: &mut HeadlessServer) -> EndpointTransportEvent {
    tokio::time::timeout(TIMEOUT, server.endpoint_event_rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn dispatch_input(server: &mut HeadlessServer) -> bool {
    loop {
        let event = next_event(server).await;
        let input = matches!(event, EndpointTransportEvent::Message { .. });
        let changed = server.handle_endpoint_event(event);
        if input {
            return changed;
        }
    }
}

fn receive(stream: &mut crate::ipc::LocalStream) -> ServerMessage {
    protocol::read_message(stream, MAX_GRAPHICS_FRAME_SIZE).unwrap()
}

fn receive_view(
    stream: &mut crate::ipc::LocalStream,
) -> (wire::ClientShellSnapshot, wire::PaneSurfaceFrame) {
    let ServerMessage::EndpointControl { kind, data } = receive(stream) else {
        panic!("expected snapshot");
    };
    assert_eq!(kind, crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND);
    let snapshot: wire::ClientShellSnapshot = serde_json::from_str(&data).unwrap();
    let ServerMessage::EndpointControl { kind, data } = receive(stream) else {
        panic!("expected fork job projection");
    };
    assert_eq!(kind, crate::protocol::endpoint_jobs::JOBS_PROJECTION_KIND);
    let jobs: crate::protocol::endpoint_jobs::EndpointJobsProjection =
        serde_json::from_str(&data).unwrap();
    assert_eq!(jobs.boot_id, snapshot.boot_id);
    assert_eq!(jobs.revision, snapshot.revision);
    let ServerMessage::PaneSurface(surface) = receive(stream) else {
        panic!("expected surface");
    };
    assert_eq!(surface.boot_id, snapshot.boot_id);
    assert_eq!(surface.projection_revision, snapshot.revision);
    (snapshot, surface)
}

async fn connect(server: &mut HeadlessServer) -> (u64, crate::ipc::LocalStream) {
    connect_with_interest(server, true).await
}

#[tokio::test]
async fn endpoint_active_socket_excludes_legacy_direct_host_and_inactive_restores_it() {
    let mut server = test_headless_server();
    let (writer, _control, _render) = test_client_writer();
    assert!(server.handle_server_event(ServerEvent::ClientConnected {
        client_id: 1,
        cols: 80,
        rows: 24,
        cell_width_px: 10,
        cell_height_px: 20,
        render_encoding: RenderEncoding::SemanticFrame,
        keybindings: None,
        direct_attach_requested: false,
        direct_graphics: true,
        writer,
    }));
    assert!(server.direct_graphics_available());
    let (_, mut stream) = connect(&mut server).await;
    assert!(server.endpoint_has_active_graphics_surface());
    assert!(!server.direct_graphics_available());
    set_surface(&mut server, &mut stream, false).await;
    assert!(!server.endpoint_has_active_graphics_surface());
    assert!(server.direct_graphics_available());
}

async fn connect_with_interest(
    server: &mut HeadlessServer,
    active: bool,
) -> (u64, crate::ipc::LocalStream) {
    let (id, stream, _) = connect_with_welcome(server, active).await;
    (id, stream)
}

async fn connect_with_welcome(
    server: &mut HeadlessServer,
    active: bool,
) -> (
    u64,
    crate::ipc::LocalStream,
    crate::protocol::endpoint::EndpointServerWelcome,
) {
    connect_with_surface_encoding(server, active, false).await
}

async fn connect_with_surface_encoding(
    server: &mut HeadlessServer,
    active: bool,
    compact: bool,
) -> (
    u64,
    crate::ipc::LocalStream,
    crate::protocol::endpoint::EndpointServerWelcome,
) {
    let mut stream = crate::ipc::connect_local_stream(&server.client_socket_path).unwrap();
    stream.set_recv_timeout(Some(TIMEOUT)).unwrap();
    let handshake = tokio::task::spawn_blocking(move || {
        use crate::protocol::endpoint;
        let hello = endpoint::EndpointClientHello {
            generation: endpoint::ENDPOINT_PROTOCOL_GENERATION,
            min_generation: endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION,
            surface_size: wire::ClientSurfaceSize { cols: 40, rows: 8 },
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: true,
            // This pane-less socket peer has no outer terminal for direct uploads.
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: active,
            surface_reuse: compact,
            surface_delta: compact,
            surface_scroll: false,
            snapshot_codecs: vec![endpoint::SNAPSHOT_CODEC_V1.into()],
            surface_codecs: vec![endpoint::SURFACE_CODEC_V1.into()],
            input_codecs: vec![endpoint::INPUT_CODEC_V1.into()],
            blob_codecs: vec![endpoint::BLOB_CODEC_V1.into()],
        };
        protocol::write_message(
            &mut stream,
            &ClientMessage::EndpointControl {
                kind: endpoint::ENDPOINT_HELLO_KIND.into(),
                data: serde_json::to_string(&hello).unwrap(),
            },
        )
        .unwrap();
        let ServerMessage::EndpointControl { kind, data } = receive(&mut stream) else {
            panic!("welcome expected");
        };
        assert_eq!(kind, endpoint::ENDPOINT_WELCOME_KIND);
        let welcome: endpoint::EndpointServerWelcome = serde_json::from_str(&data).unwrap();
        // Bound only this owned test reader after the product handshake clears its timeout.
        stream.set_recv_timeout(Some(TIMEOUT)).unwrap();
        (stream, welcome)
    });
    server.accept_client_connections().unwrap();
    let (stream, welcome) = handshake.await.unwrap();
    assert!(welcome.error.is_none());
    assert!(welcome.methods.contains(&"tab.focus".into()));
    assert!(welcome
        .capabilities
        .contains(&crate::protocol::endpoint_jobs::JOBS_PROJECTION_CAPABILITY.into()));
    assert!(welcome
        .capabilities
        .contains(&crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY.into()));
    assert!(welcome
        .capabilities
        .contains(&crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into()));
    assert!(welcome
        .capabilities
        .contains(&crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into()));
    loop {
        let event = next_event(server).await;
        let id = match &event {
            EndpointTransportEvent::Connected { client_id, .. } => Some(*client_id),
            EndpointTransportEvent::WriterDrained { .. } => None,
            other => panic!("unexpected connection event {other:?}"),
        };
        server.handle_endpoint_event(event);
        if let Some(id) = id {
            return (id, stream, welcome);
        }
    }
}

async fn wait_output(
    server: &HeadlessServer,
    terminal: &crate::terminal::TerminalId,
    marker: &str,
) {
    tokio::time::timeout(PTY_OUTPUT_TIMEOUT, async {
        loop {
            let notified = server.app.render_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            server.app.render_dirty.store(false, Ordering::Release);
            if server
                .app
                .terminal_runtimes
                .get(terminal)
                .unwrap()
                .visible_text()
                .contains(marker)
            {
                return;
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "actual owned PTY marker {marker} missing; screen={}",
            server
                .app
                .terminal_runtimes
                .get(terminal)
                .unwrap()
                .visible_text()
        )
    });
}

fn text(surface: &wire::PaneSurfaceFrame) -> String {
    surface
        .frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

async fn set_surface(
    server: &mut HeadlessServer,
    stream: &mut crate::ipc::LocalStream,
    active: bool,
) -> u64 {
    protocol::write_message(stream, &ClientMessage::ClientShellEndpointRequest {
        boot_id: server.endpoint_boot_id.clone(),
        request: serde_json::json!({"id":"surface-interest","method":"client_shell.surface.set","params":{"active":active}}).to_string(),
    }).unwrap();
    assert!(dispatch_input(server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        request_id,
        data,
        final_chunk: true,
        ..
    } = receive(stream)
    else {
        panic!("surface interest ACK expected");
    };
    assert_eq!(request_id, "surface-interest");
    let response: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(response["result"]["active"], active);
    response["result"]["projection_revision"].as_u64().unwrap()
}

#[tokio::test]
async fn endpoint_reactivation_resends_real_pty_image_after_host_scope_retirement() {
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "GRAPHICS-REACTIVATION").await;
    let runtime = server.app.terminal_runtimes.get(&terminal).unwrap();
    let child_pid = runtime.child_pid().unwrap();
    runtime
        .send_paste(
            "\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,C=1,q=2;/wAA/w==\x1b\\\nGRAPHICS-DONE\n"
                .into(),
        )
        .await
        .unwrap();
    wait_output(&server, &terminal, "ACK:GRAPHICS-DONE").await;
    server.app.state.kitty_graphics_enabled = true;
    let (_, mut stream) = connect(&mut server).await;
    server.stream_endpoint_views();
    let (_, initial) = receive_view(&mut stream);
    assert_eq!(initial.graphics.assets.len(), 1);
    assert_eq!(initial.graphics.assets[0].data, [255, 0, 0, 255]);
    assert!(!initial.graphics.placements.is_empty());
    server.stream_endpoint_views();
    let (_, repeated) = receive_view(&mut stream);
    assert!(repeated.graphics.assets.is_empty());
    assert_eq!(repeated.graphics.placements, initial.graphics.placements);
    set_surface(&mut server, &mut stream, false).await;
    set_surface(&mut server, &mut stream, true).await;
    server.stream_endpoint_views();
    let (_, reactivated) = receive_view(&mut stream);
    assert_eq!(reactivated.graphics.assets, initial.graphics.assets);
    assert_eq!(reactivated.graphics.placements, initial.graphics.placements);
    eprintln!("graphics reactivation owned child={child_pid}");
    drop(stream);
    shutdown_test_runtimes(&mut server);
    assert!(!crate::platform::process_exists(child_pid));
}

#[tokio::test]
async fn endpoint_lifecycle_actual_pty_releases_key_and_mouse_and_fences_viewer_modes() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("owned-release")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let tab = &server.app.state.workspaces[0].tabs[0];
    let pane = tab.root_pane;
    let terminal = tab.terminal_id(pane).unwrap().clone();
    let public_pane = server.app.public_pane_id(0, pane).unwrap();
    let expected = [
        ("KEYDOWN", b"\x1b[97;1:1u".as_slice()),
        ("KEYUP", b"\x1b[97;1:3u".as_slice()),
        ("MOUSEDOWN", b"\x1b[<0;3;2M".as_slice()),
        ("MOUSEUP", b"\x1b[<0;3;2m".as_slice()),
    ];
    let mut command =
        "stty raw -echo; printf '\\033[>11u\\033[?1003h\\033[?1006hREADY\\r\\n'; ".to_string();
    for (marker, bytes) in &expected {
        command.push_str(&format!("dd bs=1 count={} 2>/dev/null | od -An -v -tx1 | tr -d ' \\n'; printf ':{marker}\\r\\n'; ", bytes.len()));
    }
    // Keep the owned PTY and its modes alive until the test's explicit cleanup.
    command.push_str("IFS= read -r final_line");
    let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
        pane,
        24,
        80,
        std::env::current_dir().unwrap(),
        &["/bin/sh".into(), "-c".into(), command],
        &crate::pane::PaneLaunchEnv::default(),
        crate::pane::AgentDetection::Disabled,
        0,
        crate::terminal_theme::TerminalTheme::default(),
        server.app.event_tx.clone(),
        server.app.render_notify.clone(),
        server.app.render_dirty.clone(),
    )
    .unwrap();
    assert!(runtime
        .child_pid()
        .is_some_and(|pid| pid != std::process::id()));
    server
        .app
        .terminal_runtimes
        .insert(terminal.clone(), runtime);
    wait_output(&server, &terminal, "READY").await;
    let (first_id, mut first) = connect_with_interest(&mut server, false).await;
    let (other_id, mut other) = connect_with_interest(&mut server, false).await;
    server.stream_endpoint_views();
    let _ = receive_jobs(&mut first);
    let _ = receive_jobs(&mut other);
    let original_label = server.app.state.workspaces[0].display_name();
    protocol::write_message(&mut other, &ClientMessage::ClientShellEndpointRequest {
        boot_id: server.endpoint_boot_id.clone(),
        request: serde_json::json!({"id":"inactive-rename","method":"workspace.rename","params":{"workspace_id":server.app.public_workspace_id(0),"label":"MUST-NOT-MUTATE"}}).to_string(),
    }).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        data,
        request_id,
        final_chunk: true,
        ..
    } = receive(&mut other)
    else {
        panic!("inactive request error expected");
    };
    assert_eq!(request_id, "inactive-rename");
    let error: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(error["error"]["code"], "surface_inactive");
    assert_eq!(
        server.app.state.workspaces[0].display_name(),
        original_label
    );
    let key = wire::ClientPaneInputEvent::Key {
        code: wire::ClientKeyCode::Char('a'),
        modifiers: 0,
        kind: wire::ClientKeyKind::Press,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: Some("a".into()),
        tracks_release: true,
        physical_key_id: Some(97),
        windows_record: None,
    };
    let first_floor = set_surface(&mut server, &mut first, true).await;
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellPaneInput {
            pane_id: public_pane.clone(),
            events: vec![key.clone()],
        },
    )
    .unwrap();
    assert!(
        !dispatch_input(&mut server).await,
        "no input before matching surface"
    );
    server.stream_endpoint_views();
    let (snapshot, _) = receive_view(&mut first);
    assert!(snapshot.revision >= first_floor);
    protocol::write_message(
        &mut first,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND.into(),
            data: "commit-1".into(),
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    assert!(matches!(
        receive(&mut first),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false
        }
    ));
    assert!(matches!(
        receive(&mut first),
        ServerMessage::ClientShellKeyboardReportAll { enabled: true }
    ));
    assert!(matches!(
        receive(&mut first),
        ServerMessage::WindowTitle { title: None }
    ));
    assert!(
        matches!(receive(&mut first), ServerMessage::EndpointControl { kind, data }
        if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND && data == "commit-1")
    );
    protocol::write_message(
        &mut other,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
            data: "other-barrier".into(),
        },
    )
    .unwrap();
    assert!(
        matches!(receive(&mut other), ServerMessage::EndpointControl { kind, data }
        if kind == crate::protocol::endpoint::HEALTH_PONG_KIND && data == "other-barrier"),
        "nonselected viewer receives no host presentation effects"
    );
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellPaneInput {
            pane_id: public_pane.clone(),
            events: vec![key],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_output(&server, &terminal, "KEYDOWN").await;
    let mut child_output = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .visible_text();
    set_surface(&mut server, &mut first, false).await;
    wait_output(&server, &terminal, "KEYUP").await;
    child_output.push_str(
        &server
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .visible_text(),
    );
    let next_floor = set_surface(&mut server, &mut first, true).await;
    assert!(next_floor > first_floor);
    server.stream_endpoint_views();
    let (snapshot, _) = receive_view(&mut first);
    assert!(snapshot.revision >= next_floor);
    let reassert_floor = set_surface(&mut server, &mut first, true).await;
    assert!(
        reassert_floor > next_floor,
        "active-to-active is a fresh epoch"
    );
    server.stream_endpoint_views();
    let (snapshot, _) = receive_view(&mut first);
    assert!(snapshot.revision >= reassert_floor);
    protocol::write_message(
        &mut first,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
            data: "before-commit-2".into(),
        },
    )
    .unwrap();
    assert!(
        matches!(receive(&mut first), ServerMessage::EndpointControl { kind, data }
        if kind == crate::protocol::endpoint::HEALTH_PONG_KIND && data == "before-commit-2"),
        "reassertion emits no host effects before commit"
    );
    protocol::write_message(
        &mut first,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND.into(),
            data: "commit-2".into(),
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    assert!(matches!(
        receive(&mut first),
        ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false
        }
    ));
    assert!(matches!(
        receive(&mut first),
        ServerMessage::ClientShellKeyboardReportAll { enabled: true }
    ));
    assert!(matches!(
        receive(&mut first),
        ServerMessage::WindowTitle { title: None }
    ));
    assert!(
        matches!(receive(&mut first), ServerMessage::EndpointControl { kind, data }
        if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND && data == "commit-2")
    );
    let mouse = wire::ClientPaneInputEvent::Mouse {
        kind: wire::ClientMouseKind::Down(wire::ClientMouseButton::Left),
        position: wire::ClientMousePosition::Cell { column: 2, row: 1 },
        geometry: None,
        modifiers: 0,
        lines: 1,
    };
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellPaneInput {
            pane_id: public_pane.clone(),
            events: vec![mouse],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_output(&server, &terminal, "MOUSEDOWN").await;
    child_output.push_str(
        &server
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .visible_text(),
    );
    protocol::write_message(&mut first, &ClientMessage::Detach).unwrap();
    loop {
        let event = next_event(&mut server).await;
        let gone = matches!(event, EndpointTransportEvent::Disconnected { client_id } if client_id == first_id);
        server.handle_endpoint_event(event);
        if gone {
            break;
        }
    }
    wait_output(&server, &terminal, "MOUSEUP").await;
    child_output.push_str(
        &server
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .visible_text(),
    );
    assert!(server.endpoint_clients.contains_key(&other_id));
    assert!(server
        .endpoint_tab_geometry
        .values()
        .all(|owner| *owner != first_id));
    let screen = child_output.replace([' ', '\n', '\r'], "");
    for (marker, bytes) in expected {
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert!(
            screen.contains(&format!("{hex}:{marker}")),
            "actual child byte mismatch: {screen}"
        );
    }
    assert_eq!(server.app.state.active, Some(0));
    server.app.state.assert_invariants_for_test();
    protocol::write_message(&mut other, &ClientMessage::Detach).unwrap();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn endpoint_live_headless_socket_viewer_focus_paste_resize_disconnect_and_reconnect() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("endpoint-live");
    let second = workspace.test_add_tab(Some("second"));
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let mut terminals = Vec::new();
    for tab_index in [0, second] {
        let tab = &server.app.state.workspaces[0].tabs[tab_index];
        let pane = tab.root_pane;
        let terminal = tab.terminal_id(pane).unwrap().clone();
        let marker = format!("LIVE-PTY-{tab_index}");
        let command = format!(
            "printf '{marker}\\n'; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done"
        );
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane,
            24,
            80,
            std::env::current_dir().unwrap(),
            &["/bin/sh".into(), "-c".into(), command],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            server.app.event_tx.clone(),
            server.app.render_notify.clone(),
            server.app.render_dirty.clone(),
        )
        .unwrap();
        assert!(runtime
            .child_pid()
            .is_some_and(|pid| pid != std::process::id()));
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        wait_output(&server, &terminal, &marker).await;
        terminals.push(terminal);
    }
    let (first_id, mut first) = connect(&mut server).await;
    let (second_id, mut second_stream) = connect(&mut server).await;
    assert!(
        server.clients.is_empty(),
        "stable endpoints never enter private registry"
    );
    server.stream_endpoint_views();
    let (first_snapshot, first_surface) = receive_view(&mut first);
    let (second_snapshot, _) = receive_view(&mut second_stream);
    assert!(text(&first_surface).contains("LIVE-PTY-0"));
    let second_tab = server.app.public_tab_id(0, second).unwrap();
    let second_pane = server
        .app
        .public_pane_id(0, server.app.state.workspaces[0].tabs[second].root_pane)
        .unwrap();
    let focus = serde_json::json!({"id":"focus-second","method":"tab.focus","params":{"tab_id":second_tab}}).to_string();
    protocol::write_message(
        &mut second_stream,
        &ClientMessage::ClientShellEndpointRequest {
            boot_id: second_snapshot.boot_id,
            request: focus,
        },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    let ServerMessage::ClientShellEndpointResponseChunk {
        final_chunk: true,
        data,
        ..
    } = receive(&mut second_stream)
    else {
        panic!("focus response expected")
    };
    let focus_reply: api::schema::SuccessResponse = serde_json::from_slice(&data).unwrap();
    let api::schema::ResponseResult::TabInfo { tab: focused_tab } = focus_reply.result else {
        panic!("tab focus result expected")
    };
    assert!(
        focused_tab.focused,
        "acknowledgement must describe this viewer's focused tab"
    );
    assert_eq!(focused_tab.tab_id, second_tab);
    for (stream, expected_focus, expected_active_tab) in [
        (
            &mut first,
            false,
            first_snapshot.focused_tab_id.clone().unwrap(),
        ),
        (&mut second_stream, true, second_tab.clone()),
    ] {
        for (method, params) in [
            ("tab.get", serde_json::json!({"tab_id":second_tab})),
            (
                "workspace.get",
                serde_json::json!({"workspace_id":server.app.public_workspace_id(0)}),
            ),
            (
                "tab.list",
                serde_json::json!({"workspace_id":server.app.public_workspace_id(0)}),
            ),
        ] {
            protocol::write_message(
                stream,
                &ClientMessage::ClientShellEndpointRequest {
                    boot_id: server.endpoint_boot_id.clone(),
                    request: serde_json::json!({"id":"viewer-get","method":method,"params":params})
                        .to_string(),
                },
            )
            .unwrap();
            dispatch_input(&mut server).await;
            let ServerMessage::ClientShellEndpointResponseChunk {
                final_chunk: true,
                data,
                ..
            } = receive(stream)
            else {
                panic!("viewer get response expected")
            };
            let reply: api::schema::SuccessResponse = serde_json::from_slice(&data).unwrap();
            match reply.result {
                api::schema::ResponseResult::TabInfo { tab } => {
                    assert_eq!(tab.focused, expected_focus)
                }
                api::schema::ResponseResult::WorkspaceInfo { workspace } => {
                    assert_eq!(workspace.active_tab_id, expected_active_tab)
                }
                api::schema::ResponseResult::TabList { tabs } => assert_eq!(
                    tabs.iter()
                        .filter(|tab| tab.focused)
                        .map(|tab| tab.tab_id.as_str())
                        .collect::<Vec<_>>(),
                    [expected_active_tab.as_str()]
                ),
                other => panic!("unexpected viewer get result {other:?}"),
            }
        }
    }
    let global = server.app.handle_api_request(api::schema::Request {
        id: "global-get".into(),
        method: api::schema::Method::TabGet(api::schema::TabTarget {
            tab_id: second_tab.clone(),
        }),
    });
    let global: api::schema::SuccessResponse = serde_json::from_str(&global).unwrap();
    assert!(
        matches!(global.result, api::schema::ResponseResult::TabInfo { tab } if !tab.focused),
        "public API retains global focus facts"
    );
    server.stream_endpoint_views();
    let (focused, second_surface) = receive_view(&mut second_stream);
    assert_eq!(focused.focused_tab_id.as_deref(), Some(second_tab.as_str()));
    assert_eq!(server.app.state.workspaces[0].active_tab_index(), 0);
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(
        server.endpoint_clients[&first_id].location.focused_tab_id(),
        first_snapshot.focused_tab_id.as_deref()
    );
    assert!(text(&second_surface).contains("LIVE-PTY-1"));
    assert!(!text(&second_surface).contains("LIVE-PTY-0"));
    assert_eq!(second_surface.frame.cells.len(), 40 * 8);
    assert_eq!(second_surface.panes[0].inner_rect.width, 37);
    assert_eq!(second_surface.panes[0].inner_rect.height, 6);
    protocol::write_message(
        &mut second_stream,
        &ClientMessage::ClientShellPaneInput {
            pane_id: second_pane.clone(),
            events: vec![wire::ClientPaneInputEvent::Paste("TARGET-SECOND\n".into())],
        },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    wait_output(&server, &terminals[1], "ACK:TARGET-SECOND").await;
    assert!(!server
        .app
        .terminal_runtimes
        .get(&terminals[0])
        .unwrap()
        .visible_text()
        .contains("TARGET-SECOND"));
    protocol::write_message(
        &mut second_stream,
        &ClientMessage::ClientShellResize {
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: wire::ClientSurfaceSize { cols: 50, rows: 10 },
            pixel_mouse: true,
        },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    server.stream_endpoint_views();
    let (_, resized) = receive_view(&mut second_stream);
    assert_eq!((resized.frame.width, resized.frame.height), (50, 10));
    assert_eq!(
        (
            resized.panes[0].inner_rect.width,
            resized.panes[0].inner_rect.height
        ),
        (47, 8)
    );
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&terminals[0])
            .unwrap()
            .current_size(),
        (6, 37)
    );
    assert_eq!(
        server
            .app
            .terminal_runtimes
            .get(&terminals[1])
            .unwrap()
            .current_size(),
        (8, 47)
    );
    protocol::write_message(&mut second_stream, &ClientMessage::Detach).unwrap();
    loop {
        let event = next_event(&mut server).await;
        let gone = matches!(event, EndpointTransportEvent::Disconnected { client_id } if client_id == second_id);
        server.handle_endpoint_event(event);
        if gone {
            break;
        }
    }
    assert!(server.endpoint_clients.contains_key(&first_id));
    let (replacement_id, mut replacement) = connect(&mut server).await;
    assert_ne!(replacement_id, second_id);
    server.stream_endpoint_views();
    let (restored, _) = receive_view(&mut replacement);
    assert_eq!(restored.focused_tab_id, first_snapshot.focused_tab_id);
    assert_eq!(
        server.endpoint_clients[&first_id].location.focused_tab_id(),
        first_snapshot.focused_tab_id.as_deref()
    );
    server.app.state.assert_invariants_for_test();
    protocol::write_message(&mut first, &ClientMessage::Detach).unwrap();
    protocol::write_message(&mut replacement, &ClientMessage::Detach).unwrap();
    shutdown_test_runtimes(&mut server);
}

fn receive_jobs(
    stream: &mut crate::ipc::LocalStream,
) -> (
    wire::ClientShellSnapshot,
    crate::protocol::endpoint_jobs::EndpointJobsProjection,
) {
    let ServerMessage::EndpointControl { kind, data } = receive(stream) else {
        panic!("snapshot expected");
    };
    assert_eq!(kind, crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND);
    let snapshot: wire::ClientShellSnapshot = serde_json::from_str(&data).unwrap();
    let ServerMessage::EndpointControl { kind, data } = receive(stream) else {
        panic!("job projection expected");
    };
    assert_eq!(kind, crate::protocol::endpoint_jobs::JOBS_PROJECTION_KIND);
    let jobs: crate::protocol::endpoint_jobs::EndpointJobsProjection =
        serde_json::from_str(&data).unwrap();
    assert_eq!(jobs.boot_id, snapshot.boot_id);
    assert_eq!(jobs.revision, snapshot.revision);
    (snapshot, jobs)
}

async fn command_result(
    receiver: &mut tokio::sync::mpsc::Receiver<
        crate::client::endpoint::transport::EndpointReaderEvent,
    >,
    commands: &mut crate::client::endpoint::commands::EndpointCommands,
) -> crate::client::endpoint::commands::EndpointCommandResult {
    loop {
        let event = tokio::time::timeout(TIMEOUT, receiver.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            crate::client::endpoint::transport::EndpointReaderEvent::Message {
                endpoint_id,
                generation,
                message,
            } => {
                if let ServerMessage::ClientShellEndpointResponseChunk {
                    boot_id,
                    request_id,
                    final_chunk,
                    data,
                } = *message
                {
                    if let Some(result) = commands
                        .receive_chunk(
                            &endpoint_id,
                            generation,
                            &boot_id,
                            &request_id,
                            final_chunk,
                            data,
                        )
                        .unwrap()
                    {
                        return result;
                    }
                }
            }
            crate::client::endpoint::transport::EndpointReaderEvent::Disconnected {
                error, ..
            } => panic!("owned command socket disconnected: {error}"),
        }
    }
}

#[tokio::test]
async fn endpoint_commands_live_server_qualified_response_and_disconnect_without_mutation_replay() {
    use crate::client::endpoint::{
        commands::*, transport, ClientEndpointId, EndpointNegotiation, EndpointRegistry,
    };

    let mut server = test_headless_server();
    server.app.state.workspaces = vec![
        crate::workspace::Workspace::test_new("first"),
        crate::workspace::Workspace::test_new("second"),
    ];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let workspace = server.app.public_workspace_id(1);
    let (local_id, mut local_stream) = connect_with_interest(&mut server, true).await;
    let (remote_id, mut remote_stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (local_snapshot, _) = receive_view(&mut local_stream);
    let (remote_snapshot, _) = receive_view(&mut remote_stream);
    let local = ClientEndpointId::Local;
    let remote = ClientEndpointId::Ssh("m-command-namespace".into());
    let negotiation = || {
        EndpointNegotiation::new(
            crate::server::headless::endpoints::supported_methods()
                .iter()
                .map(|method| (*method).into())
                .collect(),
            Vec::new(),
        )
    };
    let (local_tx, mut local_rx) = tokio::sync::mpsc::channel(8);
    let (remote_tx, mut remote_rx) = tokio::sync::mpsc::channel(8);
    let local_writer =
        transport::start(local_stream, (), local.clone(), 7, &negotiation(), local_tx).unwrap();
    let remote_writer = transport::start(
        remote_stream,
        (),
        remote.clone(),
        8,
        &negotiation(),
        remote_tx,
    )
    .unwrap();
    let mut registry = EndpointRegistry::empty();
    registry.insert(local.clone(), local_writer, 7, negotiation(), false);
    registry.insert(remote.clone(), remote_writer, 8, negotiation(), false);
    let mut commands = EndpointCommands::default();
    let rename = |id: &str, label: &str| -> Box<EndpointCommandRequest> {
        Box::new(
            api::schema::Request {
                id: id.into(),
                method: api::schema::Method::WorkspaceRename(api::schema::WorkspaceRenameParams {
                    workspace_id: workspace.clone(),
                    label: label.into(),
                }),
            }
            .try_into()
            .unwrap(),
        )
    };
    // The same opaque request id may be in flight on two endpoint lanes.
    commands.enqueue(
        local.clone(),
        7,
        local_snapshot.boot_id.clone(),
        rename("same-id", "LOCAL-COMMITTED"),
    );
    commands.enqueue(
        remote.clone(),
        8,
        remote_snapshot.boot_id.clone(),
        rename("same-id", "REMOTE-COMMITTED"),
    );
    commands.enqueue(
        remote.clone(),
        8,
        remote_snapshot.boot_id.clone(),
        rename("queued-mutation", "NEVER-REPLAY"),
    );
    assert!(commands.send_next(&local, &mut registry).is_empty());
    assert!(commands.send_next(&remote, &mut registry).is_empty());
    dispatch_input(&mut server).await;
    dispatch_input(&mut server).await;
    let local_result = command_result(&mut local_rx, &mut commands).await;
    assert_eq!(local_result.endpoint_id, local);
    assert_eq!(local_result.generation, 7);
    assert_eq!(local_result.request_id, "same-id");
    assert_eq!(
        local_result.result.unwrap()["workspace"]["label"],
        "LOCAL-COMMITTED"
    );
    assert!(commands.accepts_response(&remote, 8, &remote_snapshot.boot_id, "same-id"));
    assert!(!commands.accepts_response(&remote, 7, &remote_snapshot.boot_id, "same-id"));
    assert!(!commands.accepts_response(&remote, 8, "old-boot", "same-id"));

    // Retire after a real server commit but before consuming the response: its result is unknown.
    let cancelled = commands.disconnect(&remote);
    assert_eq!(cancelled, ["queued-mutation", "same-id"]);
    let event = tokio::time::timeout(TIMEOUT, remote_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let transport::EndpointReaderEvent::Message {
        endpoint_id,
        generation,
        message,
    } = event
    else {
        panic!("real response expected")
    };
    let ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk,
        data,
    } = *message
    else {
        panic!("response chunk expected")
    };
    assert!(commands
        .receive_chunk(
            &endpoint_id,
            generation,
            &boot_id,
            &request_id,
            final_chunk,
            data
        )
        .unwrap()
        .is_none());
    registry.disconnect(&remote);
    // Consume the old socket's actual retirement before waiting for its replacement.
    loop {
        let event = next_event(&mut server).await;
        let retired = matches!(event, EndpointTransportEvent::Disconnected { client_id } if client_id == remote_id);
        server.handle_endpoint_event(event);
        if retired {
            break;
        }
    }
    assert!(!server.endpoint_clients.contains_key(&remote_id));
    assert!(registry.accepts(&local, 7));
    assert_eq!(registry.active_id(), &local);
    assert_eq!(server.app.state.active, Some(0));
    assert!(server.endpoint_clients.contains_key(&local_id));
    // The bridge-independent replacement receives a new generation and no queued mutations.
    let (replacement_id, mut replacement) = connect_with_interest(&mut server, false).await;
    assert_ne!(replacement_id, remote_id);
    server.stream_endpoint_views();
    let (replacement_snapshot, _) = receive_jobs(&mut replacement);
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::channel(8);
    let replacement_writer = transport::start(
        replacement,
        (),
        remote.clone(),
        9,
        &negotiation(),
        replacement_tx,
    )
    .unwrap();
    registry.insert(remote.clone(), replacement_writer, 9, negotiation(), false);
    assert!(commands.send_next(&remote, &mut registry).is_empty());
    assert!(!commands.accepts_response(&remote, 8, &replacement_snapshot.boot_id, "same-id"));
    commands.enqueue(
        local.clone(),
        7,
        local_snapshot.boot_id.clone(),
        Box::new(EndpointCommandRequest {
            id: "disabled-method".into(),
            method: "server.live_handoff".into(),
            params: serde_json::json!({}),
        }),
    );
    assert_eq!(
        commands.send_next(&local, &mut registry),
        ["disabled-method"]
    );
    assert!(registry.accepts(&local, 7));
    // A new explicit Local read proves the healthy lane still makes progress after retirement.
    commands.enqueue(
        local.clone(),
        7,
        local_snapshot.boot_id,
        Box::new(EndpointCommandRequest {
            id: "after-retirement".into(),
            method: "workspace.get".into(),
            params: serde_json::json!({"workspace_id":workspace}),
        }),
    );
    assert!(commands.send_next(&local, &mut registry).is_empty());
    dispatch_input(&mut server).await;
    let after = command_result(&mut local_rx, &mut commands).await;
    let label = after.result.unwrap()["workspace"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(label == "LOCAL-COMMITTED" || label == "REMOTE-COMMITTED");
    assert_ne!(label, "NEVER-REPLAY");
    server.app.state.assert_invariants_for_test();
    drop(registry);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn endpoint_split_actual_socket_creates_owned_pty_without_stealing_other_viewer() {
    let mut server = test_headless_server();
    server.app.state.default_shell = "/bin/sh".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces = vec![
        crate::workspace::Workspace::test_new("local"),
        crate::workspace::Workspace::test_new("viewer"),
    ];
    for workspace in &mut server.app.state.workspaces {
        for tab in &mut workspace.tabs {
            tab.events = server.app.event_tx.clone();
            tab.render_notify = server.app.render_notify.clone();
            tab.render_dirty = server.app.render_dirty.clone();
        }
    }
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let (first_id, mut first) = connect_with_interest(&mut server, true).await;
    let (other_id, mut other) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let _ = receive_view(&mut first);
    let _ = receive_view(&mut other);
    let other_location = server.endpoint_clients[&other_id].location.clone();
    let source = server.app.state.workspaces[1].tabs[0].root_pane;
    let source = server.app.public_pane_id(1, source).unwrap();
    let before = server.app.state.workspaces[1].tabs[0].panes.len();
    protocol::write_message(&mut first, &ClientMessage::ClientShellEndpointRequest {
        boot_id: server.endpoint_boot_id.clone(),
        request: serde_json::json!({"id":"viewer-split","method":"pane.split","params":{"target_pane_id":source,"direction":"right","focus":true}}).to_string(),
    }).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        data,
        final_chunk: true,
        ..
    } = receive(&mut first)
    else {
        panic!("split response expected");
    };
    let response: api::schema::SuccessResponse = serde_json::from_slice(&data).unwrap();
    let api::schema::ResponseResult::PaneInfo { pane } = response.result else {
        panic!("created pane expected");
    };
    assert!(pane.focused);
    assert_ne!(pane.pane_id, source);
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(server.endpoint_clients[&other_id].location, other_location);
    assert_eq!(
        server.endpoint_clients[&first_id]
            .location
            .focused_workspace_id
            .as_deref(),
        Some(pane.workspace_id.as_str())
    );
    assert_eq!(
        server.app.state.workspaces[1].tabs[0].panes.len(),
        before + 1
    );
    let runtime = server
        .app
        .terminal_runtimes
        .values()
        .find(|runtime| runtime.child_pid().is_some())
        .unwrap();
    assert_ne!(runtime.child_pid().unwrap(), std::process::id());
    runtime
        .send_paste("printf 'SPLIT-OWNED-PTY\\n'\n".into())
        .await
        .unwrap();
    let terminal = server
        .app
        .terminal_runtimes
        .iter()
        .find(|(_, runtime)| runtime.child_pid().is_some())
        .unwrap()
        .0
        .clone();
    wait_output(&server, &terminal, "\nSPLIT-OWNED-PTY\n").await;
    server.stream_endpoint_views();
    let (snapshot, before_swap_surface) = receive_view(&mut first);
    assert_eq!(
        snapshot.focused_pane_id.as_deref(),
        Some(pane.pane_id.as_str())
    );
    let (snapshot, _) = receive_view(&mut other);
    assert_eq!(
        snapshot.focused_workspace_id,
        other_location.focused_workspace_id
    );
    let source_rect = before_swap_surface
        .panes
        .iter()
        .find(|value| value.pane_id == source)
        .unwrap()
        .rect;
    let created_rect = before_swap_surface
        .panes
        .iter()
        .find(|value| value.pane_id == pane.pane_id)
        .unwrap()
        .rect;
    let other_surface = server.endpoint_clients[&other_id].surface.clone();
    protocol::write_message(&mut first, &ClientMessage::ClientShellEndpointRequest {
        boot_id: server.endpoint_boot_id.clone(),
        request: serde_json::json!({"id":"viewer-swap","method":"pane.swap","params":{"source_pane_id":pane.pane_id,"target_pane_id":source}}).to_string(),
    }).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        data,
        final_chunk: true,
        ..
    } = receive(&mut first)
    else {
        panic!("swap response expected");
    };
    let response: api::schema::SuccessResponse = serde_json::from_slice(&data).unwrap();
    let api::schema::ResponseResult::PaneSwap { swap } = response.result else {
        panic!("swap result expected");
    };
    assert!(swap.changed);
    assert_eq!(swap.source_pane_id, pane.pane_id);
    assert_eq!(swap.target_pane_id.as_deref(), Some(source.as_str()));
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(server.endpoint_clients[&other_id].location, other_location);
    server.stream_endpoint_views();
    let (snapshot, surface) = receive_view(&mut first);
    assert_eq!(
        snapshot.focused_pane_id.as_deref(),
        Some(pane.pane_id.as_str())
    );
    assert_eq!(
        surface
            .panes
            .iter()
            .find(|value| value.pane_id == source)
            .unwrap()
            .rect,
        created_rect
    );
    assert_eq!(
        surface
            .panes
            .iter()
            .find(|value| value.pane_id == pane.pane_id)
            .unwrap()
            .rect,
        source_rect
    );
    assert_eq!(server.endpoint_clients[&other_id].surface, other_surface);
    set_surface(&mut server, &mut other, false).await;
    let layout_before = server.app.state.workspaces[1].tabs[0].layout.pane_ids();
    protocol::write_message(&mut other, &ClientMessage::ClientShellEndpointRequest {
        boot_id: server.endpoint_boot_id.clone(),
        request: serde_json::json!({"id":"inactive-swap","method":"pane.swap","params":{"source_pane_id":source,"target_pane_id":pane.pane_id}}).to_string(),
    }).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        data,
        final_chunk: true,
        ..
    } = receive(&mut other)
    else {
        panic!("inactive swap error expected");
    };
    let response: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(response["error"]["code"], "surface_inactive");
    assert_eq!(
        server.app.state.workspaces[1].tabs[0].layout.pane_ids(),
        layout_before
    );
    let public_focus = server.app.state.workspaces[1].tabs[0].layout.focused();
    let first_location = server.endpoint_clients[&first_id].location.clone();
    for arrangement in [
        "equalize",
        "cycle",
        "rotate_forward",
        "rotate_backward",
        "move_left",
        "move_right",
        "move_up",
        "move_down",
    ] {
        let response = endpoint_crud_request(
            &mut server,
            &mut first,
            arrangement,
            "pane.arrange",
            serde_json::json!({"pane_id":pane.pane_id,"arrangement":arrangement}),
        )
        .await;
        let api::schema::ResponseResult::PaneLayout { layout } = response else {
            panic!("arrangement response");
        };
        assert_eq!(layout.focused_pane_id, pane.pane_id);
        assert_eq!(layout.panes.iter().filter(|value| value.focused).count(), 1);
        assert_eq!(
            server.app.state.workspaces[1].tabs[0].layout.focused(),
            public_focus
        );
        assert_eq!(server.app.state.active, Some(0));
        assert_eq!(server.endpoint_clients[&first_id].location, first_location);
        assert_eq!(server.endpoint_clients[&other_id].location, other_location);
        let ids = server.app.state.workspaces[1].tabs[0]
            .layout
            .pane_ids()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids, layout_before.iter().copied().collect());
        server.app.state.assert_invariants_for_test();
    }
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn endpoint_jobs_live_sqlite_inactive_view_updates_and_owning_server_caller_resolution() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![
        crate::workspace::Workspace::test_new("first"),
        crate::workspace::Workspace::test_new("second"),
    ];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let pane = server.app.state.workspaces[1].tabs[0].root_pane;
    let caller = format!("p{}", pane.raw());
    let workspace_id = server.app.state.workspaces[1].id.clone();
    let path = server.client_socket_path.with_file_name("endpoint-jobs.db");
    let store = crate::job::JobStore::open_at(path.clone()).unwrap();
    for (id, caller_pane) in [("owned-job", caller.clone()), ("closed-job", "p0".into())] {
        store
            .insert(&crate::job::JobRecord {
                id: id.into(),
                label: "actual SQLite row".into(),
                command: "printf 'owned'".into(),
                cwd: "/endpoint-owned/path".into(),
                caller_pane,
                caller_agent: format!("{id}-caller"),
                completion: "summary".into(),
                status: "queued".into(),
                runner_pid: None,
                exit_code: None,
                started_unix_ms: None,
                finished_unix_ms: None,
                log_path: "/endpoint-owned/log".into(),
            })
            .unwrap();
    }
    store
        .mark_running("owned-job", std::process::id(), 1)
        .unwrap();
    server.handle_internal_event_with_forwarding(AppEvent::JobsRefreshed {
        jobs: store.list_recent(2).unwrap(),
        dead_runner_pids: std::collections::HashSet::new(),
    });
    let (id, mut stream) = connect_with_interest(&mut server, false).await;
    server.stream_endpoint_views();
    let (before, jobs) = receive_jobs(&mut stream);
    let owned = jobs.jobs.iter().find(|job| job.id == "owned-job").unwrap();
    assert_eq!(owned.caller_pane, caller);
    assert_eq!(owned.workspace_id.as_deref(), Some(workspace_id.as_str()));
    assert_eq!(owned.status, "running");
    assert_eq!(owned.runner_alive, Some(true));
    assert!(jobs
        .jobs
        .iter()
        .find(|job| job.id == "closed-job")
        .unwrap()
        .workspace_id
        .is_none());
    assert!(server.endpoint_clients[&id].surface.is_none());
    assert_eq!(server.app.state.active, Some(0));
    server.handle_internal_event_with_forwarding(AppEvent::JobsRefreshed {
        jobs: store.list_recent(2).unwrap(),
        dead_runner_pids: [std::process::id()].into_iter().collect(),
    });
    server.stream_endpoint_views();
    let (_, jobs) = receive_jobs(&mut stream);
    let owned = jobs.jobs.iter().find(|job| job.id == "owned-job").unwrap();
    assert_eq!(owned.status, "running");
    assert_eq!(owned.runner_alive, Some(false));
    store.mark_finished("owned-job", Some(17), 2).unwrap();
    server.handle_internal_event_with_forwarding(AppEvent::JobsRefreshed {
        jobs: store.list_recent(2).unwrap(),
        dead_runner_pids: [std::process::id()].into_iter().collect(),
    });
    server.stream_endpoint_views();
    let (after, jobs) = receive_jobs(&mut stream);
    assert!(
        after.revision > before.revision,
        "job-only change invalidates the exact projection"
    );
    let owned = jobs.jobs.iter().find(|job| job.id == "owned-job").unwrap();
    assert_eq!(owned.exit_code, Some(17));
    assert_eq!(owned.status, "exited");
    assert_eq!(owned.runner_alive, None);
    assert_eq!(owned.workspace_id.as_deref(), Some(workspace_id.as_str()));
    assert!(server.endpoint_clients[&id].surface.is_none());
    assert_eq!(server.app.state.active, Some(0));
    protocol::write_message(&mut stream, &ClientMessage::Detach).unwrap();
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn endpoint_workspace_and_tab_create_actual_socket_keep_other_viewer_and_local_focus() {
    let mut server = test_headless_server();
    server.app.state.default_shell = "/bin/sh".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("original")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let (id, mut stream) = connect_with_interest(&mut server, true).await;
    let (other, mut other_stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    receive_view(&mut stream);
    receive_view(&mut other_stream);
    let other_location = server.endpoint_clients[&other].location.clone();
    let local = server.app.state.workspaces[0].id.clone();
    let cwd = std::env::temp_dir();
    let workspace_response = endpoint_crud_request(
        &mut server,
        &mut stream,
        "create-workspace",
        "workspace.create",
        serde_json::json!({"cwd":cwd,"label":"created","focus":true}),
    )
    .await;
    let api::schema::ResponseResult::WorkspaceCreated {
        workspace,
        root_pane,
        ..
    } = workspace_response
    else {
        panic!("workspace create response");
    };
    assert!(workspace.focused);
    assert!(root_pane.focused);
    assert_eq!(
        server.app.state.workspaces[server.app.state.active.unwrap()].id,
        local
    );
    assert_eq!(server.endpoint_clients[&other].location, other_location);
    assert_eq!(
        server.endpoint_clients[&id]
            .location
            .focused_workspace_id
            .as_deref(),
        Some(workspace.workspace_id.as_str())
    );
    assert_eq!(workspace.next_public_tab_number, Some(2));
    let tab_response = endpoint_crud_request(
        &mut server,
        &mut stream,
        "create-tab",
        "tab.create",
        serde_json::json!({"workspace_id":workspace.workspace_id,"cwd":cwd,"focus":true}),
    )
    .await;
    let api::schema::ResponseResult::TabCreated { tab, root_pane } = tab_response else {
        panic!("tab create response");
    };
    assert!(tab.focused);
    assert!(root_pane.focused);
    assert_eq!(tab.number, 2);
    assert_eq!(
        server.endpoint_clients[&id].location.focused_tab_id(),
        Some(tab.tab_id.as_str())
    );
    assert_eq!(server.endpoint_clients[&other].location, other_location);
    assert_eq!(
        server.app.state.workspaces[server.app.state.active.unwrap()].id,
        local
    );
    let index = server
        .app
        .parse_workspace_id(&workspace.workspace_id)
        .unwrap();
    assert_eq!(server.app.state.workspaces[index].next_public_tab_number, 3);
    let duplicated = endpoint_crud_request(
        &mut server,
        &mut stream,
        "duplicate-workspace",
        "workspace.duplicate",
        serde_json::json!({"workspace_id":workspace.workspace_id,"focus":true}),
    )
    .await;
    let api::schema::ResponseResult::WorkspaceCreated {
        workspace: duplicated,
        ..
    } = duplicated
    else {
        panic!("workspace duplicate response");
    };
    assert_ne!(duplicated.workspace_id, workspace.workspace_id);
    let duplicate_index = server
        .app
        .parse_workspace_id(&duplicated.workspace_id)
        .unwrap();
    assert_eq!(server.app.state.workspaces[duplicate_index].tabs.len(), 2);
    assert_eq!(server.app.state.workspaces[duplicate_index].active_tab, 1);
    assert_eq!(
        server.app.state.workspaces[duplicate_index].next_public_tab_number,
        3
    );
    assert_eq!(
        server.endpoint_clients[&id].location.focused_tab_id(),
        Some(duplicated.active_tab_id.as_str())
    );
    assert_eq!(server.endpoint_clients[&other].location, other_location);
    assert_eq!(
        server.app.state.workspaces[server.app.state.active.unwrap()].id,
        local
    );
    let source_panes = server.app.state.workspaces[index]
        .tabs
        .iter()
        .flat_map(|tab| tab.layout.pane_ids())
        .collect::<std::collections::HashSet<_>>();
    assert!(server.app.state.workspaces[duplicate_index]
        .tabs
        .iter()
        .flat_map(|tab| tab.layout.pane_ids())
        .all(|pane| !source_panes.contains(&pane)));
    for tab in &server.app.state.workspaces[duplicate_index].tabs {
        for pane in tab.panes.values() {
            assert_eq!(
                server.app.state.terminals[&pane.attached_terminal_id]
                    .cwd
                    .canonicalize()
                    .unwrap(),
                cwd.canonicalize().unwrap()
            );
        }
    }
    let owned_pids: Vec<_> = server
        .app
        .terminal_runtimes
        .values()
        .filter_map(|runtime| runtime.child_pid())
        .collect();
    assert_eq!(owned_pids.len(), 4);
    assert!(owned_pids.iter().all(|pid| *pid != std::process::id()));
    set_surface(&mut server, &mut stream, false).await;
    let before = server.app.state.workspaces.len();
    protocol::write_message(&mut stream, &ClientMessage::ClientShellEndpointRequest { boot_id: server.endpoint_boot_id.clone(), request: serde_json::json!({"id":"inactive-create","method":"workspace.create","params":{"cwd":cwd,"focus":true}}).to_string() }).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk { data, .. } = receive(&mut stream) else {
        panic!("inactive response");
    };
    let error: api::schema::ErrorResponse = serde_json::from_slice(&data).unwrap();
    assert_eq!(error.error.code, "surface_inactive");
    assert_eq!(server.app.state.workspaces.len(), before);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

async fn endpoint_crud_request(
    server: &mut HeadlessServer,
    stream: &mut crate::ipc::LocalStream,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> api::schema::ResponseResult {
    protocol::write_message(
        stream,
        &ClientMessage::ClientShellEndpointRequest {
            boot_id: server.endpoint_boot_id.clone(),
            request: serde_json::json!({"id":id,"method":method,"params":params}).to_string(),
        },
    )
    .unwrap();
    assert!(dispatch_input(server).await);
    let ServerMessage::ClientShellEndpointResponseChunk {
        data,
        final_chunk: true,
        ..
    } = receive(stream)
    else {
        panic!("CRUD response");
    };
    serde_json::from_slice::<api::schema::SuccessResponse>(&data)
        .unwrap()
        .result
}

#[tokio::test]
async fn endpoint_context_agent_actual_socket_rejects_nonowner_without_launching_ai() {
    let mut server = test_headless_server();
    let background = owned_activation_pty(&mut server, "AGENT-OWNER-SOURCE").await;
    let pid = server
        .app
        .terminal_runtimes
        .get(&background)
        .unwrap()
        .child_pid()
        .unwrap();
    let (viewer, mut stream) = connect_with_interest(&mut server, true).await;
    let (other, mut other_stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (initial, _) = receive_view(&mut stream);
    let _ = receive_view(&mut other_stream);
    let source = initial.focused_pane_id.unwrap();
    let public = server.app.state.current_pane_focus_target();
    let location = server.endpoint_clients[&viewer].location.clone();
    let other_location = server.endpoint_clients[&other].location.clone();
    let pane_count = server.app.state.workspaces[0].tabs[0].panes.len();
    for (pane, agent, code) in [
        ("not-the-owner", "claude", "pane_not_found"),
        ("not-the-owner", "codex", "pane_not_found"),
        ("not-the-owner", "agy", "pane_not_found"),
        ("not-the-owner", "grok", "pane_not_found"),
        ("not-the-owner", "letta", "pane_not_found"),
        ("not-the-owner", "qwen", "pane_not_found"),
        (source.as_str(), "unsupported-agent", "invalid_agent"),
    ] {
        protocol::write_message(&mut stream, &ClientMessage::ClientShellEndpointRequest {
            boot_id: server.endpoint_boot_id.clone(),
            request: serde_json::json!({"id":"agent-guard","method":"pane.agent.start","params":{"pane_id":pane,"agent":agent}}).to_string(),
        }).unwrap();
        assert!(dispatch_input(&mut server).await);
        let ServerMessage::ClientShellEndpointResponseChunk {
            data,
            final_chunk: true,
            ..
        } = receive(&mut stream)
        else {
            panic!("owner rejection expected")
        };
        let response: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(response["error"]["code"], code);
        assert_eq!(
            server.app.state.workspaces[0].tabs[0].panes.len(),
            pane_count
        );
        assert_eq!(server.endpoint_clients[&viewer].location, location);
        assert_eq!(server.endpoint_clients[&other].location, other_location);
        assert_eq!(server.app.state.current_pane_focus_target(), public);
    }
    server.app.state.assert_invariants_for_test();
    drop(stream);
    drop(other_stream);
    drop(server);
    assert!(!crate::platform::process_exists(pid));
    let root = std::env::temp_dir().join(format!("herdr-agent-owner-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "test_pid":std::process::id(), "child_pid":pid, "rejections":7,
            "scope":"actual socket nonowner six kinds and invalid command; no positive AI launch",
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn endpoint_custom_commands_actual_shell_pane_popup_preserve_owner_and_public_focus() {
    let mut server = test_headless_server();
    let background = owned_activation_pty(&mut server, "CUSTOM-SOURCE").await;
    for tab in &mut server.app.state.workspaces[0].tabs {
        tab.events = server.app.event_tx.clone();
        tab.render_notify = server.app.render_notify.clone();
        tab.render_dirty = server.app.render_dirty.clone();
    }
    let root = std::env::temp_dir().join(format!("herdr-owned-custom-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let (viewer, mut stream) = connect_with_interest(&mut server, true).await;
    let (other, mut other_stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (initial, _) = receive_view(&mut stream);
    let _ = receive_view(&mut other_stream);
    let source = initial.focused_pane_id.unwrap();
    let public = server.app.state.current_pane_focus_target();
    let other_location = server.endpoint_clients[&other].location.clone();
    let mut owned = Vec::new();
    let mut outputs = Vec::new();
    for action in ["shell", "pane", "popup"] {
        let output = root.join(format!("{action}.txt"));
        let command = format!("printf '%s\\n%s\\n%s\\n' \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'; {}", output.display(),
            if action == "shell" { "true" } else { "printf OWNED-CUSTOM-READY; read -r line" });
        let api::schema::ResponseResult::CommandExecuted { pane, popup_terminal_id } = endpoint_crud_request(
            &mut server, &mut stream, action, "pane.command.execute",
            serde_json::json!({"pane_id":source,"command":command,"action":action,"width":50,"height":12}),
        ).await else { panic!("custom command response"); };
        if action == "shell" {
            let child = server
                .app
                .detached_custom_command_children
                .last_mut()
                .unwrap();
            owned.push(child.id());
            assert!(child.wait().unwrap().success());
            assert!(pane.is_none() && popup_terminal_id.is_none());
        } else {
            let (pane_id, terminal_id) = if action == "pane" {
                let pane = pane.unwrap();
                let (_, pane_id) = server.app.parse_pane_id(&pane.pane_id).unwrap();
                assert_eq!(
                    server.endpoint_clients[&viewer].location.focused_pane_id(),
                    Some(pane.pane_id.as_str())
                );
                assert!(server.app.state.workspaces[0].tabs[0].zoomed);
                (
                    pane_id,
                    server.app.state.workspaces[0].tabs[0].panes[&pane_id]
                        .attached_terminal_id
                        .clone(),
                )
            } else {
                assert!(pane.is_none());
                let popup = server.app.state.popup_panes.last().unwrap();
                assert_eq!(popup_terminal_id, Some(popup.terminal_id.to_string()));
                assert_eq!(popup.workspace_id, server.app.state.workspaces[0].id);
                assert_eq!(popup.width, Some(crate::popup_size::PopupSize::Cells(50)));
                assert_eq!(popup.height, Some(crate::popup_size::PopupSize::Cells(12)));
                (popup.pane_id, popup.terminal_id.clone())
            };
            let runtime = server.app.terminal_runtimes.get(&terminal_id).unwrap();
            owned.push(runtime.child_pid().unwrap());
            wait_output(&server, &terminal_id, "OWNED-CUSTOM-READY").await;
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .send_paste("done\n".into())
                .await
                .unwrap();
            tokio::time::timeout(TIMEOUT, async {
                while server.app.find_pane(pane_id).is_some()
                    || server
                        .app
                        .state
                        .popup_panes
                        .iter()
                        .any(|popup| popup.pane_id == pane_id)
                {
                    let event = server.app.event_rx.recv().await.unwrap();
                    server.handle_internal_event_with_forwarding(event);
                }
            })
            .await
            .unwrap();
            assert_eq!(
                server.endpoint_clients[&viewer].location.focused_pane_id(),
                Some(source.as_str())
            );
            if action == "pane" {
                assert!(!server.app.state.workspaces[0].tabs[0].zoomed);
            }
        }
        let actual = std::fs::read_to_string(&output).unwrap();
        let expected = format!(
            "{}\n{}\n{}\n",
            server.app.public_workspace_id(0),
            server.app.public_tab_id(0, 0).unwrap(),
            crate::workspace::pane_env_id_from_public(&source)
        );
        assert_eq!(actual, expected);
        outputs.push(actual);
        assert_eq!(server.app.state.current_pane_focus_target(), public);
        assert_eq!(server.endpoint_clients[&other].location, other_location);
    }
    owned.push(
        server
            .app
            .terminal_runtimes
            .get(&background)
            .unwrap()
            .child_pid()
            .unwrap(),
    );
    server.app.state.assert_invariants_for_test();
    drop(stream);
    drop(other_stream);
    drop(server);
    assert!(owned
        .iter()
        .all(|pid| !crate::platform::process_exists(*pid)));
    std::fs::write(root.join("evidence.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "test_pid":std::process::id(),"child_pids":owned,"env_outputs":outputs,"public_focus":format!("{public:?}"),
        "scope":"actual socket Shell/Pane/Popup owner; no normal client/plugin positive/SSH",
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_editor_actual_pty_preserves_public_focus_owner_env_and_user_selection() {
    use std::os::unix::fs::PermissionsExt;
    let mut server = test_headless_server();
    let background = owned_activation_pty(&mut server, "EDITOR-SOURCE-MARKER").await;
    for tab in &mut server.app.state.workspaces[0].tabs {
        tab.events = server.app.event_tx.clone();
        tab.render_notify = server.app.render_notify.clone();
        tab.render_dirty = server.app.render_dirty.clone();
    }
    let root = std::env::temp_dir().join(format!("herdr-owned-editor-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let facts = root.join("facts.json");
    let editor_script = root.join("editor.py");
    std::fs::write(&editor_script, format!(r#"import os,sys,json
p=sys.argv[1]
json.dump({{"path":p,"mode":os.stat(p).st_mode & 0o777,"content":open(p).read(),"cwd":os.getcwd(),"env":{{k:v for k,v in os.environ.items() if k.startswith('HERDR_')}}}},open({facts:?},'w'))
print('OWNED-EDITOR-READY',flush=True)
input()
"#, facts=facts.to_string_lossy())).unwrap();
    let previous_editor = std::env::var_os("EDITOR");
    std::env::set_var(
        "EDITOR",
        format!("/usr/bin/python3 {}", editor_script.display()),
    );
    let (viewer, mut stream) = connect_with_interest(&mut server, true).await;
    let (other, mut other_stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (initial, _) = receive_view(&mut stream);
    let _ = receive_view(&mut other_stream);
    let source = initial.focused_pane_id.unwrap();
    let public_focus = server.app.state.current_pane_focus_target();
    let public_tab = server.app.state.workspaces[0].active_tab;
    let other_location = server.endpoint_clients[&other].location.clone();
    let before_toast = server.app.state.toast.clone();
    let mut pids = Vec::new();
    for user_changed_focus in [false, true] {
        let api::schema::ResponseResult::PaneInfo { pane } = endpoint_crud_request(
            &mut server,
            &mut stream,
            "editor-open",
            "pane.scrollback.edit",
            serde_json::json!({"pane_id": source}),
        )
        .await
        else {
            panic!("editor pane response");
        };
        let (_, editor_id) = server.app.parse_pane_id(&pane.pane_id).unwrap();
        let terminal = server.app.state.workspaces[0].tabs[0].panes[&editor_id]
            .attached_terminal_id
            .clone();
        let runtime = server.app.terminal_runtimes.get(&terminal).unwrap();
        pids.push(runtime.child_pid().unwrap());
        wait_output(&server, &terminal, "OWNED-EDITOR-READY").await;
        let actual: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&facts).unwrap()).unwrap();
        assert_eq!(actual["mode"], 0o600);
        let path = std::path::PathBuf::from(actual["path"].as_str().unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(actual["content"]
            .as_str()
            .unwrap()
            .contains("EDITOR-SOURCE-MARKER"));
        assert_eq!(
            actual["env"]["HERDR_ACTIVE_WORKSPACE_ID"],
            initial.focused_workspace_id.as_ref().unwrap().as_str()
        );
        assert_eq!(
            actual["env"]["HERDR_ACTIVE_TAB_ID"],
            initial.focused_tab_id.as_ref().unwrap().as_str()
        );
        assert_eq!(
            actual["env"]["HERDR_ACTIVE_PANE_ID"],
            crate::workspace::pane_env_id_from_public(&source)
        );
        assert_eq!(actual["cwd"], actual["env"]["HERDR_ACTIVE_PANE_CWD"]);
        assert!(actual["env"]["HERDR_SOCKET_PATH"].is_string());
        assert!(actual["env"]["HERDR_BIN_PATH"].is_string());
        assert_eq!(server.app.state.current_pane_focus_target(), public_focus);
        assert_eq!(server.app.state.workspaces[0].active_tab, public_tab);
        assert_eq!(server.endpoint_clients[&other].location, other_location);
        assert_eq!(server.app.state.toast, before_toast);
        assert!(server.app.state.workspaces[0].tabs[0].zoomed);
        if user_changed_focus {
            let _ = endpoint_crud_request(
                &mut server,
                &mut stream,
                "editor-user-focus",
                "pane.focus",
                serde_json::json!({"pane_id":source}),
            )
            .await;
        }
        server
            .app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .send_paste("done\n".into())
            .await
            .unwrap();
        tokio::time::timeout(TIMEOUT, async {
            while server.app.find_pane(editor_id).is_some() {
                let event = server.app.event_rx.recv().await.unwrap();
                server.handle_internal_event_with_forwarding(event);
            }
        })
        .await
        .unwrap();
        assert!(!path.exists());
        assert_eq!(
            server.endpoint_clients[&viewer].location.focused_pane_id(),
            Some(source.as_str())
        );
        assert_eq!(
            server.app.state.workspaces[0].tabs[0].zoomed,
            user_changed_focus
        );
        assert_eq!(server.app.state.current_pane_focus_target(), public_focus);
        assert_eq!(server.endpoint_clients[&other].location, other_location);
        server.app.state.workspaces[0].tabs[0].zoomed = false;
    }
    if let Some(editor) = previous_editor {
        std::env::set_var("EDITOR", editor);
    } else {
        std::env::remove_var("EDITOR");
    }
    server.app.state.assert_invariants_for_test();
    pids.push(
        server
            .app
            .terminal_runtimes
            .get(&background)
            .unwrap()
            .child_pid()
            .unwrap(),
    );
    drop(stream);
    drop(other_stream);
    drop(server);
    for pid in &pids {
        assert!(!crate::platform::process_exists(*pid));
    }
    std::fs::write(
        root.join("evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"real endpoint socket and owned editor PTY; not normal client or SSH",
            "owned_pids_absent":pids,"mode":384,"editor_rounds":2,
            "public_focus":format!("{public_focus:?}"),"user_focus_change_preserved":true,
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!(
        "owned editor evidence: {}",
        root.join("evidence.json").display()
    );
}

#[tokio::test]
async fn endpoint_same_tab_viewers_actual_pty_keep_public_pane_focus_and_focus_lifecycle() {
    let mut server = test_headless_server();
    server.app.state.default_shell = "/bin/sh".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("same-tab")];
    server.app.state.active = Some(0);
    let first_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let second_pane = server.app.state.workspaces[0]
        .test_split_pane(first_pane, ratatui::layout::Direction::Horizontal, false)
        .unwrap();
    server.app.state.ensure_test_terminals();
    let first_terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(first_pane)
        .unwrap()
        .clone();
    let second_terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(second_pane)
        .unwrap()
        .clone();
    let first_public = server.app.public_pane_id(0, first_pane).unwrap();
    let second_public = server.app.public_pane_id(0, second_pane).unwrap();
    // Each owned PTY reports the exact bytes it receives, including terminal focus reports.
    for (pane_id, terminal) in [
        (first_pane, &first_terminal),
        (second_pane, &second_terminal),
    ] {
        let command = r#"import os,tty
tty.setraw(0)
os.write(1,b'\x1b[?1004h\x1b[>11u\x1b[?1003h\x1b[?1006hREADY\r\n')
data=b''
while True:
 b=os.read(0,1024)
 data+=b
 gains=data.count(b'\x1b[I')
 losses=data.count(b'\x1b[O')
 os.write(1,b.hex().encode()+f':G{gains}L{losses}\r\n'.encode())
"#;
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::current_dir().unwrap(),
            &[
                "/usr/bin/python3".into(),
                "-u".into(),
                "-c".into(),
                command.into(),
            ],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            server.app.event_tx.clone(),
            server.app.render_notify.clone(),
            server.app.render_dirty.clone(),
        )
        .unwrap();
        assert!(runtime
            .child_pid()
            .is_some_and(|pid| pid != std::process::id()));
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        wait_output(&server, terminal, "READY").await;
    }
    let initial_focus = server.app.handle_api_request(api::schema::Request {
        id: "public-initial-focus".into(),
        method: api::schema::Method::PaneFocus(
            api::schema::PaneTarget {
                pane_id: first_public.clone(),
            }
            .into(),
        ),
    });
    let initial_focus: api::schema::SuccessResponse = serde_json::from_str(&initial_focus).unwrap();
    assert!(
        matches!(initial_focus.result,api::schema::ResponseResult::PaneInfo {pane} if pane.focused)
    );
    let (first_id, mut first) = connect_with_interest(&mut server, true).await;
    let (second_id, mut second) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let _ = receive_view(&mut first);
    let _ = receive_view(&mut second);
    let result = endpoint_crud_request(
        &mut server,
        &mut second,
        "second-focus",
        "pane.focus",
        serde_json::json!({"pane_id":second_public}),
    )
    .await;
    assert!(matches!(result,api::schema::ResponseResult::PaneInfo {pane} if pane.focused));
    server.stream_endpoint_views();
    let (snapshot, surface) = receive_view(&mut second);
    assert_eq!(
        snapshot.focused_pane_id.as_deref(),
        Some(second_public.as_str())
    );
    assert_eq!(
        server.endpoint_clients[&first_id]
            .location
            .focused_pane_id(),
        Some(first_public.as_str())
    );
    assert_eq!(
        server.app.state.workspaces[0].tabs[0].layout.focused(),
        first_pane
    );
    assert_eq!(surface.panes.iter().filter(|pane| pane.focused).count(), 1);
    assert_eq!(
        server.endpoint_clients[&second_id]
            .location
            .focused_tab_id(),
        server.endpoint_clients[&first_id].location.focused_tab_id()
    );
    for (stream, public, terminal, marker) in [
        (&mut first, &first_public, &first_terminal, "A1\n"),
        (&mut second, &second_public, &second_terminal, "B2\n"),
    ] {
        protocol::write_message(
            stream,
            &ClientMessage::ClientShellPaneInput {
                pane_id: public.clone(),
                events: vec![wire::ClientPaneInputEvent::Paste(marker.into())],
            },
        )
        .unwrap();
        assert!(dispatch_input(&mut server).await);
        let hex = marker
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        wait_hex_output(&server, terminal, &hex).await;
    }
    assert!(!server
        .app
        .terminal_runtimes
        .get(&first_terminal)
        .unwrap()
        .visible_text()
        .contains("42320a"));
    assert!(!server
        .app
        .terminal_runtimes
        .get(&second_terminal)
        .unwrap()
        .visible_text()
        .contains("41310a"));
    let key = wire::ClientPaneInputEvent::Key {
        code: wire::ClientKeyCode::Char('a'),
        modifiers: 0,
        kind: wire::ClientKeyKind::Press,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: Some("a".into()),
        tracks_release: true,
        physical_key_id: Some(97),
        windows_record: None,
    };
    let mouse = wire::ClientPaneInputEvent::Mouse {
        kind: wire::ClientMouseKind::Down(wire::ClientMouseButton::Left),
        position: wire::ClientMousePosition::Cell { column: 2, row: 1 },
        geometry: None,
        modifiers: 0,
        lines: 1,
    };
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellPaneInput {
            pane_id: first_public.clone(),
            events: vec![key, mouse],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &first_terminal, "1b5b39373b313a3175").await;
    wait_hex_output(&server, &first_terminal, "1b5b3c303b333b324d").await;
    // Distinct panes in one tab must each receive their own gain/loss.
    for (stream, terminal) in [
        (&mut first, &first_terminal),
        (&mut second, &second_terminal),
    ] {
        protocol::write_message(stream, &ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        assert!(dispatch_input(&mut server).await);
        wait_hex_output(&server, terminal, "1b5b49").await;
    }
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellFocus { focused: false },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &first_terminal, "1b5b4f").await;
    assert!(!server
        .app
        .terminal_runtimes
        .get(&second_terminal)
        .unwrap()
        .visible_text()
        .contains("1b5b4f"));
    // Sharing a pane suppresses duplicate terminal gain/loss until the final focused viewer leaves.
    endpoint_crud_request(
        &mut server,
        &mut first,
        "shared-focus",
        "pane.focus",
        serde_json::json!({"pane_id":second_public}),
    )
    .await;
    set_surface(&mut server, &mut first, false).await;
    wait_hex_output(&server, &first_terminal, "1b5b39373b313a3375").await;
    wait_hex_output(&server, &first_terminal, "1b5b3c303b333b326d").await;
    assert!(!server
        .app
        .terminal_runtimes
        .get(&second_terminal)
        .unwrap()
        .visible_text()
        .contains("1b5b39373b313a3375"));
    set_surface(&mut server, &mut first, true).await;
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellFocus { focused: true },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellFocus { focused: false },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    protocol::write_message(
        &mut second,
        &ClientMessage::ClientShellFocus { focused: false },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &second_terminal, "1b5b4f").await;
    server.stream_endpoint_views();
    let _ = receive_view(&mut first);
    let _ = receive_view(&mut second);
    protocol::write_message(
        &mut second,
        &ClientMessage::ClientShellPaneInput {
            pane_id: second_public.clone(),
            events: vec![wire::ClientPaneInputEvent::TextCommit("FOCUS-END".into())],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &second_terminal, "464f4355532d454e44:G1L1").await;
    server.stream_endpoint_views();
    let (first_snapshot, first_surface) = receive_view(&mut first);
    assert_eq!(
        first_snapshot.focused_pane_id.as_deref(),
        Some(second_public.as_str())
    );
    assert_eq!(
        first_surface
            .panes
            .iter()
            .filter(|pane| pane.focused)
            .map(|pane| pane.pane_id.as_str())
            .collect::<Vec<_>>(),
        [second_public.as_str()]
    );
    let mouse = wire::ClientPaneInputEvent::Mouse {
        kind: wire::ClientMouseKind::Down(wire::ClientMouseButton::Left),
        position: wire::ClientMousePosition::Cell { column: 2, row: 1 },
        geometry: None,
        modifiers: 0,
        lines: 1,
    };
    protocol::write_message(
        &mut first,
        &ClientMessage::ClientShellPaneInput {
            pane_id: second_public.clone(),
            events: vec![mouse],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &second_terminal, "1b5b3c303b333b324d").await;
    protocol::write_message(&mut first, &ClientMessage::Detach).unwrap();
    loop {
        let event = next_event(&mut server).await;
        let gone = matches!(event,EndpointTransportEvent::Disconnected {client_id} if client_id == first_id);
        server.handle_endpoint_event(event);
        if gone {
            break;
        }
    }
    wait_hex_output(&server, &second_terminal, "1b5b3c303b333b326d").await;
    assert!(server.endpoint_clients.contains_key(&second_id));
    let public_result = server.app.handle_api_request(api::schema::Request {
        id: "public-pane-after-viewer".into(),
        method: api::schema::Method::PaneGet(api::schema::PaneTarget {
            pane_id: first_public.clone(),
        }),
    });
    let public_result: api::schema::SuccessResponse = serde_json::from_str(&public_result).unwrap();
    assert!(
        matches!(public_result.result,api::schema::ResponseResult::PaneInfo {pane} if pane.focused)
    );
    assert_eq!(
        server.app.state.workspaces[0].tabs[0].layout.focused(),
        first_pane
    );
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

async fn wait_hex_output(
    server: &HeadlessServer,
    terminal: &crate::terminal::TerminalId,
    hex: &str,
) {
    tokio::time::timeout(PTY_OUTPUT_TIMEOUT, async {
        loop {
            let notified = server.app.render_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            server.app.render_dirty.store(false, Ordering::Release);
            let text = server
                .app
                .terminal_runtimes
                .get(terminal)
                .unwrap()
                .visible_text();
            if text
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
                .contains(hex)
            {
                return;
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "actual hex {hex} missing: {}",
            server
                .app
                .terminal_runtimes
                .get(terminal)
                .unwrap()
                .visible_text()
        )
    });
}

#[tokio::test]
async fn endpoint_copy_text_actual_pty_selection_motion_search_scroll_content_fence() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("owned-copy")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(pane)
        .unwrap()
        .clone();
    let public = server.app.public_pane_id(0, pane).unwrap();
    let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(pane,24,80,std::env::current_dir().unwrap(),
        &["/bin/sh".into(),"-c".into(),"printf '  alpha βeta gamma\\n\\nneedle needle\\nCOPY-READY\\n'; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done".into()],
        &crate::pane::PaneLaunchEnv::default(),crate::pane::AgentDetection::Disabled,0,crate::terminal_theme::TerminalTheme::default(),
        server.app.event_tx.clone(),server.app.render_notify.clone(),server.app.render_dirty.clone()).unwrap();
    let child = runtime.child_pid().unwrap();
    assert_ne!(child, std::process::id());
    server
        .app
        .terminal_runtimes
        .insert(terminal.clone(), runtime);
    wait_output(&server, &terminal, "COPY-READY").await;
    let (_, mut stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (_, surface) = receive_view(&mut stream);
    let revision = surface.panes[0].content_revision;
    let result = endpoint_crud_request(&mut server,&mut stream,"selection","pane.selection.read",serde_json::json!({"pane_id":public,"anchor":{"row":0,"col":2},"cursor":{"row":0,"col":6},"content_revision":revision})).await;
    assert!(matches!(result,api::schema::ResponseResult::PaneSelection {text,..} if text=="alpha"));
    for (motion, row, col) in [
        ("first_non_blank", 0, 2),
        ("next_word_start", 0, 8),
        ("line_end", 0, 17),
        ("next_paragraph", 1, 2),
    ] {
        let result = endpoint_crud_request(&mut server,&mut stream,"motion","pane.copy_motion",serde_json::json!({"pane_id":public,"cursor":{"row":0,"col":2},"motion":motion,"content_revision":revision})).await;
        assert!(
            matches!(result,api::schema::ResponseResult::PaneCopyMotion {cursor,content_revision,..} if cursor.row==row && cursor.col==col && content_revision==revision),
            "{motion}"
        );
    }
    let result = endpoint_crud_request(&mut server,&mut stream,"search","pane.copy_search",serde_json::json!({"pane_id":public,"query":"needle","direction":"forward","cursor":{"row":2,"col":0},"content_revision":revision})).await;
    let api::schema::ResponseResult::PaneCopySearch {
        matches,
        total,
        current,
        current_global,
        ..
    } = result
    else {
        panic!("search response");
    };
    assert_eq!((total, current, current_global), (2, Some(1), Some(1)));
    assert_eq!((matches[1].start.row, matches[1].start.col), (2, 7));
    let result = endpoint_crud_request(&mut server,&mut stream,"previous","pane.copy_search",serde_json::json!({"pane_id":public,"query":"needle","direction":"backward","cursor":{"row":2,"col":7},"previous":matches[1],"content_revision":revision})).await;
    assert!(matches!(
        result,
        api::schema::ResponseResult::PaneCopySearch {
            current_global: Some(0),
            ..
        }
    ));
    protocol::write_message(&mut stream,&ClientMessage::ClientShellEndpointRequest {boot_id:server.endpoint_boot_id.clone(),request:serde_json::json!({"id":"stale","method":"pane.selection.read","params":{"pane_id":public,"anchor":{"row":0,"col":2},"cursor":{"row":0,"col":6},"content_revision":revision+1}}).to_string()}).unwrap();
    assert!(dispatch_input(&mut server).await);
    let ServerMessage::ClientShellEndpointResponseChunk { data, .. } = receive(&mut stream) else {
        panic!("stale response");
    };
    let error: api::schema::ErrorResponse = serde_json::from_slice(&data).unwrap();
    assert_eq!(error.error.code, "stale_content");
    endpoint_crud_request(
        &mut server,
        &mut stream,
        "scroll",
        "pane.scroll",
        serde_json::json!({"pane_id":public,"offset_from_bottom":u64::MAX}),
    )
    .await;
    let metrics = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .scroll_metrics()
        .unwrap();
    assert_eq!(metrics.offset_from_bottom, metrics.max_offset_from_bottom);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
    assert!(!crate::platform::process_exists(child));
}

async fn endpoint_worktree_deferred_response(
    server: &mut HeadlessServer,
    stream: &mut crate::ipc::LocalStream,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<api::schema::ResponseResult, api::schema::ErrorBody> {
    protocol::write_message(
        stream,
        &ClientMessage::ClientShellEndpointRequest {
            boot_id: server.endpoint_boot_id.clone(),
            request: serde_json::json!({"id":id,"method":method,"params":params}).to_string(),
        },
    )
    .unwrap();
    assert!(dispatch_input(server).await);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            tokio::select! {
                event = server.app.event_rx.recv() => { server.app.handle_internal_event(event.unwrap()); }
                event = server.endpoint_event_rx.recv() => {
                    let event = event.unwrap();
                    let complete = matches!(event, EndpointTransportEvent::ApiResponse(_));
                    server.handle_endpoint_event(event);
                    if complete { break; }
                }
            }
        }
    }).await.unwrap();
    let ServerMessage::ClientShellEndpointResponseChunk {
        request_id,
        data,
        final_chunk: true,
        ..
    } = receive(stream)
    else {
        panic!("real deferred response");
    };
    assert_eq!(request_id, id);
    if let Ok(success) = serde_json::from_slice::<api::schema::SuccessResponse>(&data) {
        assert_eq!(success.id, id);
        Ok(success.result)
    } else {
        let failure: api::schema::ErrorResponse = serde_json::from_slice(&data).unwrap();
        assert_eq!(failure.id, id);
        Err(failure.error)
    }
}

#[tokio::test]
async fn endpoint_worktree_actual_git_deferred_create_open_dirty_remove_keep_viewer_and_public_focus(
) {
    let mut server = test_headless_server();
    let owned = server.client_socket_path.parent().unwrap().to_path_buf();
    let repo = owned.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("README"), "owned fixture").unwrap();
    git(&["add", "README"]);
    git(&[
        "-c",
        "user.name=Owned Test",
        "-c",
        "user.email=owned@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    server.app.state.default_shell = "/bin/sh".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.worktree_directory = owned.join("checkouts");
    let index = server
        .app
        .create_workspace_with_options(repo.clone(), true)
        .unwrap();
    let source = server.app.public_workspace_id(index);
    let public_before = server.app.session_snapshot();
    let (first, mut stream) = connect_with_interest(&mut server, true).await;
    let (second, other_stream) = connect_with_interest(&mut server, false).await;
    let other_location = server.endpoint_clients[&second].location.clone();
    let listed = endpoint_crud_request(
        &mut server,
        &mut stream,
        "list",
        "worktree.list",
        serde_json::json!({"workspace_id":source}),
    )
    .await;
    let api::schema::ResponseResult::WorktreeList { source: owner, .. } = listed else {
        panic!("source facts");
    };
    assert_eq!(owner.source_workspace_id.as_deref(), Some(source.as_str()));
    let path = format!(
        "{}{}",
        owner.checkout_path_prefix.unwrap(),
        crate::worktree::branch_to_path_slug("worktree/owned")
    );
    let created = endpoint_worktree_deferred_response(&mut server, &mut stream, "create", "worktree.create", serde_json::json!({"workspace_id":source,"branch":"worktree/owned","path":path,"focus":true})).await.unwrap();
    let api::schema::ResponseResult::WorktreeCreated {
        workspace,
        tab,
        root_pane,
        worktree,
    } = created
    else {
        panic!("created");
    };
    assert!(workspace.focused && tab.focused && root_pane.focused);
    assert_eq!(
        server.endpoint_clients[&first].location.focused_pane_id(),
        Some(root_pane.pane_id.as_str())
    );
    assert_eq!(server.endpoint_clients[&second].location, other_location);
    let public = server.app.session_snapshot();
    assert_eq!(
        (
            public.focused_workspace_id,
            public.focused_tab_id,
            public.focused_pane_id
        ),
        (
            public_before.focused_workspace_id.clone(),
            public_before.focused_tab_id.clone(),
            public_before.focused_pane_id.clone()
        )
    );
    let (_, pane_id) = server.app.parse_pane_id(&root_pane.pane_id).unwrap();
    let child = server
        .app
        .terminal_runtimes
        .get(
            &server.app.state.workspaces[server
                .app
                .parse_workspace_id(&workspace.workspace_id)
                .unwrap()]
            .tabs[0]
                .terminal_id(pane_id)
                .unwrap()
                .clone(),
        )
        .unwrap()
        .child_pid()
        .unwrap();
    assert_ne!(child, std::process::id());
    assert_eq!(worktree.path, path);
    assert!(std::path::Path::new(&path).join("README").is_file());
    let opened = endpoint_crud_request(
        &mut server,
        &mut stream,
        "open",
        "worktree.open",
        serde_json::json!({"workspace_id":source,"path":path,"focus":true}),
    )
    .await;
    let api::schema::ResponseResult::WorktreeOpened {
        already_open,
        root_pane: reopened,
        ..
    } = opened
    else {
        panic!("opened");
    };
    assert!(already_open && reopened.focused);
    assert_eq!(reopened.pane_id, root_pane.pane_id);
    std::fs::write(
        std::path::Path::new(&path).join("README"),
        "dirty owned file",
    )
    .unwrap();
    let error = endpoint_worktree_deferred_response(
        &mut server,
        &mut stream,
        "remove-dirty",
        "worktree.remove",
        serde_json::json!({"workspace_id":workspace.workspace_id,"force":false}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "dirty_worktree_requires_force");
    assert!(std::path::Path::new(&path).is_dir());
    assert!(server
        .app
        .parse_workspace_id(&workspace.workspace_id)
        .is_some());
    let removed = endpoint_worktree_deferred_response(
        &mut server,
        &mut stream,
        "remove-force",
        "worktree.remove",
        serde_json::json!({"workspace_id":workspace.workspace_id,"force":true}),
    )
    .await
    .unwrap();
    assert!(matches!(
        removed,
        api::schema::ResponseResult::WorktreeRemoved { forced: true, .. }
    ));
    assert!(!std::path::Path::new(&path).exists());
    assert!(server
        .app
        .parse_workspace_id(&workspace.workspace_id)
        .is_none());
    assert!(
        String::from_utf8(git(&["branch", "--list", "worktree/owned"]).stdout)
            .unwrap()
            .contains("worktree/owned")
    );
    let public = server.app.session_snapshot();
    assert_eq!(
        (
            public.focused_workspace_id,
            public.focused_tab_id,
            public.focused_pane_id
        ),
        (
            public_before.focused_workspace_id,
            public_before.focused_tab_id,
            public_before.focused_pane_id
        )
    );
    assert_eq!(server.endpoint_clients[&second].location, other_location);
    server.app.state.assert_invariants_for_test();
    drop(stream);
    drop(other_stream);
    drop(server);
    std::fs::remove_dir_all(owned).unwrap();
}

#[tokio::test]
async fn endpoint_popup_actual_socket_owns_input_focus_and_releases_original_terminal() {
    let mut server = test_headless_server();
    let background = owned_activation_pty(&mut server, "POPUP-BACKGROUND").await;
    let public_focus = server.app.state.workspaces[0].focused_pane_id();
    let popup_input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local/reports/remote-management")
        .join(format!("clipboard-popup-input-{}.bin", std::process::id()));
    std::fs::create_dir_all(popup_input.parent().unwrap()).unwrap();
    server
        .app
        .spawn_popup_argv_command(
            &[
                "/usr/bin/python3".into(),
                "-u".into(),
                "-c".into(),
                r#"import os,sys,tty
tty.setraw(0)
output=open(sys.argv[1],"wb",buffering=0)
os.write(1,b'\x1b[?1004h\x1b[>11u\x1b[?1003h\x1b[?1006hPOPUP-READY\r\n')
while True:
 data=os.read(0,1024)
 output.write(data)
 os.write(1,data.hex().encode()+b'\r\n')
"#
                .into(),
                popup_input.display().to_string(),
            ],
            Some(std::env::current_dir().unwrap()),
            Vec::new(),
            Default::default(),
        )
        .unwrap();
    let popup = server.app.state.active_popup_pane().unwrap().clone();
    let popup_pid = server
        .app
        .terminal_runtimes
        .get(&popup.terminal_id)
        .unwrap()
        .child_pid()
        .unwrap();
    let background_pid = server
        .app
        .terminal_runtimes
        .get(&background)
        .unwrap()
        .child_pid()
        .unwrap();
    wait_output(&server, &popup.terminal_id, "POPUP-READY").await;
    let (first, mut stream) = connect_with_interest(&mut server, true).await;
    let popup_facts = |server: &HeadlessServer| {
        let runtime = server
            .app
            .terminal_runtimes
            .get(&popup.terminal_id)
            .unwrap();
        let revision_before = runtime.content_seq();
        let scroll = runtime.scroll_metrics().unwrap();
        serde_json::json!({
            "terminal_id": popup.terminal_id.to_string(),
            "revision_before": revision_before,
            "revision_after": runtime.content_seq(),
            "offset_from_bottom": scroll.offset_from_bottom,
            "max_offset_from_bottom": scroll.max_offset_from_bottom,
            "viewport_rows": scroll.viewport_rows,
        })
    };
    let mut initial_publications = Vec::new();
    // The initial resize changes popup read facts, so the real producer defers that batch.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let notify = server.app.render_notify.clone();
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            server.app.render_dirty.store(false, Ordering::Release);
            let before = popup_facts(&server);
            server.stream_endpoint_views();
            let after = popup_facts(&server);
            let queued = server.endpoint_clients[&first].surface.is_some();
            initial_publications.push(serde_json::json!({
                "before": before, "after": after, "surface_queued": queued,
            }));
            if queued {
                break;
            }
            if before == after {
                notified.await;
            }
        }
    })
    .await
    .unwrap();
    let (snapshot, surface) = receive_view(&mut stream);
    assert_eq!(
        surface.popup.as_ref().unwrap().terminal_id,
        popup.terminal_id.to_string()
    );
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellPaneInput {
            pane_id: snapshot.focused_pane_id.clone().unwrap(),
            events: vec![wire::ClientPaneInputEvent::TextCommit(
                "WRONG-UNDERLYING".into(),
            )],
        },
    )
    .unwrap();
    assert!(!dispatch_input(&mut server).await);
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellPopupInput {
            terminal_id: "stale-opaque-popup".into(),
            events: vec![wire::ClientPaneInputEvent::TextCommit("WRONG-POPUP".into())],
        },
    )
    .unwrap();
    assert!(!dispatch_input(&mut server).await);
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellPopupInput {
            terminal_id: popup.terminal_id.to_string(),
            events: vec![wire::ClientPaneInputEvent::TextCommit("POPUP-INPUT".into())],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &popup.terminal_id, "504f5055502d494e505554").await;
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClipboardImage {
            target: wire::ClientClipboardImageTarget::Popup(popup.terminal_id.to_string()),
            extension: "png".into(),
            data: b"owned-popup-image".to_vec(),
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    let image_path = server.endpoint_clients[&first].staged_clipboard_files()[0].clone();
    assert_eq!(std::fs::read(&image_path).unwrap(), b"owned-popup-image");
    let expected = [
        b"POPUP-INPUT".as_slice(),
        image_path.to_str().unwrap().as_bytes(),
    ]
    .concat();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let notified = server.app.render_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            server.app.render_dirty.store(false, Ordering::Release);
            if std::fs::read(&popup_input).unwrap() == expected {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(10), notified).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "popup actual raw mismatch: {:?}, expected {:?}",
            std::fs::read(&popup_input),
            expected
        )
    });
    let key = wire::ClientPaneInputEvent::Key {
        code: wire::ClientKeyCode::Char('a'),
        modifiers: 0,
        kind: wire::ClientKeyKind::Press,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: Some("a".into()),
        tracks_release: true,
        physical_key_id: Some(97),
        windows_record: None,
    };
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellPopupInput {
            terminal_id: popup.terminal_id.to_string(),
            events: vec![key],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &popup.terminal_id, "1b5b39373b313a3175").await;
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellFocus { focused: true },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    wait_hex_output(&server, &popup.terminal_id, "1b5b49").await;
    set_surface(&mut server, &mut stream, false).await;
    wait_hex_output(&server, &popup.terminal_id, "1b5b39373b313a3375").await;
    wait_hex_output(&server, &popup.terminal_id, "1b5b4f").await;
    assert_eq!(
        server.app.state.workspaces[0].focused_pane_id(),
        public_focus
    );
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(
        server.app.state.popup_panes[0].terminal_id,
        popup.terminal_id
    );
    set_surface(&mut server, &mut stream, true).await;
    server.stream_endpoint_views();
    receive_view(&mut stream);
    let closed = endpoint_crud_request(
        &mut server,
        &mut stream,
        "popup-close-owner",
        "popup.close",
        serde_json::json!({}),
    )
    .await;
    assert!(matches!(closed, api::schema::ResponseResult::Ok {}));
    assert!(!crate::platform::process_exists(popup_pid));
    server.endpoint_clients.remove(&first);
    assert!(!image_path.exists());
    shutdown_test_runtimes(&mut server);
    assert!(!crate::platform::process_exists(background_pid));
    let evidence = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local/reports/remote-management")
        .join(format!(
            "popup-owned-socket-{}-evidence.json",
            std::process::id()
        ));
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    std::fs::write(
        evidence,
        serde_json::to_vec_pretty(&serde_json::json!({
            "initial_publications": initial_publications,
            "popup_pid": popup_pid, "background_pid": background_pid,
            "test_pid": std::process::id(), "popup_terminal": popup.terminal_id,
            "popup_input_hex": "504f5055502d494e505554",
            "key_press_hex": "1b5b39373b313a3175", "released_hex": "1b5b39373b313a3375",
            "public_focus_unchanged": true, "cleanup_process_exists": [false,false],
            "scope": "actual socket input/focus/source-off release; not normal client/job popup"
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn endpoint_popup_product_frontend_actual_mouse_close_and_exact_raw_input() {
    use crate::client::endpoint::{
        handshake::EndpointConnectOptions, runtime::EndpointRuntime, shell::ClientShellState,
        supervisor::EndpointSupervisors, transport, ClientEndpointId, EndpointNegotiation,
        EndpointRegistry,
    };
    let mut server = test_headless_server();
    let mut unused_other = test_headless_server();
    let background = owned_activation_pty(&mut server, "POPUP-CLIENT-BACKGROUND").await;
    let public_focus = server.app.state.workspaces[0].focused_pane_id();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local/reports/remote-management")
        .join(format!("popup-client-mouse-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let capture = root.join("popup-input.bin");
    server
        .app
        .spawn_popup_argv_command(
            &[
                "/usr/bin/python3".into(),
                "-u".into(),
                "-c".into(),
                r#"import os,sys,tty
tty.setraw(0)
with open(sys.argv[1],'wb',buffering=0) as output:
 os.write(1,b'\x1b[?1003h\x1b[?1006hPOPUP-CLIENT-READY\r\n')
 while True:
  data=os.read(0,1024)
  output.write(data)
  os.write(1,data.hex().encode()+b'\r\n')
"#
                .into(),
                capture.display().to_string(),
            ],
            Some(std::env::current_dir().unwrap()),
            Vec::new(),
            Default::default(),
        )
        .unwrap();
    let popup = server.app.state.active_popup_pane().unwrap().clone();
    let popup_pid = server
        .app
        .terminal_runtimes
        .get(&popup.terminal_id)
        .unwrap()
        .child_pid()
        .unwrap();
    let background_pid = server
        .app
        .terminal_runtimes
        .get(&background)
        .unwrap()
        .child_pid()
        .unwrap();
    wait_output(&server, &popup.terminal_id, "POPUP-CLIENT-READY").await;
    let (id, mut stream, welcome) = connect_with_welcome(&mut server, false).await;
    server.stream_endpoint_views();
    let (snapshot, _) = receive_jobs(&mut stream);
    let config = crate::config::Config::default();
    let settings = crate::client::endpoint::chrome::ChromeSettings::from_config(
        &config,
        crate::app::state::Palette::catppuccin(),
        None,
    );
    let mut chrome = crate::client::endpoint::chrome::ClientChrome::new(settings);
    let mut shell = ClientShellState::new();
    shell.begin_connection(&ClientEndpointId::Local, 1);
    shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot);
    let view = chrome.compute_view(&shell, 160, 40);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 8,
        cell_height_px: 16,
        pixel_geometry_exact: true,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let mut runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let negotiation = EndpointNegotiation::new(welcome.methods, welcome.capabilities);
    let writer = transport::start(
        stream,
        (),
        ClientEndpointId::Local,
        1,
        &negotiation,
        runtime.reader_sender(),
    )
    .unwrap();
    runtime
        .endpoints
        .insert(ClientEndpointId::Local, writer, 1, negotiation, false);
    assert!(runtime
        .activate(ClientEndpointId::Local, None, Instant::now())
        .error
        .is_none());
    pump_endpoint_runtime(&mut runtime, &mut server, &mut unused_other, |r| {
        r.input_lease_current()
    })
    .await;
    let mut frontend = crate::client::endpoint::frontend::ClientFrontend::from_runtime(
        runtime,
        &config,
        chrome.settings,
        (160, 40),
        options,
    );
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, 160, 40);
    let geometry =
        crate::popup_size::resolve_popup_geometry(None, None, view.layout.pane_surface).unwrap();
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for (kind, dx, dy) in [
        (MouseEventKind::Down(MouseButton::Left), 0, 0),
        (MouseEventKind::Drag(MouseButton::Left), 1, 1),
        (MouseEventKind::Up(MouseButton::Left), 1, 1),
    ] {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind,
                column: geometry.inner.x + dx,
                row: geometry.inner.y + dy,
                modifiers: KeyModifiers::empty(),
            }))
            .unwrap();
    }
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(KeyCode::Char('Z'), KeyModifiers::empty()),
        ))
        .unwrap();
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste("PASTE".into()))
        .unwrap();
    let expected = b"\x1b[<0;1;1M\x1b[<32;2;2M\x1b[<0;2;2mZPASTE";
    pump_endpoint_runtime(
        &mut frontend.runtime,
        &mut server,
        &mut unused_other,
        |_| std::fs::read(&capture).is_ok_and(|bytes| bytes == expected),
    )
    .await;
    pump_endpoint_runtime(&mut frontend.runtime, &mut server, &mut unused_other, |r| {
        r.input_lease_current()
    })
    .await;
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: geometry.outer.x,
            row: geometry.outer.y,
            modifiers: KeyModifiers::empty(),
        }))
        .unwrap();
    assert_eq!(std::fs::read(&capture).unwrap(), expected);
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        }))
        .unwrap();
    pump_endpoint_runtime(&mut frontend.runtime, &mut server, &mut unused_other, |r| {
        r.shell
            .pane_surface
            .as_ref()
            .is_some_and(|surface| surface.popup.is_none())
    })
    .await;
    assert!(server.app.state.popup_panes.is_empty());
    assert_eq!(
        server.app.state.workspaces[0].focused_pane_id(),
        public_focus
    );
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(std::fs::read(&capture).unwrap(), expected);
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    server.endpoint_clients.remove(&id);
    shutdown_test_runtimes(&mut server);
    for pid in [popup_pid, background_pid] {
        assert!(!crate::platform::process_exists(pid));
    }
    std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&serde_json::json!({
        "popup_pid":popup_pid,"background_pid":background_pid,"test_pid":std::process::id(),
        "actual_popup_raw":capture,"actual_hex":expected.iter().map(|b|format!("{b:02x}")).collect::<String>(),
        "scope":"in-process real socket/product frontend popup application mouse/key/paste/owner close; no background raw zero-byte assertion",
        "public_focus_unchanged":true,"owned_children_process_exists":[false,false]
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_real_pty_incremental_and_inactive_surface_bytes() {
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "INCREMENTAL-READY").await;
    let child = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .child_pid()
        .unwrap();
    let (full_id, mut full, full_welcome) =
        connect_with_surface_encoding(&mut server, true, false).await;
    let (compact_id, mut compact, compact_welcome) =
        connect_with_surface_encoding(&mut server, true, true).await;
    let (inactive_id, mut inactive, _) =
        connect_with_surface_encoding(&mut server, false, true).await;
    assert!(!full_welcome
        .capabilities
        .contains(&crate::protocol::surface_delta::CAPABILITY.into()));
    assert!(compact_welcome
        .capabilities
        .contains(&crate::protocol::surface_delta::CAPABILITY.into()));
    let mut decoder = crate::protocol::surface_reuse::Decoder::new(true, false);
    let mut records = Vec::new();
    fn drain(
        server: &HeadlessServer,
        id: u64,
        stream: &mut crate::ipc::LocalStream,
    ) -> Vec<ServerMessage> {
        // The existing presentation fence follows every accepted render batch.
        server.endpoint_measurement_barrier(id);
        let mut messages = Vec::new();
        loop {
            let message = receive(stream);
            if matches!(&message, ServerMessage::EndpointControl {kind,..} if kind=="test.measurement.barrier")
            {
                break;
            }
            messages.push(message);
        }
        messages
    }
    fn surfaces(messages: &[ServerMessage]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .filter_map(|m| {
                let kind = match m {
                    ServerMessage::PaneSurface(_) => "full",
                    ServerMessage::PaneSurfacePatch(_) => "patch",
                    ServerMessage::EndpointControl { kind, .. }
                        if kind == crate::protocol::surface_delta::MESSAGE_KIND =>
                    {
                        "delta"
                    }
                    ServerMessage::EndpointControl { kind, .. }
                        if kind == crate::protocol::surface_reuse::MESSAGE_KIND =>
                    {
                        "reuse"
                    }
                    _ => return None,
                };
                let mut bytes = Vec::new();
                protocol::write_message(&mut bytes, m).unwrap();
                Some(serde_json::json!({"kind":kind,"framed_bytes":bytes.len()}))
            })
            .collect()
    }
    for phase in ["initial", "pty-output", "unchanged", "second-output"] {
        if matches!(phase, "pty-output" | "second-output") {
            let line = format!("{phase}\n");
            server
                .app
                .terminal_runtimes
                .get(&terminal)
                .unwrap()
                .send_paste(line)
                .await
                .unwrap();
            wait_output(&server, &terminal, &format!("ACK:{phase}")).await;
        }
        let started = Instant::now();
        server.stream_endpoint_views();
        let render_elapsed_ns = started.elapsed().as_nanos();
        let a = drain(&server, full_id, &mut full);
        let b = drain(&server, compact_id, &mut compact);
        let c = drain(&server, inactive_id, &mut inactive);
        let full_frames = surfaces(&a);
        let compact_frames = surfaces(&b);
        let inactive_frames = surfaces(&c);
        assert!(
            inactive_frames.is_empty(),
            "inactive endpoint emitted pane surface"
        );
        if phase == "unchanged" {
            assert!(full_frames.is_empty());
            assert!(compact_frames.is_empty());
        } else {
            assert_eq!(full_frames.len(), 1);
            assert_eq!(compact_frames.len(), 1);
            let expected = a
                .iter()
                .find_map(|m| {
                    if let ServerMessage::PaneSurface(s) = m {
                        Some(s)
                    } else {
                        None
                    }
                })
                .unwrap();
            let decoded = b
                .into_iter()
                .find_map(|m| match decoder.decode(m).unwrap() {
                    ServerMessage::PaneSurface(s) => Some(s),
                    _ => None,
                })
                .unwrap();
            assert_eq!(decoded.frame, expected.frame);
            assert_eq!(decoded.panes, expected.panes);
            assert_eq!(decoded.graphics, expected.graphics);
            if phase != "initial" {
                assert_eq!(compact_frames[0]["kind"], "delta");
                assert!(
                    compact_frames[0]["framed_bytes"].as_u64().unwrap()
                        < full_frames[0]["framed_bytes"].as_u64().unwrap()
                );
            }
        }
        records.push(serde_json::json!({"phase":phase,"full":full_frames,"compact":compact_frames,"inactive":inactive_frames,"render_elapsed_ns":render_elapsed_ns}));
    }
    let cpu = std::process::Command::new("ps")
        .args(["-p", &std::process::id().to_string(), "-o", "time="])
        .output()
        .unwrap();
    let cpu = String::from_utf8(cpu.stdout).unwrap();
    server
        .app
        .terminal_runtimes
        .remove(&terminal)
        .unwrap()
        .shutdown();
    assert!(!crate::platform::process_exists(child));
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("incremental-surface-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&serde_json::json!({"records":records,"process_cpu_time_ps":cpu.trim(),"cpu_scope":"combined owned trace test process; not isolated full/delta improvement claim","test_pid":std::process::id(),"owned_child_pid":child,"inactive_pane_surface_count":0,"unchanged_pane_surface_count":0})).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_viewer_notification_focus_actual_socket_preserves_other_and_public_owner() {
    eprintln!("viewer focus phase=prepare");
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "VIEWER-NOTIFICATION").await;
    let pid = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .child_pid()
        .unwrap();
    let first_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let second_pane = server.app.state.workspaces[0]
        .test_split_pane(first_pane, ratatui::layout::Direction::Horizontal, false)
        .unwrap();
    server.app.state.ensure_test_terminals();
    let first_public = server.app.public_pane_id(0, first_pane).unwrap();
    let second_public = server.app.public_pane_id(0, second_pane).unwrap();
    let public = |server: &HeadlessServer| {
        let w = server.app.state.active.unwrap();
        [
            server.app.public_workspace_id(w),
            server
                .app
                .public_tab_id(w, server.app.state.workspaces[w].active_tab_index())
                .unwrap(),
            server
                .app
                .public_pane_id(w, server.app.state.workspaces[w].focused_pane_id().unwrap())
                .unwrap(),
        ]
    };
    let before = public(&server);
    assert!(before.iter().all(|id| !id.is_empty()));
    let (owner_id, mut owner) = connect_with_interest(&mut server, true).await;
    eprintln!("viewer focus phase=connect-other");
    let (other_id, mut other) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let _ = receive_view(&mut owner);
    let _ = receive_view(&mut other);
    let other_before = server.endpoint_clients[&other_id].location.clone();
    let target = api::schema::PaneFocusParams {
        pane_id: second_public.clone(),
        viewer: Some(api::schema::PaneFocusViewer {
            client_id: owner_id,
            boot_id: server.endpoint_boot_id.clone(),
            endpoint_id: "local".into(),
            generation: 1,
        }),
    };
    let queued: serde_json::Value =
        serde_json::from_str(&server.route_viewer_pane_focus("click", &target)).unwrap();
    assert!(queued.get("error").is_none());
    let ServerMessage::EndpointControl { kind, data } = receive(&mut owner) else {
        panic!("viewer control required");
    };
    assert_eq!(
        kind,
        crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND
    );
    let received: api::schema::PaneFocusParams = serde_json::from_str(&data).unwrap();
    assert_eq!(received, target);
    assert_eq!(public(&server), before);
    eprintln!("viewer focus phase=activation-request");
    let result = endpoint_crud_request(
        &mut server,
        &mut owner,
        "activate-click",
        "pane.focus",
        serde_json::json!({"pane_id":received.pane_id}),
    )
    .await;
    assert!(
        matches!(result, api::schema::ResponseResult::PaneInfo { pane } if pane.pane_id == second_public)
    );
    assert_eq!(
        server.endpoint_clients[&owner_id]
            .location
            .focused_pane_id(),
        Some(second_public.as_str())
    );
    assert_eq!(server.endpoint_clients[&other_id].location, other_before);
    assert_eq!(public(&server), before);
    let mut stale = target.clone();
    stale.viewer.as_mut().unwrap().boot_id.push_str("-old");
    let rejected: serde_json::Value =
        serde_json::from_str(&server.route_viewer_pane_focus("stale", &stale)).unwrap();
    assert_eq!(rejected["error"]["code"], "stale_endpoint");
    eprintln!("viewer focus phase=disconnect-owner");
    server.handle_endpoint_event(EndpointTransportEvent::Disconnected {
        client_id: owner_id,
    });
    let rejected: serde_json::Value =
        serde_json::from_str(&server.route_viewer_pane_focus("detached", &target)).unwrap();
    assert_eq!(rejected["error"]["code"], "viewer_unavailable");
    protocol::write_message(
        &mut other,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
            data: "other-owner-barrier".into(),
        },
    )
    .unwrap();
    eprintln!("viewer focus phase=other-barrier");
    assert!(
        matches!(receive(&mut other), ServerMessage::EndpointControl { kind, data } if kind == crate::protocol::endpoint::HEALTH_PONG_KIND && data == "other-owner-barrier")
    );
    assert_eq!(
        server.endpoint_clients[&other_id]
            .location
            .focused_pane_id(),
        Some(first_public.as_str())
    );
    assert_eq!(public(&server), before);
    server.app.terminal_runtimes.remove(&terminal);
    assert!(!crate::platform::process_exists(pid));
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("qualified-os-socket-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("evidence.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "scope":"real two endpoint sockets; queued callback then original focus request; not normal frontend or OS click",
        "target":target,"received_control":received,"requesting_viewer":owner_id,"other_viewer":other_id,
        "public_before":before,"public_after":public(&server),"other_before":format!("{other_before:?}"),"other_after":format!("{:?}",server.endpoint_clients[&other_id].location),
        "stale_boot":"stale_endpoint","detached":"viewer_unavailable","test_pid":std::process::id(),"owned_child_pid":pid
    })).unwrap()).unwrap();
    eprintln!(
        "viewer owner test={} child={pid} before={before:?} after={:?}",
        std::process::id(),
        public(&server)
    );
}

#[tokio::test]
async fn endpoint_clipboard_image_actual_socket_stages_only_published_pane_and_cleans_disconnect() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("IMAGE-OWNER")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let tab = &server.app.state.workspaces[0].tabs[0];
    let pane = tab.root_pane;
    let terminal = tab.terminal_id(pane).unwrap().clone();
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".local/reports/remote-management")
        .join(format!("clipboard-pane-input-{}.bin", std::process::id()));
    std::fs::create_dir_all(input.parent().unwrap()).unwrap();
    let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
        pane, 24, 80, std::env::current_dir().unwrap(),
        &["/usr/bin/python3".into(), "-u".into(), "-c".into(),
          "import os,sys,tty; tty.setraw(0); output=open(sys.argv[1],'wb',buffering=0); os.write(1,b'IMAGE-OWNER\\r\\n')\nwhile True:\n data=os.read(0,65536)\n output.write(data)\n os.write(1,data.hex().encode()+b'\\r\\n')".into(),
          input.display().to_string()],
        &crate::pane::PaneLaunchEnv::default(), crate::pane::AgentDetection::Disabled, 0,
        crate::terminal_theme::TerminalTheme::default(), server.app.event_tx.clone(),
        server.app.render_notify.clone(), server.app.render_dirty.clone(),
    ).unwrap();
    server
        .app
        .terminal_runtimes
        .insert(terminal.clone(), runtime);
    wait_output(&server, &terminal, "IMAGE-OWNER").await;
    let pid = server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .child_pid()
        .unwrap();
    let before = server.app.state.workspaces[0].focused_pane_id();
    let (client, mut stream) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    let (snapshot, _) = receive_view(&mut stream);
    for target in [
        wire::ClientClipboardImageTarget::Pane("wrong-owner".into()),
        wire::ClientClipboardImageTarget::DirectTerminal,
    ] {
        protocol::write_message(
            &mut stream,
            &ClientMessage::ClipboardImage {
                target,
                extension: "png".into(),
                data: b"wrong-image".to_vec(),
            },
        )
        .unwrap();
        assert!(!dispatch_input(&mut server).await);
        assert!(server.endpoint_clients[&client]
            .staged_clipboard_files()
            .is_empty());
    }
    let pane = snapshot.focused_pane_id.unwrap();
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClipboardImage {
            target: wire::ClientClipboardImageTarget::Pane(pane.clone()),
            extension: "png".into(),
            data: b"owned-pane-image".to_vec(),
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    let path = server.endpoint_clients[&client].staged_clipboard_files()[0].clone();
    assert_eq!(std::fs::read(&path).unwrap(), b"owned-pane-image");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellPaneInput {
            pane_id: pane,
            events: vec![wire::ClientPaneInputEvent::TextCommit("\r".into())],
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    let expected = format!("{}\r", path.display()).into_bytes();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let notified = server.app.render_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            server.app.render_dirty.store(false, Ordering::Release);
            if std::fs::read(&input).unwrap() == expected {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(10), notified).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "actual raw PTY mismatch: {:?}, expected {:?}",
            std::fs::read(&input),
            expected
        )
    });
    assert_eq!(server.app.state.workspaces[0].focused_pane_id(), before);
    server.endpoint_clients.remove(&client);
    assert!(!path.exists());
    shutdown_test_runtimes(&mut server);
    assert!(!crate::platform::process_exists(pid));
}

#[tokio::test]
async fn endpoint_phone_pc_return_reclaims_actual_pty_geometry() {
    let mut server = test_headless_server();
    server.app.state.default_shell = "/bin/sh".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("same-tab")];
    server.app.state.active = Some(0);
    let first_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let second_pane = server.app.state.workspaces[0]
        .test_split_pane(first_pane, ratatui::layout::Direction::Horizontal, false)
        .unwrap();
    server.app.state.ensure_test_terminals();
    let first_terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(first_pane)
        .unwrap()
        .clone();
    let second_terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(second_pane)
        .unwrap()
        .clone();
    // Each owned PTY reports the exact bytes it receives, including terminal focus reports.
    for (pane_id, terminal) in [
        (first_pane, &first_terminal),
        (second_pane, &second_terminal),
    ] {
        let command = r#"import os,tty
tty.setraw(0)
os.write(1,b'\x1b[?1004h\x1b[>11u\x1b[?1003h\x1b[?1006hREADY\r\n')
data=b''
while True:
 b=os.read(0,1024)
 data+=b
 gains=data.count(b'\x1b[I')
 losses=data.count(b'\x1b[O')
 os.write(1,b.hex().encode()+f':G{gains}L{losses}\r\n'.encode())
"#;
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::current_dir().unwrap(),
            &[
                "/usr/bin/python3".into(),
                "-u".into(),
                "-c".into(),
                command.into(),
            ],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            server.app.event_tx.clone(),
            server.app.render_notify.clone(),
            server.app.render_dirty.clone(),
        )
        .unwrap();
        assert!(runtime
            .child_pid()
            .is_some_and(|pid| pid != std::process::id()));
        server
            .app
            .terminal_runtimes
            .insert(terminal.clone(), runtime);
        wait_output(&server, terminal, "READY").await;
    }
    let (first_id, mut first) = connect_with_interest(&mut server, true).await;
    let (second_id, mut second) = connect_with_interest(&mut server, true).await;
    server.stream_endpoint_views();
    receive_view(&mut first);
    receive_view(&mut second);
    let tab = server.endpoint_clients[&first_id]
        .location
        .focused_tab_id()
        .unwrap()
        .to_owned();
    for (owner, cols) in [(first_id, 160), (second_id, 40), (first_id, 160)] {
        let stream = if owner == first_id {
            &mut first
        } else {
            &mut second
        };
        protocol::write_message(
            stream,
            &ClientMessage::ClientShellResize {
                surface_size: wire::ClientSurfaceSize { cols, rows: 24 },
                cell_width_px: 0,
                cell_height_px: 0,
                pixel_mouse: false,
            },
        )
        .unwrap();
        assert!(dispatch_input(&mut server).await);
        assert_eq!(server.endpoint_tab_geometry[&tab], owner);
        server.stream_endpoint_views();
        let (_, surface) = receive_view(stream);
        assert_eq!(surface.frame.width, cols);
    }
    // Returning to a terminal may repeat focus=true without an intervening loss.
    for owner in [first_id, second_id, first_id] {
        let stream = if owner == first_id {
            &mut first
        } else {
            &mut second
        };
        protocol::write_message(stream, &ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        dispatch_input(&mut server).await;
        assert_eq!(server.endpoint_tab_geometry[&tab], owner);
    }
    use interprocess::TryClone as _;
    let readers = [&first, &second].map(|stream| {
        let mut reader = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            while protocol::read_message::<_, ServerMessage>(&mut reader, MAX_GRAPHICS_FRAME_SIZE)
                .is_ok()
            {}
        })
    });
    // Workspace navigation must reclaim the PTY just like a host focus event.
    server
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("away"));
    server.app.state.ensure_test_terminals();
    let home = server.app.public_workspace_id(0);
    let away = server.app.public_workspace_id(1);
    protocol::write_message(
        &mut second,
        &ClientMessage::ClientShellFocus { focused: true },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    server.stream_endpoint_views();
    for workspace in [away, home] {
        protocol::write_message(&mut first, &ClientMessage::ClientShellEndpointRequest {
            boot_id: server.endpoint_boot_id.clone(),
            request: serde_json::json!({"id":"return-space","method":"workspace.focus","params":{"workspace_id":workspace}}).to_string(),
        }).unwrap();
        dispatch_input(&mut server).await;
        server.stream_endpoint_views();
    }
    assert_eq!(server.endpoint_tab_geometry[&tab], first_id);
    let notify = server.app.render_notify.clone();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let changed = notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            server.stream_endpoint_views();
            if server.endpoint_clients[&first_id].surface.is_some() {
                break;
            }
            tokio::select! {
                _ = changed => {},
                event = server.endpoint_event_rx.recv() => {
                    server.handle_endpoint_event(event.expect("endpoint remains connected"));
                }
            }
        }
    })
    .await
    .expect("returning workspace must publish a coherent surface");
    let surface = server.endpoint_clients[&first_id].surface.as_ref().unwrap();
    for pane in &surface.panes {
        let (_, pane_id) = server.app.parse_pane_id(&pane.pane_id).unwrap();
        let terminal = server.app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .unwrap();
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(terminal)
                .unwrap()
                .current_size(),
            (pane.inner_rect.height, pane.inner_rect.width),
            "returning viewer's frame and actual PTY must have the same dimensions"
        );
    }
    server.stream_endpoint_views();
    shutdown_test_runtimes(&mut server);
    drop(first);
    drop(second);
    drop(server);
    for reader in readers {
        reader.join().unwrap();
    }
}

#[tokio::test]
async fn endpoint_viewer_marks_only_its_focused_tab_seen_while_focused() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("endpoint-seen")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let (viewer, mut stream) = connect(&mut server).await;
    server.stream_endpoint_views();
    let tab = server.endpoint_clients[&viewer]
        .location
        .focused_tab_id()
        .expect("viewer focuses the default tab")
        .to_owned();
    let (ws_idx, tab_idx) = server.app.parse_tab_id(&tab).unwrap();
    let set_unseen = |server: &mut HeadlessServer| {
        for pane in server.app.state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .values_mut()
        {
            pane.seen = false;
        }
    };
    let all_seen = |server: &HeadlessServer| {
        server.app.state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .values()
            .all(|pane| pane.seen)
    };

    // A finished agent in the tab the viewer is looking at does not stay "done, unseen",
    // even though the server's own active workspace is not driven by endpoint viewers.
    set_unseen(&mut server);
    server.stream_endpoint_views();
    assert!(all_seen(&server));

    // A terminal that reported losing focus is not looking at the tab.
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellFocus { focused: false },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    set_unseen(&mut server);
    server.stream_endpoint_views();
    assert!(!all_seen(&server));

    // Regaining focus marks it seen again.
    protocol::write_message(
        &mut stream,
        &ClientMessage::ClientShellFocus { focused: true },
    )
    .unwrap();
    dispatch_input(&mut server).await;
    server.stream_endpoint_views();
    assert!(all_seen(&server));
}
