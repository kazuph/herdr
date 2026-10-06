//! Independent Cargo-built processes feed the existing frontend through real socket DI.
use super::*;
use crate::client::endpoint::chrome::ChromeTarget;
use crate::client::endpoint::{transport, ClientEndpointStatus, ResourceKey};
use crate::raw_input::RawInputEvent;
use interprocess::local_socket::traits::Stream as _;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[tokio::test]
async fn endpoint_frontend_viewer_callback_actual_owner_rejects_stale_and_isolates_endpoint() {
    fn frontend() -> ClientFrontend {
        let config: crate::config::Config =
            toml::from_str("[ui.toast]\ndelivery = 'terminal'\n[ui.sound]\nenabled = false\n")
                .unwrap();
        let (palette, _) = crate::app::resolve_effective_theme(
            &crate::app::theme_runtime_config(&config, true),
            None,
        );
        let settings = ChromeSettings::from_config(&config, palette, None);
        let options = EndpointConnectOptions {
            surface_size: wire::ClientSurfaceSize {
                cols: SIZE.0,
                rows: SIZE.1,
            },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: false,
        };
        let mut shell = ClientShellState::new();
        shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
            id: "m-notification-owner".into(),
            label: "Notification owner".into(),
            target: "never-contacted".into(),
            session: "owned".into(),
            enabled: true,
        }]);
        let runtime = EndpointRuntime::new(
            shell,
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options)
    }
    async fn receive_control(frontend: &mut ClientFrontend, wanted: &str) -> Value {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let event = frontend.runtime.reader_events.recv().await.unwrap();
                let update = frontend.runtime.receive(event, Instant::now());
                let payload = update
                    .host_effects
                    .iter()
                    .find_map(|effect| match &effect.message {
                        wire::ServerMessage::EndpointControl { kind, data } if kind == wanted => {
                            Some(serde_json::from_str::<Value>(data).unwrap())
                        }
                        _ => None,
                    });
                frontend.update(update).unwrap();
                frontend.draw().unwrap();
                if let Some(payload) = payload {
                    return payload;
                }
            }
        })
        .await
        .unwrap()
    }
    fn public(server: &OwnedServer) -> Value {
        let snapshot = api(&server.socket, "session.snapshot", json!({}));
        let ids: Vec<_> = ["focused_workspace_id", "focused_tab_id", "focused_pane_id"]
            .iter()
            .map(|key| {
                let id = snapshot["snapshot"][key].as_str().unwrap();
                assert!(!id.is_empty());
                id.to_owned()
            })
            .collect();
        json!(ids)
    }
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("viewer-callback-process-{}", std::process::id()));
    let mut local = OwnedServer::start(root.join("local"), "VIEWER-LOCAL");
    let mut remote = OwnedServer::start_with_settings(
        root.join("remote"),
        "VIEWER-REMOTE",
        "",
        "[ui.toast]\ndelivery = 'terminal'\n",
    );
    assert_eq!(local.pane, remote.pane);
    let extra = api(
        &remote.socket,
        "workspace.create",
        json!({"label":"VIEWER-EXTRA","cwd":remote.root,"focus":false}),
    );
    let extra_pane = extra["root_pane"]["pane_id"].as_str().unwrap().to_owned();
    let local_extra = api(
        &local.socket,
        "workspace.create",
        json!({"label":"LOCAL-EXTRA","cwd":local.root,"focus":false}),
    );
    let local_extra_pane = local_extra["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(local_extra_pane, extra_pane);
    let remote_id = ClientEndpointId::Ssh("m-notification-owner".into());
    let mut owner = frontend();
    let mut other = frontend();
    install(&mut owner, &ClientEndpointId::Local, 1, &local);
    install(&mut owner, &remote_id, 1, &remote);
    install(&mut other, &ClientEndpointId::Local, 1, &remote);
    let update = owner
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    owner.update(update).unwrap();
    pump(&mut owner, |f| {
        f.runtime.input_lease_current()
            && surface_has(f, "READY:VIEWER-LOCAL")
            && f.runtime
                .shell
                .endpoint(&remote_id)
                .is_some_and(|endpoint| endpoint.cache.snapshot().is_some())
    })
    .await;
    for (client, id) in [
        (&mut owner, remote_id.clone()),
        (&mut other, ClientEndpointId::Local),
    ] {
        let update = client.runtime.activate(id, None, Instant::now());
        client.update(update).unwrap();
        pump(client, |f| {
            f.runtime.input_lease_current() && surface_has(f, "READY:VIEWER-REMOTE")
        })
        .await;
    }
    other.dispatch_input(RawInputEvent::OuterFocusLost).unwrap();
    owner.dispatch_input(RawInputEvent::OuterFocusLost).unwrap();
    owner
        .dispatch_input(RawInputEvent::OuterFocusGained)
        .unwrap();
    pump(&mut owner, |f| f.runtime.input_lease_current()).await;
    for (state, seq) in [("working", 1), ("idle", 2)] {
        api(
            &remote.socket,
            "pane.report_agent",
            json!({"pane_id":extra_pane,"source":"custom:owned-reporter","agent":"owned-reporter","state":state,"seq":seq}),
        );
    }
    let notification = receive_control(
        &mut owner,
        crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND,
    )
    .await;
    let viewer_id = notification["viewer_id"].as_u64().unwrap();
    let boot = notification["boot_id"].as_str().unwrap();
    let target = json!({"pane_id":extra_pane,"viewer":{"client_id":viewer_id,"boot_id":boot,"endpoint_id":"m-notification-owner","generation":1}});
    let update = owner
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    owner.update(update).unwrap();
    pump(&mut owner, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:VIEWER-LOCAL")
    })
    .await;
    let owner_before = input::focused_pane(&owner).unwrap();
    let before_local = public(&local);
    let before_remote = public(&remote);
    let other_before = input::focused_pane(&other).unwrap();
    let mut rejected = Vec::new();
    for (field, value) in [("generation", json!(2)), ("endpoint_id", json!("local"))] {
        let mut invalid = target.clone();
        invalid["viewer"][field] = value;
        api(&remote.socket, "pane.focus", invalid.clone());
        let received = receive_control(
            &mut owner,
            crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND,
        )
        .await;
        assert_eq!(received, invalid);
        assert_eq!(
            owner.runtime.endpoints.active_id(),
            &ClientEndpointId::Local
        );
        assert!(surface_has(&owner, "READY:VIEWER-LOCAL"));
        rejected.push(received);
    }
    other
        .dispatch_input(RawInputEvent::OuterFocusGained)
        .unwrap();
    pump(&mut other, |f| f.runtime.input_lease_current()).await;
    for (state, seq) in [("working", 3), ("blocked", 4)] {
        api(
            &remote.socket,
            "pane.report_agent",
            json!({"pane_id":extra_pane,"source":"custom:owned-reporter","agent":"owned-reporter","state":state,"seq":seq}),
        );
    }
    let other_notification = receive_control(
        &mut other,
        crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND,
    )
    .await;
    let other_id = other_notification["viewer_id"].as_u64().unwrap();
    assert_ne!(other_id, viewer_id);
    let mut wrong_viewer = target.clone();
    wrong_viewer["viewer"]["client_id"] = json!(other_id);
    wrong_viewer["pane_id"] = json!(extra_pane);
    api(&remote.socket, "pane.focus", wrong_viewer.clone());
    assert_eq!(
        receive_control(
            &mut other,
            crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND
        )
        .await,
        wrong_viewer
    );
    assert_eq!(input::focused_pane(&other).unwrap(), other_before);
    rejected.push(wrong_viewer);
    api(&remote.socket, "pane.focus", target.clone());
    assert_eq!(
        receive_control(
            &mut owner,
            crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND
        )
        .await,
        target
    );
    pump(&mut owner, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:VIEWER-REMOTE")
    })
    .await;
    assert_eq!(owner.runtime.endpoints.active_id(), &remote_id);
    let owner_after = input::focused_pane(&owner).unwrap();
    assert_eq!(owner_after.id, extra_pane);
    let after_local = public(&local);
    let after_remote = public(&remote);
    assert_eq!(after_local, before_local);
    assert_eq!(after_remote, before_remote);
    let other_after = input::focused_pane(&other).unwrap();
    assert_eq!(other_after, other_before);
    let pids = [
        local.child.as_ref().unwrap().id(),
        remote.child.as_ref().unwrap().id(),
        local.marker_pid("READY:VIEWER-LOCAL:"),
        api(
            &local.socket,
            "pane.process_info",
            json!({"pane_id":local_extra_pane}),
        )["process_info"]["shell_pid"]
            .as_u64()
            .unwrap() as u32,
        remote.marker_pid("READY:VIEWER-REMOTE:"),
        api(
            &remote.socket,
            "pane.process_info",
            json!({"pane_id":extra_pane}),
        )["process_info"]["shell_pid"]
            .as_u64()
            .unwrap() as u32,
    ];
    owner.runtime.endpoints.disconnect(&ClientEndpointId::Local);
    owner.runtime.endpoints.disconnect(&remote_id);
    other.runtime.endpoints.disconnect(&ClientEndpointId::Local);
    local.stop();
    remote.stop();
    for pid in pids {
        assert!(!crate::platform::process_exists(pid));
    }
    std::fs::write(root.join("evidence.json"), serde_json::to_vec_pretty(&json!({"scope":"real two server sockets, same opaque pane ID, receiver stale generation/endpoint rejection and qualified callback; not SSH transport or OS click", "notification":notification,"target":target,"rejected":rejected,"public_local_before":before_local,"public_local_after":after_local,"public_remote_before":before_remote,"public_remote_after":after_remote,"other_notification":other_notification,"owner_before":{"endpoint":format!("{:?}",owner_before.endpoint),"pane_id":owner_before.id},"owner_after":{"endpoint":format!("{:?}",owner_after.endpoint),"pane_id":owner_after.id},"other_before":{"endpoint":format!("{:?}",other_before.endpoint),"pane_id":other_before.id},"other_after":{"endpoint":format!("{:?}",other_after.endpoint),"pane_id":other_after.id},"test_pid":std::process::id(),"owned_pids":pids})).unwrap()).unwrap();
}

const TIMEOUT: Duration = crate::client::LOCAL_HANDSHAKE_READ_TIMEOUT;
// Same owned proof geometry as standalone-product-client-proof.py.
const SIZE: (u16, u16) = (160, 40);

fn cargo_built_binary() -> PathBuf {
    // Cargo puts this unit executable in <target>/<profile>/deps and builds the
    // integration tests' CARGO_BIN_EXE_herdr beside deps. Derive the actual target
    // and profile from this executable, never a release or installed fallback.
    let test = std::env::current_exe().unwrap();
    let deps = test.parent().unwrap();
    assert_eq!(deps.file_name().unwrap(), "deps");
    let binary = deps.parent().unwrap().join("herdr");
    assert!(binary.is_file(), "Cargo-built herdr executable is missing");
    binary
}

