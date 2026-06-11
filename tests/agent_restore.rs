//! End-to-end test for `[agent_restore]`: a reported agent session survives a
//! full server restart and is relaunched automatically in the restored pane.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, wait_for_socket, wait_until,
};

const SESSION_ID: &str = "abcdabcd-1111-2222-3333-444455556666";

/// Debug builds isolate their config under `herdr-dev` (see
/// `config::io::app_dir_name`); the spawned server binary matches this
/// test's build profile.
fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-agent-restore-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn spawn_server(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket: &PathBuf,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        r#"onboarding = false

[agent_restore]
enabled = true
restore_delay_ms = 300

[agent_restore.commands]
claude = "echo RESTORED-{session_id}"
"#,
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn send_json_request(socket_path: &PathBuf, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn request_server_stop(socket_path: &PathBuf) {
    if let Ok(mut stream) = UnixStream::connect(socket_path) {
        let _ = writeln!(
            stream,
            r#"{{"id":"stop","method":"server.stop","params":{{}}}}"#
        );
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        let _ = reader.read_line(&mut response);
    }
}

fn first_pane_id(socket_path: &PathBuf) -> String {
    let panes = send_json_request(
        socket_path,
        r#"{"id":"panes","method":"pane.list","params":{}}"#,
    );
    panes["result"]["panes"][0]["pane_id"]
        .as_str()
        .expect("restored pane")
        .to_string()
}

fn pane_recent_text(socket_path: &PathBuf, pane_id: &str) -> String {
    let read = send_json_request(
        socket_path,
        &format!(
            r#"{{"id":"read","method":"pane.read","params":{{"pane_id":"{pane_id}","source":"recent"}}}}"#
        ),
    );
    read["result"]["read"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn agent_restore_relaunches_recorded_agent_after_server_restart() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let created = send_json_request(
        &api_socket,
        r#"{"id":"create","method":"workspace.create","params":{"label":"restore-e2e"}}"#,
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("created pane")
        .to_string();

    let report = format!(
        r#"{{"id":"report","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","source":"herdr:claude","agent":"claude","state":"working","seq":1,"session_id":"{SESSION_ID}"}}}}"#
    );
    let response = send_json_request(&api_socket, &report);
    assert_eq!(response["result"]["type"], "ok", "report: {response}");

    // The session snapshot is written on a debounce; wait until it carries
    // the reported agent + session id before restarting.
    let session_file = config_home.join(app_dir_name()).join("session.json");
    assert!(
        wait_until(Duration::from_secs(10), Duration::from_millis(100), || {
            fs::read_to_string(&session_file)
                .map(|content| content.contains(SESSION_ID))
                .unwrap_or(false)
        }),
        "session.json should record the reported agent session id"
    );

    request_server_stop(&api_socket);
    assert!(
        wait_until(Duration::from_secs(10), Duration::from_millis(100), || {
            UnixStream::connect(&api_socket).is_err()
        }),
        "server should stop"
    );
    drop(server);

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // The startup restore should type the configured command into the
    // restored pane after restore_delay_ms, and the pane shell executes it.
    let expected = format!("RESTORED-{SESSION_ID}");
    let restored_pane = first_pane_id(&api_socket);
    assert!(
        wait_until(Duration::from_secs(15), Duration::from_millis(200), || {
            pane_recent_text(&api_socket, &restored_pane).contains(&expected)
        }),
        "restored pane should run the configured restore command; last text:\n{}",
        pane_recent_text(&api_socket, &restored_pane)
    );

    // The pending marker is consumed: a manual rerun has nothing to do.
    let rerun = send_json_request(
        &api_socket,
        r#"{"id":"rerun","method":"agent.restore","params":{"dry_run":true}}"#,
    );
    assert_eq!(rerun["result"]["type"], "agent_restore", "rerun: {rerun}");
    assert_eq!(
        rerun["result"]["actions"].as_array().map(Vec::len),
        Some(0),
        "rerun: {rerun}"
    );

    drop(server);
    cleanup_test_base(&base);
}
