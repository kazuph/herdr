#![cfg(unix)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use support::CURRENT_PROTOCOL;

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-server-not-running-{}-{nanos}",
        std::process::id()
    ))
}

fn assert_server_not_running(output: Output, socket_path: &Path, request_id: &str, command: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "stderr: {stderr:?}");
    let response: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(response["id"], request_id);
    assert_eq!(response["error"]["code"], "server_not_running");
    assert_eq!(
        response["error"]["message"],
        format!(
            "no herdr server is running at {}; run `{command}` to start or attach it",
            socket_path.display()
        )
    );
}

#[test]
fn dead_server_guidance_keeps_current_and_alternate_binary_ownership() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();

    let current_binary = PathBuf::from(env!("CARGO_BIN_EXE_herdr"));
    let named_socket = config_home
        .join("herdr-dev")
        .join("sessions")
        .join("foo")
        .join("herdr.sock");
    let named = Command::new(&current_binary)
        .args(["--session", "foo", "workspace", "create"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert_server_not_running(
        named,
        &named_socket,
        "cli:workspace:create",
        &format!("{} session attach foo", current_binary.display()),
    );

    let alternate = base.join("herdr next's");
    fs::copy(&current_binary, &alternate).unwrap();
    let mut permissions = fs::metadata(&alternate).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&alternate, permissions).unwrap();
    let explicit_socket = runtime_dir.join("alternate.sock");
    let explicit = Command::new(&alternate)
        .args(["workspace", "create"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &explicit_socket)
        .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
        .env("HERDR_SESSION", "unrelated")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    let alternate_command = format!(
        "'{}'",
        alternate.display().to_string().replace('\'', "'\\''")
    );
    assert_server_not_running(
        explicit,
        &explicit_socket,
        "cli:workspace:create",
        &alternate_command,
    );

    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dead_server_agent_start_and_live_handoff_report_one_json_line() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    let missing_socket = runtime_dir.join("missing.sock");
    let current_binary = PathBuf::from(env!("CARGO_BIN_EXE_herdr"));
    let current_command = current_binary.display().to_string();

    let agent_start = Command::new(&current_binary)
        .args(["agent", "start", "worker", "--", "/bin/true"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert_server_not_running(
        agent_start,
        &missing_socket,
        "cli:agent:start",
        &current_command,
    );

    let live_handoff = Command::new(&current_binary)
        .args(["server", "live-handoff"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert_server_not_running(
        live_handoff,
        &missing_socket,
        "cli:server:live-handoff",
        &current_command,
    );

    fs::remove_dir_all(base).unwrap();
}

#[test]
fn agent_wait_reports_server_not_running_when_subscription_socket_disappears() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server_socket_path = socket_path.clone();

    let server = thread::spawn(move || {
        let (mut first_ping_stream, _) = listener.accept().unwrap();
        let mut first_ping_line = String::new();
        BufReader::new(first_ping_stream.try_clone().unwrap())
            .read_line(&mut first_ping_line)
            .unwrap();
        let first_ping: serde_json::Value = serde_json::from_str(&first_ping_line).unwrap();
        assert_eq!(first_ping["method"], "ping");
        write_fake_pong(&mut first_ping_stream, &first_ping);

        let (mut get_stream, _) = listener.accept().unwrap();
        let mut get_line = String::new();
        BufReader::new(get_stream.try_clone().unwrap())
            .read_line(&mut get_line)
            .unwrap();
        let get_request: serde_json::Value = serde_json::from_str(&get_line).unwrap();
        assert_eq!(get_request["method"], "agent.get");
        get_stream
            .write_all(
                br#"{"id":"cli:agent:wait:resolve","result":{"type":"agent_info","agent":{"pane_id":"p1","agent_status":"working"}}}"#,
            )
            .unwrap();
        get_stream.write_all(b"\n").unwrap();
        get_stream.flush().unwrap();

        let (mut second_ping_stream, _) = listener.accept().unwrap();
        let mut second_ping_line = String::new();
        BufReader::new(second_ping_stream.try_clone().unwrap())
            .read_line(&mut second_ping_line)
            .unwrap();
        let second_ping: serde_json::Value = serde_json::from_str(&second_ping_line).unwrap();
        assert_eq!(second_ping["method"], "ping");
        write_fake_pong(&mut second_ping_stream, &second_ping);

        drop(second_ping_stream);
        drop(listener);
        fs::remove_file(server_socket_path).unwrap();
    });

    let waited = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args([
            "agent",
            "wait",
            "worker",
            "--status",
            "blocked",
            "--timeout",
            "5000",
        ])
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert_server_not_running(
        waited,
        &socket_path,
        "cli:agent:wait",
        env!("CARGO_BIN_EXE_herdr"),
    );

    server.join().unwrap();
    fs::remove_dir_all(base).unwrap();
}

fn write_fake_pong(stream: &mut std::os::unix::net::UnixStream, request: &serde_json::Value) {
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "id": request["id"],
            "result": {
                "type": "pong",
                "version": "current",
                "protocol": CURRENT_PROTOCOL,
                "capabilities": {
                    "live_handoff": true,
                    "detached_server_daemon": true
                }
            }
        })
    )
    .unwrap();
    stream.flush().unwrap();
}