fn api(path: &Path, method: &str, params: Value) -> Value {
    let mut stream = crate::ipc::connect_local_stream(path).unwrap();
    stream.set_recv_timeout(Some(TIMEOUT)).unwrap();
    writeln!(
        stream,
        "{}",
        json!({"id":"owned-frontend-proof","method":method,"params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response["result"].clone()
}

struct OwnedServer {
    root: PathBuf,
    socket: PathBuf,
    client_socket: PathBuf,
    child: Option<Child>,
    pane: String,
    workspace: String,
}
impl OwnedServer {
    fn start(root: PathBuf, marker: &str) -> Self {
        Self::start_with_prelude(root, marker, "")
    }
    fn start_with_prelude(root: PathBuf, marker: &str, prelude: &str) -> Self {
        Self::start_with_settings(root, marker, prelude, "")
    }
    fn start_with_settings(root: PathBuf, marker: &str, prelude: &str, settings: &str) -> Self {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let shell = root.join("marker-shell");
        std::fs::write(&shell, format!("#!/bin/sh\n{prelude}\nprintf 'READY:{marker}:%s\\n' \"$$\"\nwhile IFS= read -r line; do printf 'ACK:{marker}:%s:%s\\n' \"$$\" \"$line\"; done\n")).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("config.toml");
        std::fs::write(&config, format!("onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[ui.sound]\nenabled = false\n[worktrees]\ndirectory = {}\n", serde_json::to_string(&shell).unwrap(), serde_json::to_string(&root.join("checkouts")).unwrap()) + settings).unwrap();
        let socket = root
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .join("herdr.sock");
        let client_socket = socket.with_file_name("herdr-client.sock");
        let mut owned = Self {
            root,
            socket,
            client_socket,
            child: None,
            pane: String::new(),
            workspace: String::new(),
        };
        owned.spawn();
        let created = api(
            &owned.socket,
            "workspace.create",
            json!({"label":marker,"cwd":owned.root,"focus":true}),
        );
        owned.pane = created["root_pane"]["pane_id"].as_str().unwrap().into();
        owned.workspace = created["workspace"]["workspace_id"]
            .as_str()
            .unwrap()
            .into();
        owned
    }
    fn spawn(&mut self) {
        let binary = cargo_built_binary();
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("HERDR_")) {
            command.env_remove(name);
        }
        command
            .arg("server")
            .current_dir(&self.root)
            .env("HERDR_SOCKET_PATH", "herdr.sock")
            .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
            .env("HERDR_CONFIG_PATH", self.root.join("config.toml"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("HERDR_DISABLE_SOUND", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if line.contains("client socket:") {
                    let _ = tx.send(());
                }
            }
        });
        self.child = Some(child);
        rx.recv_timeout(TIMEOUT).unwrap();
    }
    fn stop(&mut self) -> u32 {
        let mut child = self.child.take().unwrap();
        let pid = child.id();
        api(&self.socket, "server.stop", json!({}));
        assert!(child.wait().unwrap().success());
        pid
    }
    fn text(&self) -> String {
        api(
            &self.socket,
            "pane.read",
            json!({"pane_id":self.pane,"source":"visible","format":"text"}),
        )["read"]["text"]
            .as_str()
            .unwrap()
            .into()
    }
    fn marker_pid(&self, marker: &str) -> u32 {
        let runtime_pid = api(
            &self.socket,
            "pane.process_info",
            json!({"pane_id":self.pane}),
        )["process_info"]["shell_pid"]
            .as_u64()
            .unwrap() as u32;
        self.text()
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix(marker)?
                    .split(':')
                    .next()?
                    .parse()
                    .ok()
            })
            .find(|pid| *pid == runtime_pid)
            .expect("actual owning shell READY/ACK process ID")
    }
}
impl Drop for OwnedServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Only this test's owned process is stopped, including panic cleanup.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn connect(
    server: &OwnedServer,
    options: EndpointConnectOptions,
) -> (crate::ipc::LocalStream, EndpointNegotiation) {
    let mut stream = crate::ipc::connect_local_stream(&server.client_socket).unwrap();
    let welcome = crate::client::endpoint::handshake::connect(&mut stream, options, true).unwrap();
    (
        stream,
        EndpointNegotiation::new(welcome.methods, welcome.capabilities),
    )
}
fn install(
    frontend: &mut ClientFrontend,
    endpoint: &ClientEndpointId,
    generation: u64,
    server: &OwnedServer,
) {
    let (stream, negotiation) = connect(server, frontend.options);
    assert!(frontend
        .runtime
        .shell
        .begin_connection(endpoint, generation));
    let writer = transport::start(
        stream,
        (),
        endpoint.clone(),
        generation,
        &negotiation,
        frontend.runtime.reader_sender(),
    )
    .unwrap();
    frontend
        .runtime
        .endpoints
        .insert(endpoint.clone(), writer, generation, negotiation, false);
}
fn pane_context_item(frontend: &mut ClientFrontend, pane_id: &str, item: &str) {
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let pane = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .unwrap()
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .unwrap();
    let x = view.layout.pane_surface.x + pane.inner_rect.x;
    let y = view.layout.pane_surface.y + pane.inner_rect.y;
    let snapshot = frontend
        .runtime
        .shell
        .endpoint(frontend.runtime.endpoints.active_id())
        .unwrap()
        .cache
        .snapshot()
        .unwrap();
    let runtime_pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .unwrap();
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == runtime_pane.tab_id)
        .unwrap();
    let facts = crate::app::state::ContextMenuFacts::Pane {
        has_manual_label: runtime_pane.label.is_some(),
        has_layout_actions: snapshot
            .panes
            .iter()
            .filter(|p| p.tab_id == runtime_pane.tab_id)
            .count()
            > 1,
        is_zoomed: tab.zoomed,
    };
    let items = facts.items();
    let index = items
        .iter()
        .position(|candidate| *candidate == item)
        .unwrap();
    let rect = crate::app::context_menu_rect_from(
        ratatui::layout::Rect::new(0, 0, SIZE.0, SIZE.1),
        x,
        y,
        &items,
    );
    for (button, column, row) in [
        (crossterm::event::MouseButton::Right, x, y),
        (
            crossterm::event::MouseButton::Left,
            rect.x + 1,
            rect.y + 1 + index as u16,
        ),
    ] {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(button),
                    column,
                    row,
                    modifiers: crossterm::event::KeyModifiers::empty(),
                },
            ))
            .unwrap();
        assert!(frontend.context.is_some());
        assert!(frontend.modal.is_none());
    }
}

fn workspace_context_item(frontend: &mut ClientFrontend, target: ResourceKey, item: &str) {
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let hit = view
        .hits
        .iter()
        .find(|hit| matches!(&hit.target, ChromeTarget::Workspace(key) if key == &target))
        .unwrap();
    let (x, y) = (hit.rect.x, hit.rect.y);
    frontend
        .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
            column: x,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .unwrap();
    assert!(frontend.context.is_some());
    let snapshot = frontend
        .runtime
        .shell
        .endpoint(&target.endpoint)
        .unwrap()
        .cache
        .snapshot()
        .unwrap();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == target.id)
        .unwrap();
    let owner = frontend.runtime.shell.endpoint(&target.endpoint).unwrap();
    let git = owner
        .cache
        .workspace_facts(owner.generation.unwrap(), &target.id)
        .and_then(|facts| facts.git_space.as_ref());
    let facts = match workspace
        .worktree
        .as_ref()
        .or(git.filter(|space| !space.is_linked_worktree))
    {
        None => crate::app::state::ContextMenuFacts::Workspace {},
        Some(worktree) => crate::app::state::ContextMenuFacts::GitWorkspace {
            is_linked_worktree: worktree.is_linked_worktree,
            has_worktree_children: workspace.worktree.is_some()
                && !worktree.is_linked_worktree
                && snapshot
                    .workspaces
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .worktree
                            .as_ref()
                            .is_some_and(|candidate| candidate.key == worktree.key)
                    })
                    .count()
                    >= 2,
            collapsed: false,
        },
    };
    let items = facts.items();
    let index = items
        .iter()
        .position(|candidate| *candidate == item)
        .unwrap();
    let rect = crate::app::context_menu_rect_from(
        ratatui::layout::Rect::new(0, 0, SIZE.0, SIZE.1),
        x,
        y,
        &items,
    );
    frontend
        .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x + 1,
            row: rect.y + 1 + index as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .unwrap();
}

async fn pump(frontend: &mut ClientFrontend, ready: impl Fn(&ClientFrontend) -> bool) {
    let result = tokio::time::timeout(TIMEOUT, async {
        while !ready(frontend) {
            let event = frontend.runtime.reader_events.recv().await.unwrap();
            let update = frontend.runtime.receive(event, Instant::now());
            frontend.update(update).unwrap();
            frontend.draw().unwrap();
        }
    })
    .await;
    if result.is_err() {
        let endpoint = frontend
            .runtime
            .shell
            .endpoint(frontend.runtime.endpoints.active_id());
        eprintln!(
            "pump timeout: lease={} size={:?} snapshot={:?} surface={:?}",
            frontend.runtime.input_lease_current(),
            frontend.options.surface_size,
            endpoint.and_then(|endpoint| endpoint.cache.snapshot()),
            frontend.runtime.shell.pane_surface,
        );
    }
    result.unwrap();
}
fn click(frontend: &mut ClientFrontend, endpoint: &ClientEndpointId, workspace: &str) {
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let hit = view.hits.iter().find(|hit| matches!(&hit.target, super::super::chrome::ChromeTarget::Workspace(key) if key.endpoint == *endpoint && key.id == workspace)).unwrap();
    let (column, row) = (hit.rect.x, hit.rect.y);
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        ))
        .unwrap();
}
fn surface_has(frontend: &ClientFrontend, marker: &str) -> bool {
    frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .is_some_and(|surface| {
            surface
                .frame
                .cells
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                .contains(marker)
        })
}
fn frame(frontend: &mut ClientFrontend) -> crate::protocol::FrameData {
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    frontend.chrome.render(&view)
}

fn prefix_key(frontend: &mut ClientFrontend, key: u8) {
    for event in crate::raw_input::parse_raw_input_bytes_sync(&[1, key]) {
        frontend.dispatch_input(event).unwrap();
    }
}

fn worktree_dialog_text(frontend: &ClientFrontend) -> String {
    let backend = ratatui::backend::TestBackend::new(SIZE.0, SIZE.1);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| worktrees::render(frontend, frame))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

