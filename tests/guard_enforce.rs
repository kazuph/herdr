mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid,
};

const HERDR: &str = env!("CARGO_BIN_EXE_herdr");
const REQUEST_REASON: &str =
    "対象プロジェクト: herdr。必要な理由: guard の動作確認。実施内容: ビルド出力の削除。確認済み: 再生成可能。戻し方: cargo build で再作成。";

fn unique_test_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/hge-{}-{}", std::process::id(), nanos()))
}

/// Workdir outside /tmp so `rm` targets are not auto-judged as tmp-safe.
fn unique_test_dir_in_var_tmp() -> PathBuf {
    PathBuf::from(format!("/var/tmp/hge-{}-{}", std::process::id(), nanos()))
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
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

/// Directories and paths for one isolated guard-enforce scenario.
struct GuardEnv {
    base: PathBuf,
    socket_path: PathBuf,
    workdir: PathBuf,
    marker_path: PathBuf,
    log_path: PathBuf,
    approval_file: PathBuf,
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn send_request(socket_path: &Path, json: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
    stream.set_nonblocking(true).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for {json}");
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8(buf.drain(..=pos).collect()).unwrap();
            return serde_json::from_str(&line).unwrap();
        }
        let mut bytes = [0u8; 256];
        match stream.read(&mut bytes) {
            Ok(0) => panic!("stream closed while waiting for {json}"),
            Ok(n) => buf.extend_from_slice(&bytes[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("socket read failed: {err}"),
        }
    }
}

/// Create the isolated config/state/runtime layout and start `herdr server`.
///
/// `extra_config` is appended to the `[guard]` section (e.g.
/// `ask_timeout_seconds = 2`). `mode` selects `[guard].mode`; `None` keeps
/// the `enforce` default.
fn setup(mode: Option<&str>, extra_config: &str, write_marker: bool) -> (GuardEnv, SpawnedHerdr) {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let state_home = base.join("state");
    // Debug builds namespace app state under `herdr-dev`.
    let state_dir = state_home.join("herdr-dev");
    let socket_path = runtime_dir.join("herdr.sock");
    // rm targets under /tmp are always judged safe, so the workdir must live
    // outside tmp for `rm -rf ./build` to reach `ask`.
    let workdir = unique_test_dir_in_var_tmp();
    let home = base.join("home");
    let marker_path = state_dir.join("guard/agent-panes/p_e2e.json");
    let log_path = base.join("guard-log.jsonl");
    let approval_file = base.join("approvals");

    fs::create_dir_all(config_home.join("herdr-dev")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&home).unwrap();
    register_runtime_dir(&runtime_dir);

    let mut config = format!(
        "onboarding = false\n\
         [guard]\n\
         log_path = \"{}\"\n\
         approval_file = \"{}\"\n{extra_config}",
        log_path.display(),
        approval_file.display()
    );
    if let Some(mode) = mode {
        config.push_str(&format!("mode = \"{mode}\"\n"));
    }
    fs::write(config_home.join("herdr-dev/config.toml"), config).unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(HERDR);
    cmd.arg("server");
    cmd.cwd(&workdir);
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_STATE_HOME", &state_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", &socket_path);
    cmd.env("HERDR_SOCKET_PATH_EXPLICIT", "1");
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env_remove("HERDR_ENV");
    cmd.env("HOME", &home);
    cmd.env("SHELL", "/bin/sh");
    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    wait_for_socket(&socket_path, Duration::from_secs(15));

    // The API socket is bound before the App is created, and `App::new` wipes
    // stale guard markers; the marker therefore has to be written only after
    // a stateful request proves the App is serving.
    let ready = send_request(
        &socket_path,
        r#"{"id":"ready","method":"decision.list","params":{}}"#,
    );
    assert!(
        ready.get("result").is_some(),
        "server not serving decisions: {ready}"
    );
    if write_marker {
        write_agent_marker(&marker_path, &workdir);
    }

    (
        GuardEnv {
            base,
            socket_path,
            workdir,
            marker_path,
            log_path,
            approval_file,
        },
        SpawnedHerdr {
            _master: pair.master,
            child,
        },
    )
}

/// Spawn `herdr guard exec -- <argv...>` as the pane would run it: inside the
/// isolated workdir with the marker/socket env the pane launch would set.
fn guard_exec(env: &GuardEnv, argv: &[&str], request_reason: Option<&str>) -> Child {
    let mut cmd = Command::new(HERDR);
    cmd.arg("guard")
        .arg("exec")
        .arg("--")
        .args(argv)
        .current_dir(&env.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.env("XDG_CONFIG_HOME", env.base.join("config"));
    cmd.env("XDG_STATE_HOME", env.base.join("state"));
    cmd.env("HOME", env.base.join("home"));
    cmd.env("HERDR_SOCKET_PATH", &env.socket_path);
    cmd.env("HERDR_SOCKET_PATH_EXPLICIT", "1");
    cmd.env("HERDR_PANE_ID", "p_e2e");
    cmd.env("HERDR_GUARD_MARKER", &env.marker_path);
    cmd.env("HERDR_GUARD_SHIM_DIR", env.base.join("shim"));
    cmd.env("HERDR_GUARD_HERDR", HERDR);
    for var in [
        "HERDR_CLIENT_SOCKET_PATH",
        "HERDR_SOCKET_PATH_EXPLICIT",
        "HERDR_GUARD_VIA",
        "HERDR_WORKSPACE_ID",
        "HERDR_TAB_ID",
        "AI_GUARD_REQUEST_REASON",
        "AI_GUARD_REQUEST_ENV_NAME",
        "AI_GUARD_TEMP_APPROVAL_SECONDS",
        "AI_GUARD_DANGER_WORD_ACK",
        "ZDOTDIR",
        "BASH_ENV",
    ] {
        cmd.env_remove(var);
    }
    if let Some(reason) = request_reason {
        cmd.env("AI_GUARD_REQUEST_REASON", reason);
    }
    cmd.spawn().unwrap()
}

fn wait_child(child: &mut Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_end(&mut stdout);
            }
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_end(&mut stderr);
            }
            return Output {
                status,
                stdout,
                stderr,
            };
        }
        assert!(Instant::now() < deadline, "guard exec did not finish");
        thread::sleep(Duration::from_millis(25));
    }
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn decisions(env: &GuardEnv, status: Option<&str>) -> Vec<serde_json::Value> {
    let params = match status {
        Some(status) => format!("{{\"status\":\"{status}\"}}"),
        None => "{}".to_string(),
    };
    let response = send_request(
        &env.socket_path,
        &format!("{{\"id\":\"list\",\"method\":\"decision.list\",\"params\":{params}}}"),
    );
    response["result"]["decisions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn pending_decision(env: &GuardEnv, timeout: Duration) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(decision) = decisions(env, Some("pending")).into_iter().next() {
            return decision;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("no pending decision appeared");
}

fn answer(
    env: &GuardEnv,
    decision_id: &str,
    option: &str,
    text: Option<&str>,
) -> serde_json::Value {
    let text_part = text
        .map(|text| format!(",\"text\":{text:?}"))
        .unwrap_or_default();
    send_request(
        &env.socket_path,
        &format!(
            "{{\"id\":\"answer\",\"method\":\"decision.answer\",\"params\":{{\"decision_id\":\"{decision_id}\",\"option_id\":\"{option}\"{text_part}}}}}"
        ),
    )
}

fn read_log_records(env: &GuardEnv) -> Vec<serde_json::Value> {
    fs::read_to_string(&env.log_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn cleanup(env: GuardEnv, server: SpawnedHerdr) {
    let workdir = env.workdir.clone();
    drop(server);
    cleanup_test_base(&env.base);
    let _ = fs::remove_dir_all(&workdir);
}

#[test]
fn enforce_blocks_main_push_without_executing() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);

    let mut child = guard_exec(
        &env,
        &["git", "push", "origin", "main"],
        Some(REQUEST_REASON),
    );
    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.contains("❌ ブロック（承認不可）: git push origin main"),
        "{stderr}"
    );
    assert!(stderr.contains("承認では解除できません"), "{stderr}");
    assert!(decisions(&env, None).is_empty(), "block created a decision");

    let records = read_log_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["verdict"], "block");
    assert_eq!(records[0]["rule"], "git-push-main");

    cleanup(env, server);
}

