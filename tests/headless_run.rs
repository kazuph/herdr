#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Headless {
    root: PathBuf,
    server: Child,
}
impl Headless {
    fn command(root: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
        command
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("HERDR_SOCKET_PATH", root.join("api.sock"))
            .env("HERDR_CLIENT_SOCKET_PATH", root.join("client.sock"))
            .env("HERDR_SOCKET_PATH_EXPLICIT", "1")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_SESSION")
            .env("SHELL", "/bin/sh");
        command
    }
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hr-headless-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let log = fs::File::create(root.join("server.log")).unwrap();
        let server = Self::command(&root)
            .arg("server")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let this = Self { root, server };
        let until = Instant::now() + Duration::from_secs(5);
        while !this.root.join("api.sock").exists() {
            assert!(Instant::now() < until, "server did not start");
            std::thread::sleep(Duration::from_millis(25));
        }
        this
    }
    fn run(&self, args: &[&str]) -> Output {
        Self::command(&self.root).args(args).output().unwrap()
    }
}
impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn silent_background_run_starts_without_any_caller_and_has_no_recipient() {
    let h = Headless::new();
    let output = h.run(&[
        "run",
        "--completion",
        "none",
        "--",
        "sh",
        "-c",
        "printf headless-ok",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let job = started["job"].as_str().unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let status = h.run(&["job", "status", job]);
        let record: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(record["caller_pane"], "");
        assert_eq!(record["caller_agent"], "");
        assert_eq!(record["completion"], "none");
        if record["status"] == "exited" {
            assert_eq!(record["exit_code"], 0);
            break;
        }
        assert!(Instant::now() < until, "job did not finish: {record}");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(String::from_utf8_lossy(&h.run(&["log", job]).stdout).contains("headless-ok"));
}

#[test]
fn notifying_or_pane_runs_still_fail_without_a_caller() {
    let h = Headless::new();
    for options in [
        vec![],
        vec!["--completion", "summary"],
        vec!["--completion", "full"],
        vec!["--pane"],
        vec!["--pane", "--completion", "none"],
    ] {
        let mut args = vec!["run"];
        args.extend(options);
        args.extend(["--", "echo", "must-not-run"]);
        let output = h.run(&args);
        assert!(!output.status.success(), "unexpected success: {args:?}");
    }
    let jobs: serde_json::Value = serde_json::from_slice(&h.run(&["run", "list"]).stdout).unwrap();
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0);
}