async fn exercise_worktree_dialogs(
    frontend: &mut ClientFrontend,
    local: &OwnedServer,
    other: &OwnedServer,
    endpoint: &ClientEndpointId,
) -> Vec<u32> {
    let repo = other.root.join("worktree-fixture");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "README"],
        vec![
            "-c",
            "user.name=Owned proof",
            "-c",
            "user.email=owned@example.invalid",
            "commit",
            "-qm",
            "owned fixture",
        ],
    ] {
        std::fs::write(repo.join("README"), "owned fixture\n").unwrap();
        assert!(Command::new("git")
            .current_dir(&repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let created = api(
        &other.socket,
        "workspace.create",
        json!({"cwd":repo,"label":"WORKTREE-OWNER","focus":false}),
    );
    let workspace = created["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let root_pane = created["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let root_pid = api(
        &other.socket,
        "pane.process_info",
        json!({"pane_id":root_pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    let public_before = api(&other.socket, "session.snapshot", json!({}));
    let local_before = api(&local.socket, "session.snapshot", json!({}));
    let update = frontend.runtime.activate(
        endpoint.clone(),
        Some(super::super::FocusTarget::Workspace(workspace.clone())),
        Instant::now(),
    );
    frontend.update(update).unwrap();
    pump(frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:RIGHT")
    })
    .await;
    assert!(worktrees::action(frontend, crate::app::NavigateAction::NewWorktree).unwrap());
    pump(frontend, |f| {
        worktree_dialog_text(f).contains("new worktree")
    })
    .await;
    frontend
        .dispatch_input(RawInputEvent::Paste("worktree/client-owner".into()))
        .unwrap();
    assert!(worktree_dialog_text(frontend).contains("worktree/client-owner"));
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(frontend, |f| {
        f.worktrees.is_none() && f.runtime.input_lease_current()
    })
    .await;
    let list = api(
        &other.socket,
        "worktree.list",
        json!({"workspace_id":workspace}),
    );
    let entry = list["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["branch"] == "worktree/client-owner")
        .unwrap();
    let checkout = PathBuf::from(entry["path"].as_str().unwrap());
    assert!(checkout.starts_with(other.root.join("checkouts")));
    let child_workspace = entry["open_workspace_id"].as_str().unwrap().to_string();
    let child_pane = frontend
        .runtime
        .shell
        .endpoint(endpoint)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .focused_pane_id
        .clone()
        .unwrap();
    let child_pid = api(
        &other.socket,
        "pane.process_info",
        json!({"pane_id":child_pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    frontend
        .dispatch_input(RawInputEvent::Paste("WORKTREE-CLIENT-ONLY\n".into()))
        .unwrap();
    pump(frontend, |f| {
        surface_has(f, "ACK:RIGHT") && surface_has(f, "WORKTREE-CLIENT-ONLY")
    })
    .await;
    assert!(!local.text().contains("WORKTREE-CLIENT-ONLY"));
    assert!(!other.text().contains("WORKTREE-CLIENT-ONLY"));
    let update = frontend.runtime.activate(
        endpoint.clone(),
        Some(super::super::FocusTarget::Workspace(workspace.clone())),
        Instant::now(),
    );
    frontend.update(update).unwrap();
    pump(frontend, |f| f.runtime.input_lease_current()).await;
    worktrees::action(frontend, crate::app::NavigateAction::OpenWorktree).unwrap();
    pump(frontend, |f| {
        worktree_dialog_text(f).contains("open worktree")
    })
    .await;
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"/client-owner\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(frontend, |f| {
        f.worktrees.is_none() && f.runtime.input_lease_current()
    })
    .await;
    assert_eq!(
        frontend
            .runtime
            .shell
            .endpoint(endpoint)
            .unwrap()
            .cache
            .snapshot()
            .unwrap()
            .focused_workspace_id
            .as_deref(),
        Some(child_workspace.as_str())
    );
    std::fs::write(checkout.join("README"), "dirty owned fixture\n").unwrap();
    worktrees::action(frontend, crate::app::NavigateAction::RemoveWorktree).unwrap();
    pump(frontend, |f| {
        worktree_dialog_text(f).contains("delete worktree checkout?")
    })
    .await;
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(frontend, |f| {
        worktree_dialog_text(f).contains("delete anyway")
    })
    .await;
    assert!(checkout.is_dir());
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(frontend, |f| {
        f.worktrees.is_none() && f.runtime.input_lease_current()
    })
    .await;
    assert!(!checkout.exists());
    assert!(Command::new("git")
        .current_dir(&repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/worktree/client-owner"
        ])
        .status()
        .unwrap()
        .success());
    let public_after = api(&other.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            public_before["snapshot"][key],
            public_after["snapshot"][key]
        );
    }
    assert_eq!(
        local_before,
        api(&local.socket, "session.snapshot", json!({}))
    );
    api(
        &other.socket,
        "workspace.close",
        json!({"workspace_id":workspace}),
    );
    let update = frontend.runtime.activate(
        endpoint.clone(),
        Some(super::super::FocusTarget::Workspace(
            other.workspace.clone(),
        )),
        Instant::now(),
    );
    frontend.update(update).unwrap();
    pump(frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:RIGHT")
    })
    .await;
    vec![root_pid, child_pid]
}

#[tokio::test]
async fn endpoint_frontend_independent_processes_same_ids_input_and_reconnect() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-process-{}", std::process::id()));
    let mut local = OwnedServer::start(root.join("local"), "LEFT");
    let mut other = OwnedServer::start(root.join("other"), "RIGHT");
    for server in [&local, &other] {
        api(
            &server.socket,
            "agent.rename",
            json!({"target":server.pane,"name":"owned-agent"}),
        );
    }
    assert_eq!(local.pane, other.pane);
    assert_eq!(local.workspace, other.workspace);
    let local_id = ClientEndpointId::Local;
    let other_id = ClientEndpointId::Ssh("m-owned-real-socket-di".into());
    let config: crate::config::Config = toml::from_str("[keys]\nprefix = 'ctrl+a'\nfocus_history_back = 'prefix+b'\nfocus_history_forward = 'prefix+f'\nlast_pane = 'prefix+z'\nnext_agent = 'prefix+q'\nprevious_agent = 'prefix+shift+q'\n").unwrap();
    let (palette, _) =
        crate::app::resolve_effective_theme(&crate::app::theme_runtime_config(&config, true), None);
    let settings = ChromeSettings::from_config(&config, palette.clone(), None);
    let mut shell = ClientShellState::new();
    shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
        id: "m-owned-real-socket-di".into(),
        label: "Second".into(),
        target: "never-contacted".into(),
        session: "owned".into(),
        enabled: true,
    }]);
    let view = ClientChrome::new(ChromeSettings::from_config(&config, palette, None))
        .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &local_id, 1, &local);
    install(&mut frontend, &other_id, 1, &other);
    let update = frontend
        .runtime
        .activate(local_id.clone(), None, Instant::now());
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && surface_has(f, "READY:LEFT")
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .is_some()
    })
    .await;
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let keys: Vec<_> = view
        .hits
        .iter()
        .filter_map(|hit| match &hit.target {
            super::super::chrome::ChromeTarget::Workspace(key) => Some(key.clone()),
            _ => None,
        })
        .collect();
    assert!(keys.contains(&ResourceKey {
        endpoint: local_id.clone(),
        id: local.workspace.clone()
    }));
    assert!(keys.contains(&ResourceKey {
        endpoint: other_id.clone(),
        id: other.workspace.clone()
    }));
    let first = frame(&mut frontend);
    let boot = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .unwrap()
        .boot_id
        .clone();
    api(
        &other.socket,
        "workspace.rename",
        json!({"workspace_id":other.workspace,"label":"RIGHT-UPDATED"}),
    );
    pump(&mut frontend, |f| {
        f.runtime
            .shell
            .endpoint(&other_id)
            .unwrap()
            .cache
            .snapshot()
            .is_some_and(|s| s.workspaces.iter().any(|w| w.label == "RIGHT-UPDATED"))
    })
    .await;
    assert_eq!(
        frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()
            .unwrap()
            .boot_id,
        boot
    );
    assert_eq!(frontend.runtime.endpoints.active_id(), &local_id);
    click(&mut frontend, &other_id, &other.workspace);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime.endpoints.active_id() == &other_id
            && surface_has(f, "READY:RIGHT")
    })
    .await;
    assert!(!notes::available(&frontend));
    assert!(
        !frontend
            .runtime
            .shell
            .endpoint(&other_id)
            .unwrap()
            .cache
            .snapshot()
            .unwrap()
            .integration_updates_available
    );
    assert!(frontend
        .runtime
        .shell
        .endpoint(&other_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .update_available
        .is_none());
    let menu_host_preferences = root.join("client-chrome.json");
    frontend.chrome_preferences_path = Some(menu_host_preferences.clone());
    let menu_local_before = std::fs::read(local.root.join("config.toml")).unwrap();
    let menu_other_before = std::fs::read(other.root.join("config.toml")).unwrap();
    let menu_public_before = api(&other.socket, "session.snapshot", json!({}));
    let local_context_before = api(&local.socket, "session.snapshot", json!({}));
    let cancel_tab = frontend
        .runtime
        .shell
        .endpoint(&local_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .tabs[0]
        .tab_id
        .clone();
    context::open_tab(
        &mut frontend,
        super::super::ResourceKey {
            endpoint: local_id.clone(),
            id: cancel_tab,
        },
        0,
        0,
    );
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.context.is_some());
    assert!(frontend.modal.is_none());
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.context.is_none());
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && f.runtime.endpoints.active_id() == &local_id
    })
    .await;
    assert!(frontend.modal.is_none());
    assert_eq!(
        api(&local.socket, "session.snapshot", json!({})),
        local_context_before
    );
    click(&mut frontend, &other_id, &other.workspace);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && f.runtime.endpoints.active_id() == &other_id
    })
    .await;
    let tab_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let hit = tab_view
        .hits
        .iter()
        .find(|hit| {
            matches!(&hit.target,
        super::super::chrome::ChromeTarget::Tab(key) if key.endpoint == other_id)
        })
        .unwrap();
    let tab_context_target = match &hit.target {
        super::super::chrome::ChromeTarget::Tab(key) => key.clone(),
        _ => unreachable!(),
    };
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
                column: hit.rect.x,
                row: hit.rect.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        ))
        .unwrap();
    assert!(frontend.context.is_some());
    frontend.draw().unwrap();
    let context_rect = crate::app::context_menu_rect_from(
        ratatui::layout::Rect::new(0, 0, SIZE.0, SIZE.1),
        hit.rect.x,
        hit.rect.y,
        &crate::app::state::ContextMenuFacts::Tab {}.items(),
    );
    for (kind, column) in [
        (crossterm::event::MouseEventKind::Moved, context_rect.x + 1),
        (
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            context_rect.x,
        ),
    ] {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind,
                    column,
                    row: context_rect.y + 2,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            ))
            .unwrap();
        assert!(frontend.context.is_some());
        assert!(frontend.modal.is_none());
    }
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.modal.is_some() && f.runtime.input_lease_current()
    })
    .await;
    assert!(frontend.context.is_none());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste("-CTX".into()))
        .unwrap();
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .and_then(|endpoint| endpoint.cache.live_snapshot(1))
                .is_some_and(|snapshot| {
                    snapshot.tabs.iter().any(|tab| {
                        tab.tab_id == tab_context_target.id && tab.label.ends_with("-CTX")
                    })
                })
    })
    .await;
    let tabs_after = api(
        &other.socket,
        "tab.list",
        json!({"workspace_id":other.workspace}),
    );
    assert!(tabs_after["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tab| tab["tab_id"] == tab_context_target.id
            && tab["label"].as_str().unwrap().ends_with("-CTX")));
    let local_context_after = api(&local.socket, "session.snapshot", json!({}));
    assert_eq!(local_context_before, local_context_after);
    let other_context_after = api(&other.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            menu_public_before["snapshot"][key],
            other_context_after["snapshot"][key]
        );
    }
    pane_context_item(&mut frontend, &other.pane, "Split vertical");
    pump(&mut frontend, |f| {
        f.context.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .panes
                .len()
                == 2
    })
    .await;
    let extra_pane = frontend
        .runtime
        .shell
        .endpoint(&other_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .panes
        .iter()
        .find(|pane| pane.pane_id != other.pane)
        .unwrap()
        .pane_id
        .clone();
    let context_split_pty_pid = api(
        &other.socket,
        "pane.process_info",
        json!({"pane_id":extra_pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    let pane_public_before = api(&other.socket, "session.snapshot", json!({}));
    for item in [
        "Equalize pane sizes",
        "Cycle pane layout",
        "Rotate panes",
        "Rotate panes reverse",
        "Move to left split",
        "Move to right split",
        "Move to upper split",
        "Move to lower split",
    ] {
        pane_context_item(&mut frontend, &other.pane, item);
        pump(&mut frontend, |f| {
            f.context.is_none() && f.runtime.input_lease_current()
        })
        .await;
        let actual = api(&other.socket, "session.snapshot", json!({}));
        for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
            assert_eq!(
                pane_public_before["snapshot"][key], actual["snapshot"][key],
                "{item}: {key}"
            );
        }
        assert_eq!(
            local_context_before,
            api(&local.socket, "session.snapshot", json!({}))
        );
    }
    api(&other.socket, "pane.close", json!({"pane_id":extra_pane}));
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .panes
                .len()
                == 1
    })
    .await;
    let workspace_public_before = api(&other.socket, "session.snapshot", json!({}));
    let source_workspace = api(
        &other.socket,
        "workspace.get",
        json!({"workspace_id":other.workspace}),
    );
    workspace_context_item(
        &mut frontend,
        ResourceKey {
            endpoint: other_id.clone(),
            id: other.workspace.clone(),
        },
        "Duplicate",
    );
    pump(&mut frontend, |f| {
        f.context.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .workspaces
                .len()
                == 2
    })
    .await;
    let duplicate_id = frontend
        .runtime
        .shell
        .endpoint(&other_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id != other.workspace)
        .unwrap()
        .workspace_id
        .clone();
    let duplicate = api(
        &other.socket,
        "workspace.get",
        json!({"workspace_id":duplicate_id}),
    );
    assert_ne!(
        duplicate["workspace"]["workspace_id"],
        source_workspace["workspace"]["workspace_id"]
    );
    assert_eq!(
        duplicate["workspace"]["next_public_tab_number"],
        source_workspace["workspace"]["next_public_tab_number"]
    );
    assert_ne!(
        duplicate["workspace"]["active_tab_id"],
        source_workspace["workspace"]["active_tab_id"]
    );
    let duplicated_panes = api(
        &other.socket,
        "pane.list",
        json!({"workspace_id":duplicate_id}),
    );
    let duplicate_pane = duplicated_panes["panes"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(duplicate_pane, other.pane);
    let duplicate_pty_pid = api(
        &other.socket,
        "pane.process_info",
        json!({"pane_id":duplicate_pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            workspace_public_before["snapshot"][key],
            api(&other.socket, "session.snapshot", json!({}))["snapshot"][key]
        );
    }
    assert_eq!(
        local_context_before,
        api(&local.socket, "session.snapshot", json!({}))
    );
    workspace_context_item(
        &mut frontend,
        ResourceKey {
            endpoint: other_id.clone(),
            id: duplicate_id.clone(),
        },
        "Rename",
    );
    pump(&mut frontend, |f| {
        f.modal.is_some() && f.runtime.input_lease_current()
    })
    .await;
    frontend
        .dispatch_input(RawInputEvent::Paste("-WORKSPACE-CTX".into()))
        .unwrap();
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.modal.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .workspaces
                .iter()
                .any(|workspace| {
                    workspace.workspace_id == duplicate_id
                        && workspace.label.ends_with("-WORKSPACE-CTX")
                })
    })
    .await;
    let section = crate::workspace::WorkspaceSection::Work;
    frontend
        .chrome
        .collapsed_sections
        .insert((other_id.clone(), section));
    frontend
        .chrome
        .collapsed_sections
        .insert((local_id.clone(), section));
    workspace_context_item(
        &mut frontend,
        ResourceKey {
            endpoint: other_id.clone(),
            id: duplicate_id.clone(),
        },
        "💼 work",
    );
    pump(&mut frontend, |f| {
        f.context.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .workspace_facts(
                    f.runtime
                        .shell
                        .endpoint(&other_id)
                        .unwrap()
                        .generation
                        .unwrap(),
                    &duplicate_id,
                )
                .is_some_and(|facts| facts.section == Some(section))
    })
    .await;
    assert!(!frontend
        .chrome
        .collapsed_sections
        .contains(&(other_id.clone(), section)));
    assert!(frontend
        .chrome
        .collapsed_sections
        .contains(&(local_id.clone(), section)));
    assert_eq!(
        (
            frontend.chrome.workspace_scroll,
            frontend.chrome.agent_scroll
        ),
        (0, 0)
    );
    let saved_preferences = super::preferences::load(&menu_host_preferences).unwrap();
    let sections = saved_preferences.collapsed_sections.unwrap();
    assert!(sections
        .iter()
        .any(|entry| entry.profile_id.is_none() && entry.section == section));
    assert!(!sections.iter().any(|entry| entry.profile_id.as_deref()
        == match &other_id {
            ClientEndpointId::Ssh(profile) => Some(profile.as_str()),
            ClientEndpointId::Local => None,
        }
        && entry.section == section));
    let stored = api(&other.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            workspace_public_before["snapshot"][key],
            stored["snapshot"][key]
        );
    }
    assert_eq!(
        local_context_before,
        api(&local.socket, "session.snapshot", json!({}))
    );
    let visual_order = frontend
        .chrome
        .visual_workspace_targets(&frontend.runtime.shell, frontend.cols);
    let indexed_position = visual_order
        .iter()
        .position(|key| key.endpoint == other_id && key.id == duplicate_id)
        .unwrap();
    let visible_workspace_order = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows)
        .hits
        .into_iter()
        .filter_map(|hit| match hit.target {
            ChromeTarget::Workspace(key) => Some(key),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(visible_workspace_order, visual_order);
    let mut indexed_config = crate::config::Config::default();
    indexed_config.keys.prefix = config.keys.prefix.clone();
    indexed_config.keys.switch_tab = crate::config::BindingConfig::empty();
    indexed_config.keys.switch_workspace = crate::config::BindingConfig::one("prefix+1..9");
    let old_keybinds = std::mem::replace(
        &mut frontend.keybinds,
        indexed_config.live_keybinds().unwrap(),
    );
    let update = frontend.runtime.activate(
        other_id.clone(),
        Some(super::super::FocusTarget::Pane(other.pane.clone())),
        Instant::now(),
    );
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
    prefix_key(
        &mut frontend,
        b'1' + u8::try_from(indexed_position).unwrap(),
    );
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime.shell.active_endpoint_id == other_id
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .focused_workspace_id
                .as_deref()
                == Some(duplicate_id.as_str())
    })
    .await;
    frontend.keybinds = old_keybinds;
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            workspace_public_before["snapshot"][key],
            api(&other.socket, "session.snapshot", json!({}))["snapshot"][key]
        );
    }
    assert_eq!(
        local_context_before,
        api(&local.socket, "session.snapshot", json!({}))
    );
    api(
        &other.socket,
        "workspace.close",
        json!({"workspace_id":duplicate_id}),
    );
    let update = frontend.runtime.activate(
        other_id.clone(),
        Some(super::super::FocusTarget::Pane(other.pane.clone())),
        Instant::now(),
    );
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .workspaces
                .len()
                == 1
    })
    .await;
    for action in [
        crate::app::GlobalMenuAction::SidebarNarrow,
        crate::app::GlobalMenuAction::SidebarWide,
        crate::app::GlobalMenuAction::SidebarNormal,
        crate::app::GlobalMenuAction::Restart,
    ] {
        let view =
            frontend
                .chrome
                .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
        let hit = view
            .hits
            .iter()
            .find(|hit| matches!(hit.target, super::super::chrome::ChromeTarget::GlobalMenu))
            .unwrap();
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: hit.rect.x,
                    row: hit.rect.y,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            ))
            .unwrap();
        assert!(frontend.menu.is_some());
        let actions = crate::app::global_menu_actions_for(notes::available(&frontend));
        for item in actions
            .iter()
            .filter(|item| **item != crate::app::GlobalMenuAction::Separator)
        {
            if *item == action {
                break;
            }
            for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[B") {
                frontend.dispatch_input(event).unwrap();
            }
        }
        for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
            frontend.dispatch_input(event).unwrap();
        }
        frontend.draw().unwrap();
        if action == crate::app::GlobalMenuAction::Restart {
            pump(&mut frontend, |frontend| {
                frontend
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.mode() == crate::app::Mode::ConfirmDanger)
            })
            .await;
            for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
                frontend.dispatch_input(event).unwrap();
            }
            assert!(frontend.menu.is_none());
        } else {
            assert!(frontend.menu.is_none());
            let expected = match action {
                crate::app::GlobalMenuAction::SidebarNarrow => {
                    frontend.chrome.settings.sidebar_min_width
                }
                crate::app::GlobalMenuAction::SidebarWide => {
                    frontend.chrome.settings.sidebar_max_width
                }
                _ => frontend.chrome.settings.default_sidebar_width,
            };
            assert_eq!(frontend.chrome.settings.sidebar_width, expected);
            assert_eq!(
                preferences::load(&menu_host_preferences)
                    .unwrap()
                    .sidebar_width,
                Some(expected)
            );
        }
        pump(&mut frontend, |frontend| {
            frontend.runtime.input_lease_current()
        })
        .await;
    }
    assert_eq!(
        std::fs::read(local.root.join("config.toml")).unwrap(),
        menu_local_before
    );
    assert_eq!(
        std::fs::read(other.root.join("config.toml")).unwrap(),
        menu_other_before
    );
    let menu_public_after = api(&other.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            menu_public_before["snapshot"][key],
            menu_public_after["snapshot"][key]
        );
    }
    assert_eq!(frontend.runtime.endpoints.active_id(), &other_id);
    let other_config_before = std::fs::read(other.root.join("config.toml")).unwrap();
    let local_config_before = std::fs::read(local.root.join("config.toml")).unwrap();
    let original_palette = frontend.chrome.settings.palette.clone();
    let original_theme = frontend.host_settings.name.clone();
    prefix_key(&mut frontend, b's');
    pump(&mut frontend, |f| {
        f.settings
            .as_ref()
            .is_some_and(|s| s.history == Some(false))
    })
    .await;
    assert_eq!(frontend.runtime.endpoints.active_id(), &other_id);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[B\t") {
        frontend.dispatch_input(event).unwrap();
    }
    assert_ne!(frontend.host_settings.name, original_theme);
    assert_eq!(
        frontend.settings.as_ref().unwrap().state.section,
        crate::app::state::SettingsSection::Sound
    );
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.settings.is_none());
    assert_eq!(frontend.host_settings.name, original_theme);
    assert_eq!(frontend.chrome.settings.palette, original_palette);
    assert_eq!(
        std::fs::read(other.root.join("config.toml")).unwrap(),
        other_config_before
    );
    assert_eq!(
        std::fs::read(local.root.join("config.toml")).unwrap(),
        local_config_before
    );
    // The hidden Experiments entry is a separate UI port. This is the shared
    // controller's real server-owned toggle, not acceptance of that entry.
    settings::open(&mut frontend).unwrap();
    pump(&mut frontend, |f| {
        f.settings
            .as_ref()
            .is_some_and(|s| s.history == Some(false))
    })
    .await;
    let settings = frontend.settings.as_mut().unwrap();
    settings.state.section = crate::app::state::SettingsSection::Experiments;
    settings.state.list.select(0);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b" ") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.settings.as_ref().is_some_and(|s| s.history == Some(true))
    })
    .await;
    assert_eq!(
        api(&other.socket, "server.pane_history.get", json!({}))["enabled"],
        true
    );
    assert_eq!(
        api(&local.socket, "server.pane_history.get", json!({}))["enabled"],
        false
    );
    assert_eq!(
        std::fs::read(local.root.join("config.toml")).unwrap(),
        local_config_before
    );
    let other_config: crate::config::Config =
        toml::from_str(&std::fs::read_to_string(other.root.join("config.toml")).unwrap()).unwrap();
    assert!(other_config.experimental.pane_history);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.settings.is_none());
    let host_config = root.join("reload-host.toml");
    std::fs::write(&host_config, "[keys]\nprefix = 'ctrl+a'\nsettings = 'prefix+m'\nfocus_history_back = 'prefix+b'\nfocus_history_forward = 'prefix+f'\nlast_pane = 'prefix+z'\nnext_agent = 'prefix+q'\nprevious_agent = 'prefix+shift+q'\n[ui]\nsidebar_width = 34\nmouse_capture = false\n").unwrap();
    let changed_other = crate::config::upsert_section_bool(
        &std::fs::read_to_string(other.root.join("config.toml")).unwrap(),
        "experimental",
        "pane_history",
        false,
    );
    std::fs::write(other.root.join("config.toml"), &changed_other).unwrap();
    let boot = frontend
        .runtime
        .shell
        .endpoint_snapshot_identity(&other_id, 1)
        .unwrap()
        .0
        .to_owned();
    {
        // Host config lookup is synchronous; restore the environment before awaiting real IO.
        let _guard = crate::config::lock_test_config_env();
        let previous = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &host_config);
        prefix_key(&mut frontend, b'R');
        match previous {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
    }
    assert_eq!(frontend.chrome.settings.sidebar_width, 34);
    assert!(!frontend.options.mouse_capture);
    let mut reload_response = None;
    tokio::time::timeout(TIMEOUT, async {
        while frontend.reload_request.is_some() {
            let event = frontend.runtime.reader_events.recv().await.unwrap();
            let update = frontend.runtime.receive(event, Instant::now());
            for completed in &update.completed {
                if completed
                    .result
                    .as_ref()
                    .is_ok_and(|value| value["type"] == "config_reload")
                {
                    assert_eq!(completed.endpoint_id, other_id);
                    assert_eq!(completed.generation, 1);
                    assert_eq!(completed.boot_id, boot);
                    assert!(reload_response.is_none());
                    reload_response = Some(completed.result.as_ref().unwrap().clone());
                }
            }
            frontend.update(update).unwrap();
        }
    })
    .await
    .unwrap();
    pump(&mut frontend, |frontend| {
        !frontend.host.mouse_capture.load(Ordering::Acquire)
    })
    .await;
    let response = reload_response.unwrap();
    assert_eq!(response["status"], "applied");
    assert_eq!(response["diagnostics"], json!([]));
    frontend.draw().unwrap();
    pump(&mut frontend, |frontend| {
        frontend.runtime.input_lease_current()
    })
    .await;
    let (snapshot_request, update) = frontend
        .runtime
        .issue_method_with_id(crate::api::schema::Method::SessionSnapshot(
            crate::api::schema::EmptyParams::default(),
        ))
        .unwrap();
    frontend.update(update).unwrap();
    let owner_warnings = tokio::time::timeout(TIMEOUT, async {
        loop {
            let event = frontend.runtime.reader_events.recv().await.unwrap();
            let update = frontend.runtime.receive(event, Instant::now());
            let response = update
                .completed
                .iter()
                .find(|completed| completed.request_id == snapshot_request);
            let warnings = response.map(|completed| {
                assert_eq!(completed.endpoint_id, other_id);
                assert_eq!(completed.generation, 1);
                assert_eq!(completed.boot_id, boot);
                let value = completed.result.as_ref().unwrap();
                assert_eq!(value["type"], "session_snapshot");
                assert!(value["snapshot"]["agent_session_warnings"].is_array());
                value["snapshot"]["agent_session_warnings"].clone()
            });
            frontend.update(update).unwrap();
            if let Some(warnings) = warnings {
                break warnings;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        owner_warnings,
        api(&other.socket, "session.snapshot", json!({}))["snapshot"]["agent_session_warnings"]
    );
    assert_eq!(
        api(&other.socket, "server.pane_history.get", json!({}))["enabled"],
        false
    );
    assert_eq!(
        std::fs::read(local.root.join("config.toml")).unwrap(),
        local_config_before
    );
    assert_eq!(
        std::fs::read_to_string(other.root.join("config.toml")).unwrap(),
        changed_other
    );
    prefix_key(&mut frontend, b'?');
    assert!(frontend.help.is_some());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::OuterFocusLost)
        .unwrap();
    assert_eq!(frontend.runtime.shell.outer_focused, Some(false));
    assert!(frontend.help.is_some());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::OuterFocusGained)
        .unwrap();
    assert_eq!(frontend.runtime.shell.outer_focused, Some(true));
    assert!(frontend.help.is_some());
    assert!(!frontend.runtime.input_lease_current());
    assert_eq!(frontend.runtime.endpoints.active_id(), &other_id);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:RIGHT")
    })
    .await;
    assert!(frontend.help.is_some());
    let help_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let help_frame = frontend
        .chrome
        .render_with_overlay(&help_view, |frame| help::render(&frontend, frame));
    assert_eq!(
        help_frame.cells.len(),
        usize::from(SIZE.0) * usize::from(SIZE.1)
    );
    assert!(help_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
        .contains("keybinds"));
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[F") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.help.as_ref().unwrap().scroll > 0);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[H") {
        frontend.dispatch_input(event).unwrap();
    }
    assert_eq!(frontend.help.as_ref().unwrap().scroll, 0);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"jNOT-PTY\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.help.is_none());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "OTHER-ONLY\n".into(),
        ))
        .unwrap();
    pump(&mut frontend, |f| {
        surface_has(f, "ACK:RIGHT") && surface_has(f, "OTHER-ONLY")
    })
    .await;
    assert!(other.text().contains("OTHER-ONLY"));
    let ack_prefix = format!("ACK:RIGHT:{}:", other.marker_pid("ACK:RIGHT:"));
    let text = other.text();
    let ack_lines: Vec<_> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix(&ack_prefix))
        .collect();
    assert_eq!(ack_lines, ["OTHER-ONLY"]);
    assert!(!local.text().contains("OTHER-ONLY"));
    for (key, endpoint, marker) in [
        (b'b', &local_id, "READY:LEFT"),
        (b'f', &other_id, "READY:RIGHT"),
        (b'z', &local_id, "READY:LEFT"),
        (b'z', &other_id, "READY:RIGHT"),
        (b'q', &local_id, "READY:LEFT"),
        (b'Q', &other_id, "READY:RIGHT"),
    ] {
        prefix_key(&mut frontend, key);
        pump(&mut frontend, |frontend| {
            frontend.runtime.input_lease_current()
                && frontend.runtime.endpoints.active_id() == endpoint
                && surface_has(frontend, marker)
        })
        .await;
    }
    let local_pty_pid = local.marker_pid("READY:LEFT:");
    let stopped_other_pty_pid = other.marker_pid("ACK:RIGHT:");
    workspace_context_item(
        &mut frontend,
        ResourceKey {
            endpoint: other_id.clone(),
            id: other.workspace.clone(),
        },
        "🏠 personal",
    );
    pump(&mut frontend, |f| {
        f.context.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .workspace_facts(1, &other.workspace)
                .is_some_and(|facts| {
                    facts.section == Some(crate::workspace::WorkspaceSection::Personal)
                })
    })
    .await;
    click(&mut frontend, &local_id, &local.workspace);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime.endpoints.active_id() == &local_id
            && surface_has(f, "READY:LEFT")
    })
    .await;
    let old_other_boot = frontend
        .runtime
        .shell
        .endpoint(&other_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .boot_id
        .clone();
    let stopped_pid = other.stop();
    let persisted_section_path = other
        .root
        .join("config")
        .join(crate::config::app_dir_name())
        .join("session.json");
    let persisted: crate::persist::SessionSnapshot =
        serde_json::from_slice(&std::fs::read(&persisted_section_path).unwrap()).unwrap();
    assert_eq!(
        persisted
            .workspaces
            .iter()
            .find(|workspace| workspace.id.as_deref() == Some(other.workspace.as_str()))
            .unwrap()
            .section,
        crate::workspace::WorkspaceSection::Personal
    );
    pump(&mut frontend, |f| {
        f.runtime.shell.endpoint(&other_id).unwrap().status == ClientEndpointStatus::Reconnecting
    })
    .await;
    assert_eq!(frontend.runtime.endpoints.active_id(), &local_id);
    assert!(frontend.runtime.input_lease_current());
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "LOCAL-DURING-DISCONNECT\n".into(),
        ))
        .unwrap();
    pump(&mut frontend, |f| {
        surface_has(f, "ACK:LEFT") && surface_has(f, "LOCAL-DURING-DISCONNECT")
    })
    .await;
    assert!(local.text().contains("LOCAL-DURING-DISCONNECT"));
    other.spawn();
    install(&mut frontend, &other_id, 2, &other);
    pump(&mut frontend, |f| {
        f.runtime
            .shell
            .endpoint(&other_id)
            .unwrap()
            .cache
            .snapshot()
            .is_some_and(|s| s.boot_id != old_other_boot)
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .workspace_facts(2, &other.workspace)
                .is_some_and(|facts| {
                    facts.section == Some(crate::workspace::WorkspaceSection::Personal)
                })
    })
    .await;
    assert_eq!(frontend.runtime.endpoints.active_id(), &local_id);
    assert!(frontend.runtime.input_lease_current());
    assert!(!other.text().contains("LOCAL-DURING-DISCONNECT"));
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "LOCAL-AFTER-RECONNECT\n".into(),
        ))
        .unwrap();
    pump(&mut frontend, |f| {
        surface_has(f, "ACK:LEFT") && surface_has(f, "LOCAL-AFTER-RECONNECT")
    })
    .await;
    assert!(local.text().contains("LOCAL-AFTER-RECONNECT"));
    assert!(!other.text().contains("LOCAL-AFTER-RECONNECT"));
    // An old boot's p1 history must not select the new server's identically named p1.
    assert!(history::action(&mut frontend, crate::app::NavigateAction::LastPane).unwrap());
    assert_eq!(frontend.runtime.endpoints.active_id(), &local_id);
    assert!(frontend.runtime.input_lease_current());
    let after = frame(&mut frontend);
    click(&mut frontend, &other_id, &other.workspace);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime.endpoints.active_id() == &other_id
            && surface_has(f, "READY:RIGHT")
    })
    .await;
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "OTHER-AFTER-RECONNECT\n".into(),
        ))
        .unwrap();
    pump(&mut frontend, |f| {
        surface_has(f, "ACK:RIGHT") && surface_has(f, "OTHER-AFTER-RECONNECT")
    })
    .await;
    assert!(other.text().contains("OTHER-AFTER-RECONNECT"));
    assert!(!local.text().contains("OTHER-AFTER-RECONNECT"));
    let restarted_other_pty_pid = other.marker_pid("ACK:RIGHT:");
    assert_ne!(stopped_other_pty_pid, restarted_other_pty_pid);
    let worktree_pty_pids =
        exercise_worktree_dialogs(&mut frontend, &local, &other, &other_id).await;
    let mobile_public_local = api(&local.socket, "session.snapshot", json!({}));
    let mobile_public_other = api(&other.socket, "session.snapshot", json!({}));
    let mobile_config: crate::config::Config =
        toml::from_str("[keys]\nprefix = 'ctrl+a'\nworkspace_picker = 'prefix+w'\n").unwrap();
    let previous_keybinds = std::mem::replace(
        &mut frontend.keybinds,
        mobile_config.live_keybinds().unwrap(),
    );
    let narrow = (config.ui.mobile_width_threshold, SIZE.1);
    input::handle(
        &mut frontend,
        super::super::super::ClientLoopEvent::Resize(narrow.0, narrow.1, 0, 0),
    )
    .unwrap();
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
    let header_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, narrow.0, narrow.1);
    let header_frame = frontend.chrome.render(&header_view);
    let header_text = header_frame
        .cells
        .iter()
        .take(usize::from(narrow.0) * 2)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(header_text.contains("switch"));
    assert!(header_text.contains("tab 1"));
    assert!(header_text.contains("no agents"));
    frontend.draw().unwrap();
    frontend
        .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: header_view.menu_launcher.x + 1,
            row: header_view.menu_launcher.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .unwrap();
    assert!(frontend.mobile.is_some());
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.mobile.is_none());
    prefix_key(&mut frontend, b'w');
    assert!(frontend.mobile.is_some());
    frontend.draw().unwrap();
    let mobile_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, narrow.0, narrow.1);
    let mobile_frame = frontend.chrome.render_with_overlay(&mobile_view, |frame| {
        mobile::render(&frontend, frame, mobile_view.layout.pane_surface)
    });
    assert_eq!(
        mobile_frame.cells.len(),
        usize::from(narrow.0) * usize::from(narrow.1)
    );
    let mobile_text = mobile_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(mobile_text.contains("machines"));
    assert!(mobile_text.contains("Local"));
    assert!(mobile_text.contains("Second"));
    assert!(mobile_text.contains("close"));
    let mobile_initial_endpoint = frontend.runtime.shell.active_endpoint_id.clone();
    let second_row = mobile_frame
        .cells
        .chunks(usize::from(narrow.0))
        .position(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                .contains("Second")
        })
        .unwrap() as u16;
    let mobile_click = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 1,
            row: second_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    };
    let original_label = frontend
        .runtime
        .shell
        .endpoint(&other_id)
        .unwrap()
        .cache
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == other.workspace)
        .unwrap()
        .label
        .clone();
    api(
        &other.socket,
        "workspace.rename",
        json!({"workspace_id":other.workspace,"label":"mobile-updated"}),
    );
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if frontend
                .runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .workspaces
                .iter()
                .any(|workspace| {
                    workspace.workspace_id == other.workspace && workspace.label == "mobile-updated"
                })
            {
                break;
            }
            let event = frontend.runtime.reader_events.recv().await.unwrap();
            let update = frontend.runtime.receive(event, Instant::now());
            frontend.update(update).unwrap();
        }
    })
    .await
    .unwrap();
    frontend.dispatch_input(mobile_click()).unwrap();
    assert!(frontend.mobile.is_some());
    assert_eq!(
        frontend.runtime.shell.active_endpoint_id,
        mobile_initial_endpoint
    );
    api(
        &other.socket,
        "workspace.rename",
        json!({"workspace_id":other.workspace,"label":original_label}),
    );
    pump(&mut frontend, |f| {
        f.runtime
            .shell
            .endpoint(&other_id)
            .unwrap()
            .cache
            .snapshot()
            .unwrap()
            .workspaces
            .iter()
            .any(|workspace| {
                workspace.workspace_id == other.workspace && workspace.label == original_label
            })
    })
    .await;
    frontend.draw().unwrap();
    frontend.dispatch_input(mobile_click()).unwrap();
    pump(&mut frontend, |f| {
        f.mobile.is_none()
            && f.runtime.input_lease_current()
            && f.runtime.shell.active_endpoint_id == other_id
    })
    .await;
    prefix_key(&mut frontend, b'w');
    frontend.draw().unwrap();
    for (number, marker, endpoint, server, other_server) in [
        (b'1', "MOBILE-LOCAL", &local_id, &local, &other),
        (b'2', "MOBILE-OTHER", &other_id, &other, &local),
    ] {
        for event in crate::raw_input::parse_raw_input_bytes_sync(&[number]) {
            frontend.dispatch_input(event).unwrap();
        }
        pump(&mut frontend, |f| {
            f.mobile.is_none()
                && f.runtime.input_lease_current()
                && f.runtime.shell.active_endpoint_id == *endpoint
        })
        .await;
        frontend
            .dispatch_input(RawInputEvent::Paste(format!("{marker}\n")))
            .unwrap();
        pump(&mut frontend, |f| {
            surface_has(f, marker) && surface_has(f, "ACK:")
        })
        .await;
        assert!(server.text().contains(marker));
        assert!(!other_server.text().contains(marker));
        prefix_key(&mut frontend, b'w');
        assert!(frontend.mobile.is_some());
    }
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.mobile.is_none());
    prefix_key(&mut frontend, b'w');
    frontend.draw().unwrap();
    let close = crate::ui::mobile_switcher_areas_for_screen(ratatui::layout::Rect::new(
        0, 0, narrow.0, narrow.1,
    ))
    .close;
    frontend
        .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .unwrap();
    assert!(frontend.mobile.is_none());
    assert_eq!(
        mobile_public_local,
        api(&local.socket, "session.snapshot", json!({}))
    );
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            mobile_public_other["snapshot"][key],
            api(&other.socket, "session.snapshot", json!({}))["snapshot"][key]
        );
    }
    frontend.keybinds = previous_keybinds;
    input::handle(
        &mut frontend,
        super::super::super::ClientLoopEvent::Resize(SIZE.0, SIZE.1, 0, 0),
    )
    .unwrap();
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;

    let navigate_public_local = api(&local.socket, "session.snapshot", json!({}));
    let navigate_public_other = api(&other.socket, "session.snapshot", json!({}));
    let navigate_active = frontend.runtime.shell.active_endpoint_id.clone();
    prefix_key(&mut frontend, b'w');
    assert!(frontend.mobile.is_some());
    frontend.draw().unwrap();
    let initial_selection = frontend.chrome.navigate_selection.clone();
    let targets = frontend
        .chrome
        .visual_workspace_targets(&frontend.runtime.shell, SIZE.0);
    assert!(targets.len() > 1);
    let index = targets
        .iter()
        .position(|key| Some(key) == initial_selection.as_ref())
        .unwrap();
    let direction = if index + 1 < targets.len() {
        b"\x1b[B".as_slice()
    } else {
        b"\x1b[A".as_slice()
    };
    for event in crate::raw_input::parse_raw_input_bytes_sync(direction) {
        frontend.dispatch_input(event).unwrap();
    }
    frontend.draw().unwrap();
    let selected = frontend.chrome.navigate_selection.clone().unwrap();
    assert_ne!(Some(&selected), initial_selection.as_ref());
    assert_eq!(frontend.runtime.shell.active_endpoint_id, navigate_active);
    assert_eq!(
        navigate_public_local,
        api(&local.socket, "session.snapshot", json!({}))
    );
    assert_eq!(
        navigate_public_other,
        api(&other.socket, "session.snapshot", json!({}))
    );
    let desktop_view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let desktop_frame = frontend.chrome.render_with_overlay(&desktop_view, |frame| {
        mobile::render(&frontend, frame, desktop_view.layout.pane_surface)
    });
    assert!(desktop_frame
        .cells
        .iter()
        .skip(
            usize::from(SIZE.0)
                * usize::from(
                    desktop_view.layout.pane_surface.y + desktop_view.layout.pane_surface.height
                        - 1
                )
        )
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
        .contains(" NAVIGATE "));
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
    assert!(frontend.mobile.is_none());
    assert_eq!(frontend.runtime.shell.active_endpoint_id, selected.endpoint);
    assert_eq!(
        frontend
            .runtime
            .shell
            .endpoint(&selected.endpoint)
            .unwrap()
            .cache
            .snapshot()
            .unwrap()
            .focused_workspace_id
            .as_deref(),
        Some(selected.id.as_str())
    );
    assert_eq!(
        navigate_public_local,
        api(&local.socket, "session.snapshot", json!({}))
    );
    assert_eq!(
        navigate_public_other,
        api(&other.socket, "session.snapshot", json!({}))
    );

    let navigator_public_local = api(&local.socket, "session.snapshot", json!({}));
    let navigator_public_other = api(&other.socket, "session.snapshot", json!({}));
    prefix_key(&mut frontend, b'g');
    assert!(frontend.navigator.is_some());
    frontend.draw().unwrap();
    for endpoint in &frontend.runtime.shell.endpoints {
        for pane in &endpoint.cache.snapshot().unwrap().panes {
            let facts = endpoint.cache.displayed_pane_facts(&pane.pane_id).unwrap();
            assert!(facts.number > 0);
        }
    }
    let (_, navigator_panes) = navigator::test_rows(&frontend);
    assert_eq!(
        navigator_panes,
        frontend
            .runtime
            .shell
            .endpoints
            .iter()
            .map(|endpoint| endpoint.cache.snapshot().unwrap().panes.len())
            .sum::<usize>()
    );
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"/\x1b[200~RIGHT-UPDATED\x1b[201~") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(navigator::test_rows(&frontend).0 > 0);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x15\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(frontend.navigator.is_some());
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[H ") {
        frontend.dispatch_input(event).unwrap();
    }
    let folded = navigator::test_rows(&frontend).0;
    for event in crate::raw_input::parse_raw_input_bytes_sync(b" ") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(navigator::test_rows(&frontend).0 > folded);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"/\x1b[200~RIGHT-UPDATED\x1b[201~") {
        frontend.dispatch_input(event).unwrap();
    }
    frontend.draw().unwrap();
    assert_eq!(navigator::test_query(&frontend), "RIGHT-UPDATED");
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
    assert!(frontend.navigator.is_none());
    assert_eq!(frontend.runtime.shell.active_endpoint_id, other_id);
    frontend
        .dispatch_input(RawInputEvent::Paste("NAVIGATOR-OTHER\n".into()))
        .unwrap();
    pump(&mut frontend, |_| {
        other.text().contains("ACK:RIGHT:") && other.text().contains("NAVIGATOR-OTHER")
    })
    .await;
    assert!(!local.text().contains("NAVIGATOR-OTHER"));
    assert_eq!(
        navigator_public_local,
        api(&local.socket, "session.snapshot", json!({}))
    );
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            navigator_public_other["snapshot"][key],
            api(&other.socket, "session.snapshot", json!({}))["snapshot"][key]
        );
    }

    frontend.runtime.endpoints.disconnect(&local_id);
    frontend.runtime.endpoints.disconnect(&other_id);
    let local_pid = local.stop();
    let restarted_pid = other.stop();
    let owned_pids = [
        local_pid,
        stopped_pid,
        restarted_pid,
        local_pty_pid,
        stopped_other_pty_pid,
        restarted_other_pty_pid,
        context_split_pty_pid,
        duplicate_pty_pid,
    ];
    for pid in owned_pids
        .into_iter()
        .chain(worktree_pty_pids.iter().copied())
    {
        assert!(
            !Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "pid="])
                .output()
                .unwrap()
                .status
                .success(),
            "owned process {pid} remains"
        );
    }
    let evidence = root.join("evidence.json");
    std::fs::write(&evidence,serde_json::to_vec_pretty(&json!({"scope":"existing frontend/runtime real socket DI; independent Cargo-built processes; not normal saved SSH connector acceptance", "server_binary":cargo_built_binary(), "test_binary":std::env::current_exe().unwrap(), "result":"PASS", "same_pane_id":local.pane, "same_workspace_id":local.workspace, "stopped_other_pid":stopped_pid,"local_pid":local_pid,"restarted_other_pid":restarted_pid,"local_pty_pid":local_pty_pid,"stopped_other_pty_pid":stopped_other_pty_pid,"restarted_other_pty_pid":restarted_other_pty_pid,"context_split_pty_pid":context_split_pty_pid,"context_pane_split_and_arrange":8,"worktree_dialog_pty_pids_absent":worktree_pty_pids,"owned_pids_absent":owned_pids,"post_reconnect_local_ack":"LOCAL-AFTER-RECONNECT","post_reconnect_other_ack":"OTHER-AFTER-RECONNECT","initial_frame":first,"reconnected_frame":after,"mobile_frame":mobile_frame})).unwrap()).unwrap();
    println!(
        "independent process frontend evidence: {}",
        evidence.display()
    );
}