#[test]
fn enforce_ask_approval_executes_the_command() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build/sub")).unwrap();
    fs::write(env.workdir.join("build/out.txt"), "x").unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let decision = pending_decision(&env, Duration::from_secs(10));

    assert_eq!(decision["kind"], "command_guard");
    assert_eq!(decision["title"], format!("rm @ {}", work_leaf(&env)));
    let options: Vec<&str> = decision["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["approve", "approve_day", "reject"]);
    let labels: Vec<&str> = decision["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["承認", "1日許可", "却下"]);
    assert_eq!(decision["allow_text"], true);
    assert_eq!(decision["origin"]["pane_id"], "p_e2e");
    assert_eq!(decision["origin"]["agent"], "codex");
    assert_eq!(decision["origin"]["command"], "rm -rf ./build");
    assert_eq!(decision["origin"]["reason"], REQUEST_REASON);
    let body = decision["body"].as_str().unwrap();
    assert!(body.contains(REQUEST_REASON), "{body}");
    assert!(body.contains("コマンド: rm -rf ./build"), "{body}");
    assert!(body.contains("エージェント: codex"), "{body}");

    let response = answer(
        &env,
        decision["decision_id"].as_str().unwrap(),
        "approve",
        None,
    );
    assert!(response.get("error").is_none(), "{response}");
    let output = wait_child(&mut child, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !env.workdir.join("build").exists(),
        "build dir still present"
    );
    assert!(stderr_text(&output).contains("✅ 承認: rm -rf ./build"));

    cleanup(env, server);
}

#[test]
fn enforce_ask_rejection_keeps_the_command_from_running() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let decision = pending_decision(&env, Duration::from_secs(10));
    answer(
        &env,
        decision["decision_id"].as_str().unwrap(),
        "reject",
        Some("まだ消さないで"),
    );

    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(env.workdir.join("build").exists(), "build dir was removed");
    assert!(
        stderr.contains("❌ Command cancelled: rm -rf ./build"),
        "{stderr}"
    );
    assert!(stderr.contains("まだ消さないで"), "{stderr}");

    cleanup(env, server);
}

