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
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("HERDR_GUARD_")) {
            command.env_remove(key);
        }
        let path = std::env::var("PATH").unwrap_or_default();
        command.env(
            "PATH",
            path.split(':')
                .filter(|p| !p.contains("/guard/bin"))
                .collect::<Vec<_>>()
                .join(":"),
        );
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

#[test]
fn explicit_missing_caller_never_creates_a_silent_job() {
    let h = Headless::new();
    let output = h.run(&[
        "run",
        "--caller",
        "missing-pane",
        "--completion",
        "none",
        "--",
        "echo",
        "must-not-run",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unable to resolve caller pane"));
    let jobs: serde_json::Value = serde_json::from_slice(&h.run(&["run", "list"]).stdout).unwrap();
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0);
}

impl Headless {
    fn api(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::io::{BufRead, BufReader, Write};
        let mut stream =
            std::os::unix::net::UnixStream::connect(self.root.join("api.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"id":"guard-test", "method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn shell(&self, pane: &str, command: &str) {
        let response = self.api(
            "pane.send_input",
            serde_json::json!({"pane_id":pane,"text":command,"keys":["Enter"]}),
        );
        assert!(response.get("result").is_some(), "{response}");
    }

    fn wait_file(&self, name: &str) -> String {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = fs::read_to_string(self.root.join(name)) {
                if !text.is_empty() {
                    return text;
                }
            }
            assert!(
                Instant::now() < until,
                "missing {name}; log: {}",
                fs::read_to_string(self.root.join("server.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn pane_client(
        &self,
        pane: &str,
        name: &str,
        request: &serde_json::Value,
    ) -> serde_json::Value {
        fs::write(self.root.join(format!("{name}.json")), request.to_string()).unwrap();
        self.shell(
            pane,
            &format!(
                "python3 '{}/client.py' '{}/api.sock' '{}/{}.json' '{}/{}.out'",
                self.root.display(),
                self.root.display(),
                self.root.display(),
                name,
                self.root.display(),
                name
            ),
        );
        serde_json::from_str(&self.wait_file(&format!("{name}.out"))).unwrap()
    }
}

#[test]
fn force_uses_socket_peer_ancestry_rejects_real_other_clients_and_keeps_valid_cli_retry() {
    use serde_json::json;
    let h = Headless::new();
    let mut panes = Vec::new();
    for label in ["caller-a", "caller-b", "target"] {
        let created = h.api("workspace.create", json!({"label":label,"cwd":h.root}));
        panes.push(
            created["result"]["root_pane"]["pane_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let (a, b, target) = (&panes[0], &panes[1], &panes[2]);
    fs::write(
        h.root.join("client.py"),
        r#"import json,socket,sys
with socket.socket(socket.AF_UNIX) as s:
 s.connect(sys.argv[1]); s.sendall(open(sys.argv[2],'rb').read()+b'\n')
 result=s.makefile('rb').readline()
 open(sys.argv[3],'wb').write(result)
"#,
    )
    .unwrap();
    fs::write(
        h.root.join("question.txt"),
        include_str!("../src/detect/fixtures/claude-workspace-trust-live.txt"),
    )
    .unwrap();
    fs::write(
        h.root.join("target.py"),
        r#"import os,sys,tty
from pathlib import Path
root=Path(sys.argv[1]); tty.setraw(0)
os.write(1,b'\x1b[2J\x1b[H'+(root/'question.txt').read_bytes().replace(b'\n',b'\r\n'))
(root/'ready').write_text('ready')
with open(root/'input','ab',buffering=0) as sink:
 while True:
  data=os.read(0,4096)
  if not data: break
  sink.write(data)
"#,
    )
    .unwrap();
    h.shell(
        target,
        &format!(
            "python3 '{}/target.py' '{}'",
            h.root.display(),
            h.root.display()
        ),
    );
    h.wait_file("ready");
    let reported = h.api(
        "pane.report_agent",
        json!({"pane_id":target,"source":"test","agent":"claude","state":"working"}),
    );
    assert!(reported.get("result").is_some(), "{reported}");
    let request = |force, caller: &str| json!({"id":"force","method":"pane.send_keys","params":{"pane_id":target,"keys":["Enter"],"force":force,"caller_pane_id":caller}});
    let rejected = h.pane_client(a, "a-rejected", &request(false, a));
    assert_eq!(rejected["error"]["code"], "pane_confirmation_pending");
    // B has its own real shell and socket connection, but claims A's identity.
    let stolen = h.pane_client(b, "b-spoof", &request(true, a));
    assert_eq!(stolen["error"]["code"], "pane_confirmation_pending");
    for (index, (method, mut params)) in [
        ("pane.send_text", json!({"pane_id":target,"text":"danger"})),
        (
            "pane.send_input",
            json!({"pane_id":target,"text":"danger","keys":["Enter"]}),
        ),
        ("agent.send", json!({"target":target,"text":"danger"})),
        ("agent.prompt", json!({"target":target,"text":"danger"})),
    ]
    .into_iter()
    .enumerate()
    {
        params["force"] = json!(true);
        params["caller_pane_id"] = json!(a);
        let stolen = h.pane_client(
            b,
            &format!("b-route-{index}"),
            &json!({"id":"spoof-route","method":method,"params":params}),
        );
        assert_eq!(
            stolen["error"]["code"], "pane_confirmation_pending",
            "{stolen}"
        );
        assert_eq!(
            h.api(method, params)["error"]["code"],
            "pane_confirmation_pending"
        );
    }
    let params = request(true, a)["params"].clone();
    let outside = h.api("pane.send_keys", params.clone());
    assert_eq!(outside["error"]["code"], "pane_confirmation_pending");
    // JSON cannot inject transport-owned peer metadata, either.
    let mut forged = params;
    forged["input_peer_pid"] = json!(std::process::id());
    assert_eq!(
        h.api("pane.send_keys", forged)["error"]["code"],
        "pane_confirmation_pending"
    );
    // The real target records every input byte, including any illicit forced input.
    assert_eq!(fs::read(h.root.join("input")).unwrap_or_default(), b"");
    h.shell(a, &format!("HERDR_PANE_ID={} '{}' pane send-keys -f {} Enter >'{}/a-valid.out' 2>'{}/a-valid.err'; echo $? >'{}/a-valid.status'", a, env!("CARGO_BIN_EXE_herdr"), target, h.root.display(), h.root.display(), h.root.display()));
    assert_eq!(
        h.wait_file("a-valid.status").trim(),
        "0",
        "{}",
        fs::read_to_string(h.root.join("a-valid.err")).unwrap_or_default()
    );
    assert_eq!(h.wait_file("input").as_bytes(), b"\r");
    // B's verified identity also cannot borrow A's rejection record.
    let own_b = h.pane_client(b, "b-own-unseen", &request(true, b));
    assert_eq!(own_b["error"]["code"], "pane_confirmation_pending");
    assert_eq!(fs::read(h.root.join("input")).unwrap(), b"\r");
}