#[tokio::test]
async fn endpoint_frontend_last_workspace_close_preserves_default_and_monotonic_ids() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-empty-{}", std::process::id()));
    let mut server = OwnedServer::start(root, "EMPTY-OWNED");
    let config = crate::config::Config::default();
    let settings =
        ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None);
    let shell = ClientShellState::new();
    let view = ClientChrome::new(ChromeSettings::from_config(
        &config,
        crate::app::state::Palette::catppuccin(),
        None,
    ))
    .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();
    eprintln!("empty-owned stage: initial READY");
    pump(&mut frontend, |frontend| {
        frontend.runtime.input_lease_current() && surface_has(frontend, "READY:EMPTY-OWNED:")
    })
    .await;
    let first_pty = server.marker_pid("READY:EMPTY-OWNED:");
    let old_pane = server.pane.clone();
    api(
        &server.socket,
        "workspace.close",
        json!({"workspace_id":server.workspace}),
    );
    eprintln!("empty-owned stage: existing default workspace replacement and lease");
    pump(&mut frontend, |frontend| {
        frontend.runtime.input_lease_current()
            && surface_has(frontend, "READY:EMPTY-OWNED:")
            && frontend
                .runtime
                .shell
                .endpoint(&ClientEndpointId::Local)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .focused_pane_id
                .as_ref()
                .is_some_and(|pane| pane != &old_pane)
    })
    .await;
    let replacement = frontend
        .runtime
        .shell
        .endpoint(&ClientEndpointId::Local)
        .unwrap()
        .cache
        .snapshot()
        .unwrap();
    assert_eq!(replacement.workspaces.len(), 1);
    server.pane = replacement.focused_pane_id.clone().unwrap();
    server.workspace = replacement.focused_workspace_id.clone().unwrap();
    let replacement_pane = server.pane.clone();
    let replacement_pty = server.marker_pid("READY:EMPTY-OWNED:");
    assert!(modal::action(&mut frontend, crate::app::NavigateAction::NewWorkspace).unwrap());
    eprintln!("empty-owned stage: created workspace READY and distinct pane");
    pump(&mut frontend, |frontend| {
        frontend.runtime.input_lease_current()
            && surface_has(frontend, "READY:EMPTY-OWNED:")
            && frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .panes
                .iter()
                .any(|pane| pane.pane_id != replacement_pane)
    })
    .await;
    let snapshot = frontend
        .runtime
        .shell
        .endpoint(&ClientEndpointId::Local)
        .unwrap()
        .cache
        .snapshot()
        .unwrap();
    assert_eq!(snapshot.workspaces.len(), 2);
    server.pane = snapshot.focused_pane_id.clone().unwrap();
    server.workspace = snapshot.focused_workspace_id.clone().unwrap();
    assert_ne!(server.pane, old_pane);
    assert_ne!(server.pane, replacement_pane);
    let second_pty = server.marker_pid("READY:EMPTY-OWNED:");
    frontend
        .dispatch_input(crate::raw_input::RawInputEvent::Paste(
            "EMPTY-CREATE-ACK\n".into(),
        ))
        .unwrap();
    pump(&mut frontend, |frontend| {
        surface_has(frontend, "ACK:EMPTY-OWNED:") && surface_has(frontend, "EMPTY-CREATE-ACK")
    })
    .await;
    let server_pid = server.stop();
    for pid in [server_pid, first_pty, replacement_pty, second_pty] {
        assert!(!Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .status()
            .unwrap()
            .success());
    }
}