#[test]
fn enforce_ask_day_approval_suppresses_the_second_prompt() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let decision = pending_decision(&env, Duration::from_secs(10));
    answer(
        &env,
        decision["decision_id"].as_str().unwrap(),
        "approve_day",
        None,
    );
    let output = wait_child(&mut child, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0));
    assert!(!env.workdir.join("build").exists());

    // The ai_guard day key was written to the approval file.
    let approvals = fs::read_to_string(&env.approval_file).unwrap();
    let day_key = format!(
        "day:{}",
        env.workdir.canonicalize().unwrap().to_string_lossy()
    );
    assert!(approvals.contains(&day_key), "{approvals}");

    // An equivalent command in the same cwd runs without a new decision.
    fs::create_dir_all(env.workdir.join("build")).unwrap();
    let mut second = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let output = wait_child(&mut second, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0));
    assert!(!env.workdir.join("build").exists());
    assert_eq!(decisions(&env, None).len(), 1, "second run prompted again");

    let records = read_log_records(&env);
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["verdict"], "allow");
    assert!(
        records[1]["suppressed_by"]
            .as_str()
            .unwrap_or("")
            .starts_with("day:"),
        "{}",
        records[1]
    );

    cleanup(env, server);
}

#[test]
fn enforce_ask_expiry_refuses_the_command() {
    let _lock = test_lock();
    let (env, server) = setup(None, "ask_timeout_seconds = 2\n", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let decision = pending_decision(&env, Duration::from_secs(10));
    assert_eq!(
        decision["expires_unix_ms"].as_u64().unwrap()
            - decision["created_unix_ms"].as_u64().unwrap(),
        2000
    );

    let output = wait_child(&mut child, Duration::from_secs(30));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(env.workdir.join("build").exists(), "build dir was removed");
    assert!(stderr.contains("期限切れ"), "{stderr}");
    assert!(stderr.contains("Command cancelled"), "{stderr}");
    assert_eq!(
        decisions(&env, Some("expired")).len(),
        1,
        "decision did not expire"
    );

    cleanup(env, server);
}

#[test]
fn enforce_ask_cancellation_refuses_the_command() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], Some(REQUEST_REASON));
    let decision = pending_decision(&env, Duration::from_secs(10));
    send_request(
        &env.socket_path,
        &format!(
            "{{\"id\":\"cancel\",\"method\":\"decision.cancel\",\"params\":{{\"decision_id\":\"{}\"}}}}",
            decision["decision_id"].as_str().unwrap()
        ),
    );

    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(env.workdir.join("build").exists(), "build dir was removed");
    assert!(stderr.contains("取り消"), "{stderr}");

    cleanup(env, server);
}