#[tokio::test]
async fn endpoint_frontend_copy_search_cursor_and_input_isolation_actual_process() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-copy-{}", std::process::id()));
    let mut server = OwnedServer::start(root, "COPY-OWNED");
    let config: crate::config::Config = toml::from_str("[keys]\nprefix = 'ctrl+a'\n").unwrap();
    let settings =
        ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None);
    let shell = ClientShellState::new();
    let view = ClientChrome::new(ChromeSettings::from_config(
        &config,
        crate::app::state::Palette::catppuccin(),
        None,
    ))
    .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();

    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:COPY-OWNED:")
    })
    .await;
    let pty = server.marker_pid("READY:COPY-OWNED:");
    prefix_key(&mut frontend, b'[');
    assert!(copy::active(&frontend));
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"g0/COPY-OWNED\r") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, copy::search_idle_for_test).await;
    assert_eq!(copy::search_position_for_test(&frontend), Some((0, 6, 1)));
    let public = api(
        &server.socket,
        "pane.read",
        json!({"pane_id":server.pane,"source":"recent","format":"text"}),
    );
    assert!(!public.to_string().contains("ACK:COPY-OWNED:"));
    // Adjacent ESC+q bytes encode Alt-q; exercise the fork's separate Esc then q.
    for bytes in [b"n0vllll".as_slice(), b"\x1b", b"q"] {
        for event in crate::raw_input::parse_raw_input_bytes_sync(bytes) {
            frontend.dispatch_input(event).unwrap();
        }
    }
    pump(&mut frontend, |f| !copy::active(f)).await;
    assert!(frontend.runtime.input_lease_current());
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    server.stop();
    assert!(!crate::platform::process_exists(pty));
}

#[tokio::test]
async fn endpoint_frontend_pointer_selection_and_scrollbar_actual_process() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-pointer-{}", std::process::id()));
    let mut server = OwnedServer::start_with_prelude(
        root,
        "POINTER-OWNED",
        &format!(
            "i=0; while [ $i -lt {} ]; do printf 'LINE-%s\\n' \"$i\"; i=$((i+1)); done",
            SIZE.1 * 2
        ),
    );
    let config: crate::config::Config = toml::from_str("[keys]\nprefix = 'ctrl+a'\n").unwrap();
    let settings =
        ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None);
    let shell = ClientShellState::new();
    let view = ClientChrome::new(ChromeSettings::from_config(
        &config,
        crate::app::state::Palette::catppuccin(),
        None,
    ))
    .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();

    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:POINTER-OWNED:")
    })
    .await;
    let pty = server.marker_pid("READY:POINTER-OWNED:");
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let pane = frontend.runtime.shell.pane_surface.as_ref().unwrap().panes[0].clone();
    let inner = pane.inner_rect;
    let x = view.layout.pane_surface.x + inner.x;
    let y = view.layout.pane_surface.y + inner.y + 2;
    for (button, column, row) in [(0, x, y), (32, x + 4, y)] {
        let bytes = format!("\x1b[<{button};{};{}M", column + 1, row + 1);
        for event in crate::raw_input::parse_raw_input_bytes_sync(bytes.as_bytes()) {
            frontend.dispatch_input(event).unwrap();
        }
    }
    let top = pane.scroll.unwrap().max_offset_from_bottom - pane.scroll.unwrap().offset_from_bottom;
    assert_eq!(
        selection::range_for_test(&frontend),
        Some(((top as u32 + 2, 0), (top as u32 + 2, 4)))
    );
    let drawn = frontend.chrome.render_with_overlay(&view, |frame| {
        selection::render(&frontend, frame, view.layout.pane_surface)
    });
    let original = frontend.chrome.render(&view);
    let index = usize::from(y) * usize::from(drawn.width) + usize::from(x);
    assert_ne!(drawn.cells[index].bg, original.cells[index].bg);
    assert!(frontend.pointer_selection.deadline.is_none());
    selection::clear(&mut frontend);
    let track = pane.scrollbar_rect.unwrap();
    let (tx, ty) = (
        view.layout.pane_surface.x + track.x,
        view.layout.pane_surface.y + track.y,
    );
    for event in crate::raw_input::parse_raw_input_bytes_sync(
        format!("\x1b[<0;{};{}M", tx + 1, ty + 1).as_bytes(),
    ) {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .pane_surface
                .as_ref()
                .is_some_and(|surface| {
                    surface.panes[0].scroll.is_some_and(|scroll| {
                        scroll.offset_from_bottom == scroll.max_offset_from_bottom
                            && scroll.offset_from_bottom > 0
                    })
                })
    })
    .await;
    assert!(surface_has(&frontend, "LINE-0"));
    for event in crate::raw_input::parse_raw_input_bytes_sync(
        format!("\x1b[<0;{};{}m", tx + 1, ty + 1).as_bytes(),
    ) {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(selection::range_for_test(&frontend).is_none());
    let entry_offset = pane.scroll.unwrap().max_offset_from_bottom;
    prefix_key(&mut frontend, b'[');
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"G") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .pane_surface
                .as_ref()
                .is_some_and(|surface| {
                    surface.panes[0]
                        .scroll
                        .is_some_and(|scroll| scroll.offset_from_bottom == 0)
                })
    })
    .await;
    prefix_key(&mut frontend, b'[');
    pump(&mut frontend, |f| {
        copy::active(f)
            && f.copy_action.is_none()
            && f.runtime.input_lease_current()
            && f.runtime
                .shell
                .pane_surface
                .as_ref()
                .is_some_and(|surface| {
                    surface.panes[0]
                        .scroll
                        .is_some_and(|scroll| scroll.offset_from_bottom == entry_offset)
                })
    })
    .await;
    assert_eq!(copy::entry_offset_for_test(&frontend), Some(entry_offset));
    let public = api(
        &server.socket,
        "pane.read",
        json!({"pane_id":server.pane,"source":"recent","format":"text"}),
    );
    assert!(!public.to_string().contains("ACK:POINTER-OWNED:"));
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    let server_pid = server.stop();
    for pid in [pty, server_pid] {
        assert!(!crate::platform::process_exists(pid));
    }
}