#[test]
fn enforce_ask_without_request_reason_fails_before_creating_a_decision() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], None);
    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(env.workdir.join("build").exists(), "build dir was removed");
    assert!(stderr.contains("申請文が必要"), "{stderr}");
    assert!(
        stderr.contains("AI_GUARD_REQUEST_REASON='対象プロジェクト"),
        "{stderr}"
    );
    assert!(decisions(&env, None).is_empty(), "decision was created");

    cleanup(env, server);
}

#[test]
fn enforce_ask_non_japanese_request_reason_is_rejected() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", true);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(
        &env,
        &["rm", "-rf", "./build"],
        Some("rebuild the build directory"),
    );
    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(env.workdir.join("build").exists());
    assert!(stderr.contains("日本語で書いてください"), "{stderr}");
    assert!(decisions(&env, None).is_empty(), "decision was created");

    cleanup(env, server);
}

#[test]
fn record_mode_keeps_executing_blocked_commands() {
    let _lock = test_lock();
    let (env, server) = setup(Some("record"), "", true);

    let mut child = guard_exec(&env, &["git", "push", "origin", "main"], None);
    let output = wait_child(&mut child, Duration::from_secs(10));
    let stderr = stderr_text(&output);
    // The real git ran (the workdir is not a repo, so it failed itself);
    // the guard neither blocked nor created a decision.
    assert!(!stderr.contains("ブロック"), "{stderr}");
    assert!(
        decisions(&env, None).is_empty(),
        "record mode created a decision"
    );
    let records = read_log_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["verdict"], "block");

    cleanup(env, server);
}

#[test]
fn unmarked_pane_executes_without_judging() {
    let _lock = test_lock();
    let (env, server) = setup(None, "", false);
    fs::create_dir_all(env.workdir.join("build")).unwrap();

    let mut child = guard_exec(&env, &["rm", "-rf", "./build"], None);
    let output = wait_child(&mut child, Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(0));
    assert!(!env.workdir.join("build").exists());
    assert!(decisions(&env, None).is_empty());
    assert!(
        read_log_records(&env).is_empty(),
        "unmarked pane was judged"
    );

    cleanup(env, server);
}

fn write_agent_marker(marker_path: &Path, workdir: &Path) {
    fs::create_dir_all(marker_path.parent().unwrap()).unwrap();
    fs::write(
        marker_path,
        serde_json::json!({
            "agent": "codex",
            "cwd": workdir.to_string_lossy(),
            "public_pane_id": "p_e2e",
            "updated_epoch": 1
        })
        .to_string(),
    )
    .unwrap();
}

/// `workdir` leaf plus its parent, matching the `parent/leaf` title suffix.
fn work_leaf(env: &GuardEnv) -> String {
    let leaf = env.workdir.file_name().unwrap().to_string_lossy();
    let parent = env
        .workdir
        .parent()
        .and_then(|p| p.file_name())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    format!("{parent}/{leaf}")
}