#[tokio::test]
async fn endpoint_frontend_right_click_passthrough_captured_pane_actual_bytes() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-right-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let receiver = root.join("receiver.py");
    std::fs::write(
        &receiver,
        r#"import os, pathlib, sys, tty
tty.setraw(0)
pid=os.getpid()
p=pathlib.Path(sys.argv[1])/('input-'+str(pid)+'.bin')
p.write_bytes(b'')
os.write(1, b'\x1b[?1002h\x1b[?1006h'+('READY:RAW:'+str(pid)+'\r\n').encode())
while True:
 data=os.read(0,65536)
 if not data: break
 with p.open('ab') as f: f.write(data)
 os.write(1,('INPUT:'+str(pid)+':'+data.hex()+'\r\n').encode())
"#,
    )
    .unwrap();
    let mut server = OwnedServer::start_with_prelude(
        root.join("server"),
        "RAW",
        &format!(
            "exec python3 '{}' '{}'\n",
            receiver.display(),
            root.display()
        ),
    );
    let second = api(
        &server.socket,
        "pane.split",
        json!({"target_pane_id":server.pane,"direction":"right","focus":false}),
    );
    let second_id = second["pane"]["pane_id"].as_str().unwrap().to_owned();
    let public_before = api(&server.socket, "session.snapshot", json!({}));
    let config: crate::config::Config =
        toml::from_str("[ui]\nright_click_passthrough_modifier = 'ctrl'\n").unwrap();
    let (palette, _) =
        crate::app::resolve_effective_theme(&crate::app::theme_runtime_config(&config, true), None);
    let settings = ChromeSettings::from_config(&config, palette.clone(), None);
    let shell = ClientShellState::new();
    let view = ClientChrome::new(ChromeSettings::from_config(&config, palette, None))
        .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .pane_surface
                .as_ref()
                .is_some_and(|surface| {
                    surface.panes.len() == 2 && surface.panes.iter().all(|p| p.mouse_reporting)
                })
    })
    .await;
    let pids: Vec<u32> = [&server.pane, &second_id]
        .iter()
        .map(|id| {
            api(&server.socket, "pane.process_info", json!({"pane_id":id}))["process_info"]
                ["shell_pid"]
                .as_u64()
                .unwrap() as u32
        })
        .collect();
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let pane = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .unwrap()
        .panes
        .iter()
        .find(|p| p.pane_id == second_id)
        .unwrap()
        .clone();
    assert!(!pane.focused);
    let x = view.layout.pane_surface.x + pane.inner_rect.x;
    let y = view.layout.pane_surface.y + pane.inner_rect.y;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for (kind, dx, dy, modifiers) in [
        (
            MouseEventKind::Down(MouseButton::Right),
            0,
            0,
            KeyModifiers::CONTROL,
        ),
        (
            MouseEventKind::Drag(MouseButton::Right),
            1,
            1,
            KeyModifiers::empty(),
        ),
        (
            MouseEventKind::Up(MouseButton::Right),
            1,
            1,
            KeyModifiers::empty(),
        ),
    ] {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind,
                column: x + dx,
                row: y + dy,
                modifiers,
            }))
            .unwrap();
        assert!(frontend.context.is_none());
    }
    let expected = b"\x1b[<2;1;1M\x1b[<34;2;2M\x1b[<2;2;2m";
    let target_capture = root.join(format!("input-{}.bin", pids[1]));
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && std::fs::read(&target_capture).is_ok_and(|bytes| bytes == expected)
    })
    .await;
    assert!(frontend.right_click.is_none());
    assert_eq!(
        std::fs::read(root.join(format!("input-{}.bin", pids[0]))).unwrap(),
        b""
    );
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert_eq!(
            public_before["snapshot"][key],
            api(&server.socket, "session.snapshot", json!({}))["snapshot"][key]
        );
    }
    for (column, row, modifiers) in [
        (x, y, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        (
            view.layout.pane_surface.x + pane.rect.x,
            view.layout.pane_surface.y + pane.rect.y,
            KeyModifiers::CONTROL,
        ),
    ] {
        frontend
            .dispatch_input(crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column,
                row,
                modifiers,
            }))
            .unwrap();
        assert!(frontend.context.is_some());
        for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
            frontend.dispatch_input(event).unwrap();
        }
        assert!(frontend.context.is_none());
    }
    assert_eq!(std::fs::read(&target_capture).unwrap(), expected);
    assert_eq!(
        std::fs::read(root.join(format!("input-{}.bin", pids[0]))).unwrap(),
        b""
    );
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    let server_pid = server.stop();
    let all_pids = [server_pid, pids[0], pids[1]];
    for pid in all_pids {
        assert!(!Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output()
            .unwrap()
            .status
            .success());
    }
    std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&json!({"scope":"real Cargo-built server/PTY and product frontend socket DI; not normal CLI or SSH", "result":"PASS","target_pane":second_id,"public_focus_before":public_before,"actual_stdin_hex":expected.iter().map(|b|format!("{b:02x}")).collect::<String>(),"non_target_stdin_bytes":0,"owned_pids_absent":all_pids})).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_frontend_jobs_two_click_owner_log_actual_process() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("frontend-job-popup-{}", std::process::id()));
    let mut server = OwnedServer::start(root.join("server"), "JOB-OWNER");
    let caller = api(
        &server.socket,
        "workspace.create",
        json!({"label":"job-caller","cwd":server.root,"focus":false}),
    );
    let caller_pane = caller["root_pane"]["pane_id"].as_str().unwrap().to_owned();
    let public_before = api(&server.socket, "session.snapshot", json!({}));
    let log = root.join("owned-job.log");
    std::fs::write(&log, "OWNED-JOB-LOG-MARKER\nsecond line\n").unwrap();
    let database = server
        .root
        .join("config")
        .join(crate::config::app_dir_name())
        .join("herdr.db");
    let store = crate::job::JobStore::open_at(database.clone()).unwrap();
    for (id, path, caller) in [
        ("job-owned", log.clone(), caller_pane.clone()),
        ("job-missing", root.join("missing.log"), caller_pane.clone()),
        ("job-gone", log.clone(), "not-a-live-caller".into()),
    ] {
        store
            .insert(&crate::job::JobRecord {
                id: id.into(),
                label: id.into(),
                command: "owned command".into(),
                cwd: root.display().to_string(),
                caller_pane: caller,
                caller_agent: format!("owned-{id}"),
                completion: "none".into(),
                status: "queued".into(),
                runner_pid: None,
                exit_code: None,
                started_unix_ms: None,
                finished_unix_ms: None,
                log_path: path.display().to_string(),
            })
            .unwrap();
    }
    assert_eq!(
        store.get("job-owned").unwrap().unwrap().caller_pane,
        caller_pane
    );
    assert_eq!(
        store.get("job-missing").unwrap().unwrap().caller_pane,
        caller_pane
    );
    assert_eq!(
        store.get("job-gone").unwrap().unwrap().caller_pane,
        "not-a-live-caller"
    );
    let config = crate::config::Config::default();
    let (palette, _) =
        crate::app::resolve_effective_theme(&crate::app::theme_runtime_config(&config, true), None);
    let settings = ChromeSettings::from_config(&config, palette, None);
    let shell = ClientShellState::new();
    let view = ClientChrome::new(ChromeSettings::from_config(
        &config,
        settings.palette.clone(),
        None,
    ))
    .compute_view(&shell, SIZE.0, SIZE.1);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && surface_has(f, "READY:JOB-OWNER:")
            && f.runtime
                .shell
                .endpoint(&ClientEndpointId::Local)
                .and_then(|e| e.cache.snapshot().and_then(|s| e.jobs.for_snapshot(s)))
                .is_some_and(|jobs| jobs.jobs.len() == 3)
    })
    .await;
    let root_pid = server.marker_pid("READY:JOB-OWNER:");
    let caller_pid = api(
        &server.socket,
        "pane.process_info",
        json!({"pane_id":caller_pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    frontend.chrome.detail_view = crate::app::state::SidebarDetailView::Jobs;
    for job in ["job-missing", "job-gone", "job-owned", "job-owned"] {
        let before = frontend
            .runtime
            .shell
            .endpoint(&ClientEndpointId::Local)
            .unwrap()
            .cache
            .snapshot()
            .unwrap()
            .focused_pane_id
            .as_deref()
            .map(str::to_owned);
        let view = frontend
            .chrome
            .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
        let hit = view
            .hits
            .iter()
            .find(|hit| matches!(&hit.target,ChromeTarget::Job(key) if key.id==job))
            .unwrap();
        frontend
            .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: hit.rect.x,
                row: hit.rect.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }))
            .unwrap();
        assert!(frontend.job_request.is_some());
        pump(&mut frontend, |f| {
            f.job_request.is_none() && f.runtime.input_lease_current()
        })
        .await;
        if job != "job-owned" {
            assert_eq!(
                frontend
                    .runtime
                    .shell
                    .endpoint(&ClientEndpointId::Local)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .focused_pane_id
                    .as_deref(),
                before.as_deref()
            );
            assert!(frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .popup
                .is_none());
        } else if before.as_deref() != Some(&caller_pane) {
            assert_eq!(
                frontend
                    .runtime
                    .shell
                    .endpoint(&ClientEndpointId::Local)
                    .unwrap()
                    .cache
                    .snapshot()
                    .unwrap()
                    .focused_pane_id
                    .as_deref(),
                Some(caller_pane.as_str())
            );
            assert!(frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .popup
                .is_none());
        }
        let public = api(&server.socket, "session.snapshot", json!({}));
        for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
            assert!(public["snapshot"][key].is_string());
            assert!(public_before["snapshot"][key].is_string());
            assert_eq!(public["snapshot"][key], public_before["snapshot"][key]);
        }
    }
    pump(&mut frontend, |f| {
        f.runtime
            .shell
            .pane_surface
            .as_ref()
            .and_then(|s| s.popup.as_deref())
            .is_some_and(|popup| {
                popup
                    .frame
                    .cells
                    .iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
                    .contains("OWNED-JOB-LOG-MARKER")
            })
    })
    .await;
    let popup = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()
        .unwrap()
        .popup
        .as_ref()
        .unwrap();
    assert_eq!(popup.title, "job log: job-owned");
    let popup_terminal = popup.terminal_id.clone();
    pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
    async fn popup_request(
        frontend: &mut ClientFrontend,
        method: crate::api::schema::Method,
    ) -> Result<Value, super::super::commands::EndpointCommandError> {
        let endpoint = frontend.runtime.shell.active_endpoint_id.clone();
        let state = frontend.runtime.shell.endpoint(&endpoint).unwrap();
        let generation = state.generation.unwrap();
        let boot = state
            .cache
            .live_snapshot(generation)
            .unwrap()
            .boot_id
            .clone();
        let (id, update) = frontend.runtime.issue_method_with_id(method).unwrap();
        frontend.update(update).unwrap();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let event = frontend.runtime.reader_events.recv().await.unwrap();
                let update = frontend.runtime.receive(event, Instant::now());
                let result = update
                    .completed
                    .iter()
                    .find(|response| response.request_id == id)
                    .map(|response| {
                        assert_eq!(response.endpoint_id, endpoint);
                        assert_eq!(response.generation, generation);
                        assert_eq!(response.boot_id, boot);
                        response.result.clone()
                    });
                frontend.update(update).unwrap();
                if let Some(result) = result {
                    return result;
                }
            }
        })
        .await
        .unwrap()
    }
    let facts = popup_request(
        &mut frontend,
        crate::api::schema::Method::PopupGet(crate::api::schema::PopupTarget {
            terminal_id: popup_terminal.clone(),
        }),
    )
    .await
    .unwrap();
    let revision = facts["content_revision"].as_u64().unwrap();
    assert_eq!(
        facts["surface_revision"].as_u64(),
        frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()
            .map(|surface| surface.surface_revision)
    );
    assert_eq!(facts["terminal_id"], popup_terminal);
    assert!(revision.is_multiple_of(2));
    let selection = crate::api::schema::PopupSelectionReadParams {
        terminal_id: popup_terminal.clone(),
        anchor: crate::api::schema::PaneTextPoint { row: 0, col: 0 },
        cursor: crate::api::schema::PaneTextPoint { row: 0, col: 19 },
        content_revision: revision,
    };
    let copied = popup_request(
        &mut frontend,
        crate::api::schema::Method::PopupSelectionRead(selection.clone()),
    )
    .await
    .unwrap();
    assert_eq!(copied["text"], "OWNED-JOB-LOG-MARKER");
    assert_eq!(copied["terminal_id"], popup_terminal);
    assert_eq!(copied["content_revision"], revision);
    let mut stale = selection.clone();
    stale.content_revision = revision + 1;
    assert_eq!(
        popup_request(
            &mut frontend,
            crate::api::schema::Method::PopupSelectionRead(stale)
        )
        .await
        .unwrap_err()
        .code
        .as_deref(),
        Some("stale_content")
    );
    let mut wrong = selection;
    wrong.terminal_id = caller_pane.clone();
    assert_eq!(
        popup_request(
            &mut frontend,
            crate::api::schema::Method::PopupSelectionRead(wrong)
        )
        .await
        .unwrap_err()
        .code
        .as_deref(),
        Some("popup_not_open")
    );
    let scrolled = popup_request(
        &mut frontend,
        crate::api::schema::Method::PopupScroll(crate::api::schema::PopupScrollParams {
            terminal_id: popup_terminal.clone(),
            offset_from_bottom: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(scrolled["terminal_id"], popup_terminal);
    assert_eq!(scrolled["scroll"]["offset_from_bottom"], 0);
    let popup_view =
        frontend
            .chrome
            .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let geometry = super::popup::geometry(
        frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()
            .unwrap()
            .popup
            .as_ref()
            .unwrap(),
        popup_view.layout.pane_surface,
    )
    .unwrap();
    frontend.copy_on_select = true;
    for (kind, column) in [
        (
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            geometry.inner.x,
        ),
        (
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            geometry.inner.x + 5,
        ),
    ] {
        frontend
            .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row: geometry.inner.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }))
            .unwrap();
    }
    pump(&mut frontend, |f| {
        super::popup_selection::range_for_test(f) == Some(((0, 0), (0, 5)))
    })
    .await;
    let popup_pointer_range = super::popup_selection::range_for_test(&frontend);
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[200~z\x1b[201~") {
        frontend.dispatch_input(event).unwrap();
    }
    assert_eq!(
        super::popup_selection::range_for_test(&frontend),
        popup_pointer_range
    );
    frontend.dispatch_input(RawInputEvent::LineFeed).unwrap();
    assert_eq!(
        super::popup_selection::range_for_test(&frontend),
        popup_pointer_range
    );
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"x") {
        frontend.dispatch_input(event).unwrap();
    }
    assert!(super::popup_selection::range_for_test(&frontend).is_none());
    assert!(frontend.popup_selection.deadline.is_none());
    let history_rows = usize::from(SIZE.1) * 2;
    let history = (0..history_rows)
        .map(|row| format!("ROW-{row:03}\n"))
        .collect::<String>();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(history.as_bytes())
        .unwrap();
    let last_history_row = format!("ROW-{:03}", history_rows - 1);
    eprintln!("active-timer phase=history-presented test_pid={} root_pid={root_pid} caller_pid={caller_pid}", std::process::id());
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && f.runtime
                .shell
                .pane_surface
                .as_ref()
                .and_then(|s| s.popup.as_ref())
                .is_some_and(|popup| {
                    popup
                        .frame
                        .cells
                        .iter()
                        .map(|c| c.symbol.as_str())
                        .collect::<String>()
                        .contains(&last_history_row)
                })
    })
    .await;
    let mut active_timer_cancellations = Vec::new();
    for cancel in ["key", "workspace-switch"] {
        let view = frontend
            .chrome
            .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
        let popup = frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()
            .unwrap()
            .popup
            .as_ref()
            .unwrap();
        let geometry = super::popup::geometry(popup, view.layout.pane_surface).unwrap();
        for (kind, row) in [
            (
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                geometry.inner.y + geometry.inner.height / 2,
            ),
            (
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                geometry.inner.y.saturating_sub(1),
            ),
        ] {
            let lease_before_mouse = frontend.runtime.input_lease_current();
            frontend
                .dispatch_input(RawInputEvent::Mouse(crossterm::event::MouseEvent {
                    kind,
                    column: geometry.inner.x,
                    row,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                }))
                .unwrap();
            eprintln!("active-timer mouse={kind:?} operation={cancel} lease_before={lease_before_mouse} lease_after={} range={:?} deadline={:?} surface={}", frontend.runtime.input_lease_current(), super::popup_selection::range_for_test(&frontend), frontend.popup_selection.deadline, frontend.runtime.shell.pane_surface.as_ref().unwrap().surface_revision);
            if matches!(kind, crossterm::event::MouseEventKind::Down(_)) {
                pump(&mut frontend, |f| {
                    f.runtime.input_lease_current()
                        && super::popup_selection::range_for_test(f).is_some()
                })
                .await;
            }
        }
        eprintln!(
            "active-timer phase={cancel}-timer-ready lease={} deadline={:?} range={:?}",
            frontend.runtime.input_lease_current(),
            frontend.popup_selection.deadline,
            super::popup_selection::range_for_test(&frontend)
        );
        pump(&mut frontend, |f| {
            f.popup_selection.deadline.is_some()
                && super::popup_selection::range_for_test(f).is_some()
        })
        .await;
        eprintln!("active-timer phase={cancel}-lease-ready");
        pump(&mut frontend, |f| f.runtime.input_lease_current()).await;
        let range_before = super::popup_selection::range_for_test(&frontend).unwrap();
        let deadline_before = frontend.popup_selection.deadline;
        assert!(deadline_before.is_some());
        if cancel == "key" {
            let before_key_surface = frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .surface_revision;
            for event in crate::raw_input::parse_raw_input_bytes_sync(b"x") {
                frontend.dispatch_input(event).unwrap();
            }
            eprintln!("active-timer phase=key-new-surface-presented");
            pump(&mut frontend, |f| {
                f.runtime.input_lease_current()
                    && f.runtime
                        .shell
                        .pane_surface
                        .as_ref()
                        .is_some_and(|s| s.surface_revision > before_key_surface)
            })
            .await;
        } else {
            let update = frontend.runtime.activate(
                ClientEndpointId::Local,
                Some(crate::client::endpoint::FocusTarget::Pane(
                    server.pane.clone(),
                )),
                Instant::now(),
            );
            frontend.update(update).unwrap();
            eprintln!("active-timer phase={cancel}-target-presented");
            pump(&mut frontend, |f| {
                f.runtime.input_lease_current()
                    && surface_has(f, "READY:JOB-OWNER:")
                    && f.runtime
                        .shell
                        .pane_surface
                        .as_ref()
                        .is_some_and(|s| s.popup.is_none())
            })
            .await;
        }
        assert!(super::popup_selection::range_for_test(&frontend).is_none());
        assert!(frontend.popup_selection.deadline.is_none());
        let cancelled_scroll = api(
            &server.socket,
            "popup.get",
            json!({"terminal_id":popup_terminal}),
        )["scroll"]
            .clone();
        // Exercise the original due handler after cancellation; it must issue no scroll.
        super::popup_selection::tick(&mut frontend, deadline_before.unwrap()).unwrap();
        let after_due_handler = api(
            &server.socket,
            "popup.get",
            json!({"terminal_id":popup_terminal}),
        )["scroll"]
            .clone();
        assert!(cancelled_scroll["offset_from_bottom"].is_u64());
        assert_eq!(after_due_handler, cancelled_scroll);
        active_timer_cancellations.push(json!({"operation":cancel,
            "range_before":range_before,"deadline_before_some":deadline_before.is_some(),
            "range_after":super::popup_selection::range_for_test(&frontend),
            "deadline_after_some":frontend.popup_selection.deadline.is_some(),
            "old_popup_scroll_after_cancel":cancelled_scroll,
            "old_popup_scroll_after_due_handler":after_due_handler}));
        if cancel == "workspace-switch" {
            let update = frontend.runtime.activate(
                ClientEndpointId::Local,
                Some(crate::client::endpoint::FocusTarget::Pane(
                    caller_pane.clone(),
                )),
                Instant::now(),
            );
            frontend.update(update).unwrap();
            eprintln!("active-timer phase={cancel}-target-presented");
            pump(&mut frontend, |f| {
                f.runtime.input_lease_current()
                    && f.runtime
                        .shell
                        .pane_surface
                        .as_ref()
                        .is_some_and(|s| s.popup.is_some())
            })
            .await;
            assert!(super::popup_selection::range_for_test(&frontend).is_none());
            assert!(frontend.popup_selection.deadline.is_none());
        }
    }
    let frame = frame(&mut frontend);
    let server_pid = server.child.as_ref().unwrap().id();
    let children = Command::new("pgrep")
        .args(["-P", &server_pid.to_string()])
        .output()
        .unwrap();
    let popup_pid = String::from_utf8(children.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.parse::<u32>().ok())
        .find(|pid| *pid != root_pid && *pid != caller_pid)
        .expect("owned real log viewer PID");
    let args = Command::new("ps")
        .args(["-p", &popup_pid.to_string(), "-o", "args="])
        .output()
        .unwrap();
    let args = String::from_utf8(args.stdout).unwrap();
    assert!(args.contains("__job-log-view") && args.contains(log.to_str().unwrap()));
    for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
        frontend.dispatch_input(event).unwrap();
    }
    pump(&mut frontend, |f| {
        f.runtime
            .shell
            .pane_surface
            .as_ref()
            .is_some_and(|surface| surface.popup.is_none())
    })
    .await;
    let public = api(&server.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert!(public["snapshot"][key].is_string());
        assert!(public_before["snapshot"][key].is_string());
        assert_eq!(public["snapshot"][key], public_before["snapshot"][key]);
    }
    server.stop();
    let pids = [server_pid, root_pid, caller_pid, popup_pid];
    for pid in pids {
        assert!(!crate::platform::process_exists(pid));
    }
    std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&json!({
        "scope":"independent Cargo-built owner server / product frontend real socket DI Jobs two-click and actual log viewer",
        "database":database,"log":log,"popup_frame":frame,"owned_pids":pids,"public_snapshot_unchanged":true,
        "public_focus_before":public_before["snapshot"],"public_focus_after":public["snapshot"],
        "popup_runtime_facts":facts,"popup_selection":copied,"popup_scroll":scrolled,"popup_pointer_range":popup_pointer_range,"active_timer_cancellations":active_timer_cancellations
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_frontend_notification_actual_owner_api_renders_and_targetless_key_is_noop() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("notification-process-{}", std::process::id()));
    let mut server = OwnedServer::start_with_settings(
        root.clone(),
        "NOTIFICATION",
        "",
        "[ui.toast]\ndelivery = \"herdr\"\n",
    );
    let config: crate::config::Config =
        toml::from_str("[ui.toast]\ndelivery = \"herdr\"\n[ui.sound]\nenabled = false\n").unwrap();
    let (palette, _) =
        crate::app::resolve_effective_theme(&crate::app::theme_runtime_config(&config, true), None);
    let settings = ChromeSettings::from_config(&config, palette, None);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: SIZE.0,
            rows: SIZE.1,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        ClientShellState::new(),
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &server);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();
    pump(&mut frontend, |frontend| {
        frontend.runtime.input_lease_current() && surface_has(frontend, "READY:NOTIFICATION")
    })
    .await;
    let public_before = api(&server.socket, "session.snapshot", json!({}));
    let shown = api(
        &server.socket,
        "notification.show",
        json!({"title":"OWNED-NOTIFICATION", "body":"OWNED-NOTIFICATION-CONTEXT", "sound":"none", "position":"bottom-right"}),
    );
    assert_eq!(shown["shown"], true);
    pump(&mut frontend, |frontend| {
        frontend
            .runtime
            .shell
            .endpoint(&ClientEndpointId::Local)
            .and_then(|endpoint| endpoint.cache.notification(1))
            .is_some()
    })
    .await;
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, SIZE.0, SIZE.1);
    let actual = frontend
        .chrome
        .render_with_overlay(&view, |frame| notification::render(&frontend, frame));
    let text = actual
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("OWNED-NOTIFICATION"));
    assert!(text.contains("OWNED-NOTIFICATION-CONTEXT"));
    assert!(notification::rect(&frontend).is_some());
    notification::open(&mut frontend).unwrap();
    assert!(notification::rect(&frontend).is_some());
    frontend.host_settings.toast = crate::config::ToastDelivery::Off;
    assert!(notification::rect(&frontend).is_none());
    frontend.host_settings.toast = crate::config::ToastDelivery::Herdr;
    assert!(notification::rect(&frontend).is_some());
    assert_eq!(
        api(&server.socket, "session.snapshot", json!({})),
        public_before
    );
    let server_pid = server.child.as_ref().unwrap().id();
    let child_pid = api(
        &server.socket,
        "pane.process_info",
        json!({"pane_id":server.pane}),
    )["process_info"]["shell_pid"]
        .as_u64()
        .unwrap() as u32;
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    server.stop();
    assert!(!crate::platform::process_exists(server_pid));
    assert!(!crate::platform::process_exists(child_pid));
    std::fs::write(root.join("evidence.json"), serde_json::to_vec_pretty(&json!({"scope":"real owned server notification.show JSON projection/render; targetless no-op only, not target switch/normal SSH", "frame":actual, "owned_pids":[server_pid,child_pid], "public_before":public_before, "shown":shown})).unwrap()).unwrap();
}

#[tokio::test]
async fn endpoint_frontend_custom_actual_keys_shell_pane_popup_preserve_owner() {
    let root = std::env::current_dir()
        .unwrap()
        .join(".local")
        .join(format!("custom-process-{}", std::process::id()));
    let mut local = OwnedServer::start(root.join("local"), "CUSTOM-LOCAL");
    let mut other = OwnedServer::start(root.join("other"), "CUSTOM-OTHER");
    let mut config_text = String::from("[ui.sound]\nenabled = false\n[keys]\nprefix = 'ctrl+a'\n");
    for (key, action) in [("y", "shell"), ("v", "pane"), ("j", "popup")] {
        let path = other.root.join(format!("{action}.txt"));
        let mut command = format!("printf '%s\\n' \"$$\" \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'", path.display());
        if action != "shell" {
            command.push_str("; printf 'OWNED-CUSTOM-READY\\n'; read -r line");
        }
        config_text.push_str(&format!(
            "[[keys.command]]\nkey = 'prefix+{key}'\ntype = '{action}'\ncommand = {}\n",
            serde_json::to_string(&command).unwrap()
        ));
        if action == "popup" {
            config_text.push_str("width = 50\nheight = 12\n");
        }
    }
    let config: crate::config::Config = toml::from_str(&config_text).unwrap();
    let (palette, _) =
        crate::app::resolve_effective_theme(&crate::app::theme_runtime_config(&config, true), None);
    let settings = ChromeSettings::from_config(&config, palette, None);
    let mut shell = ClientShellState::new();
    let other_id = ClientEndpointId::Ssh("m-custom-owned-socket".into());
    shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
        id: "m-custom-owned-socket".into(),
        label: "Custom owner".into(),
        target: "never-contacted".into(),
        session: "owned".into(),
        enabled: true,
    }]);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: SIZE.0,
            rows: SIZE.1,
        },
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_geometry_exact: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: false,
    };
    let runtime = EndpointRuntime::new(
        shell,
        EndpointRegistry::empty(),
        EndpointSupervisors::new(&[], Instant::now()),
        options,
    );
    let mut frontend = ClientFrontend::from_runtime(runtime, &config, settings, SIZE, options);
    install(&mut frontend, &ClientEndpointId::Local, 1, &local);
    install(&mut frontend, &other_id, 1, &other);
    let update = frontend
        .runtime
        .activate(ClientEndpointId::Local, None, Instant::now());
    frontend.update(update).unwrap();
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current()
            && surface_has(f, "READY:CUSTOM-LOCAL")
            && f.runtime
                .shell
                .endpoint(&other_id)
                .unwrap()
                .cache
                .snapshot()
                .is_some()
    })
    .await;
    click(&mut frontend, &other_id, &other.workspace);
    pump(&mut frontend, |f| {
        f.runtime.input_lease_current() && surface_has(f, "READY:CUSTOM-OTHER")
    })
    .await;
    let local_before = api(&local.socket, "session.snapshot", json!({}));
    let public_before = api(&other.socket, "session.snapshot", json!({}));
    let original = input::focused_pane(&frontend).unwrap();
    prefix_key(&mut frontend, b'[');
    assert!(copy::active(&frontend));
    let copy_entry = copy::entry_offset_for_test(&frontend).unwrap();
    let mut frames = Vec::new();
    let mut launched_pids = Vec::new();
    let mut envs = Vec::new();
    for (key, action) in [(b'y', "shell"), (b'v', "pane"), (b'j', "popup")] {
        prefix_key(&mut frontend, key);
        let output = other.root.join(format!("{action}.txt"));
        pump(&mut frontend, |f| {
            f.custom_request.is_none()
                && f.runtime.input_lease_current()
                && output.is_file()
                && (action == "shell"
                    || (action == "pane" && surface_has(f, "OWNED-CUSTOM-READY"))
                    || (action == "popup"
                        && f.runtime
                            .shell
                            .pane_surface
                            .as_ref()
                            .and_then(|surface| surface.popup.as_ref())
                            .is_some_and(|popup| {
                                popup
                                    .frame
                                    .cells
                                    .iter()
                                    .map(|cell| cell.symbol.as_str())
                                    .collect::<String>()
                                    .contains("OWNED-CUSTOM-READY")
                            })))
        })
        .await;
        assert!(!copy::active(&frontend));
        if action == "shell" {
            let pane = frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .panes
                .iter()
                .find(|pane| pane.pane_id == original.id)
                .unwrap();
            assert_eq!(pane.scroll.unwrap().offset_from_bottom, copy_entry);
        }
        let text = std::fs::read_to_string(&output).unwrap();
        let mut lines = text.lines();
        launched_pids.push(lines.next().unwrap().parse::<u32>().unwrap());
        let env = lines.collect::<Vec<_>>().join("\n");
        assert_eq!(env.lines().count(), 3);
        assert!(env.lines().all(|line| !line.is_empty()));
        envs.push(env);
        frames.push(frame(&mut frontend));
        if action == "pane" {
            assert_ne!(input::focused_pane(&frontend).unwrap(), original);
        } else {
            assert_eq!(input::focused_pane(&frontend).unwrap(), original);
        }
        if action == "popup" {
            let popup = frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .unwrap()
                .popup
                .as_ref()
                .unwrap();
            assert_eq!(popup.width, Some(wire::ClientShellPopupSize::Cells(50)));
            assert_eq!(popup.height, Some(wire::ClientShellPopupSize::Cells(12)));
            let geometry = crate::popup_size::resolve_popup_geometry(
                Some(crate::popup_size::PopupSize::Cells(50)),
                Some(crate::popup_size::PopupSize::Cells(12)),
                ratatui::layout::Rect::new(
                    0,
                    0,
                    frontend.options.surface_size.cols,
                    frontend.options.surface_size.rows,
                ),
            )
            .unwrap();
            assert_eq!(
                (popup.frame.width, popup.frame.height),
                (geometry.inner.width, geometry.inner.height)
            );
        }
        if action != "shell" {
            for event in crate::raw_input::parse_raw_input_bytes_sync(b"\r") {
                frontend.dispatch_input(event).unwrap();
            }
            pump(&mut frontend, |f| {
                f.runtime.input_lease_current()
                    && input::focused_pane(f).as_ref() == Some(&original)
                    && f.runtime
                        .shell
                        .pane_surface
                        .as_ref()
                        .is_some_and(|surface| surface.popup.is_none())
                    && surface_has(f, "READY:CUSTOM-OTHER")
            })
            .await;
        }
        for name in ["shell", "pane", "popup"] {
            assert!(!local.root.join(format!("{name}.txt")).exists());
        }
    }
    assert_eq!(envs[0], envs[1]);
    assert_eq!(envs[0], envs[2]);
    let public_after = api(&other.socket, "session.snapshot", json!({}));
    for key in ["focused_workspace_id", "focused_tab_id", "focused_pane_id"] {
        assert!(public_before["snapshot"][key]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            public_before["snapshot"][key],
            public_after["snapshot"][key]
        );
    }
    assert_eq!(
        api(&local.socket, "session.snapshot", json!({})),
        local_before
    );
    let mut pids = vec![
        local.child.as_ref().unwrap().id(),
        other.child.as_ref().unwrap().id(),
    ];
    for server in [&local, &other] {
        pids.push(
            api(
                &server.socket,
                "pane.process_info",
                json!({"pane_id":server.pane}),
            )["process_info"]["shell_pid"]
                .as_u64()
                .unwrap() as u32,
        );
    }
    pids.extend(launched_pids);
    frontend
        .runtime
        .endpoints
        .disconnect(&ClientEndpointId::Local);
    frontend.runtime.endpoints.disconnect(&other_id);
    local.stop();
    other.stop();
    for pid in &pids {
        assert!(!crate::platform::process_exists(*pid));
    }
    std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&json!({"scope":"actual independent servers / product frontend custom prefix keys Shell Pane Popup only, not normal ANSI or PluginAction", "frames":frames,"env":envs,"public_before":public_before["snapshot"],"public_after":public_after["snapshot"],"owned_pids":pids})).unwrap()).unwrap();
}
