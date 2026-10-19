//! Real server + transport relay: replies are held/dropped, never fabricated.
use super::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const DRIVER: &str = "cli::tb::tests::recovery::tb_recovery_subprocess_driver";
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn tb_recovery_subprocess_driver() {
    let Some(root) = std::env::var_os("HERDR_TB_RECOVERY_ROOT") else {
        return;
    };
    let bin = PathBuf::from(std::env::var_os("HERDR_TB_RECOVERY_BIN").unwrap());
    let result = activate(
        Path::new(&root),
        &bin,
        "0.7.2",
        super::super::super::plugin::link_for_tb,
    );
    if let Err(error) = &result {
        fs::write(Path::new(&root).join("driver-error"), error.to_string()).unwrap();
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

struct OwnedProcess(Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn rpc(socket: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    writeln!(
        stream,
        "{}",
        json!({"id":"tb-recovery-test", "method":method, "params":params})
    )
    .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    let value: Value = serde_json::from_str(&response).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value
}

struct Harness {
    s: Sandbox,
    _environment: PluginEnvironment,
    server: OwnedProcess,
    executable: PathBuf,
    socket: PathBuf,
    old: crate::api::schema::InstalledPluginInfo,
    browser: OwnedProcess,
}
impl Harness {
    fn new() -> Self {
        let s = Sandbox::new();
        let mut environment = PluginEnvironment::new(&s);
        let socket = s.0.join("real.sock");
        for (key, path) in [
            ("HERDR_SOCKET_PATH", socket.clone()),
            ("HERDR_CLIENT_SOCKET_PATH", s.0.join("client.sock")),
            ("HERDR_CONFIG_PATH", s.0.join("config.toml")),
            ("XDG_RUNTIME_DIR", s.0.join("runtime")),
        ] {
            environment.old.push((key, std::env::var_os(key)));
            std::env::set_var(key, path);
        }
        fs::create_dir_all(s.0.join("runtime")).unwrap();
        fs::write(s.0.join("config.toml"), "onboarding = false\n").unwrap();
        let first = s.0.join("one.tgz");
        fixture(&first, "0.7.1");
        local_install(&s, &first, "0.7.1", register_offline).unwrap();
        let second = s.0.join("two.tgz");
        fixture(&second, "0.7.2");
        let expanded = s.0.join("new-payload");
        unpack(&second, &expanded).unwrap();
        fs::rename(expanded.join("terminal-browser"), s.root().join("0.7.2")).unwrap();
        let old = crate::persist::plugin_registry::try_load()
            .unwrap()
            .into_iter()
            .find(|p| p.plugin_id == PLUGIN)
            .unwrap();
        fs::create_dir(s.0.join("dev")).unwrap();
        fs::write(
            s.0.join("dev/herdr-plugin.toml"),
            fs::read_to_string(s.root().join("0.7.1/herdr-plugin/herdr-plugin.toml"))
                .unwrap()
                .replace("0.7.1", "0.7.3"),
        )
        .unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("herdr");
        assert!(
            executable.is_file(),
            "real Herdr must be built by cargo nextest"
        );
        let server = OwnedProcess(
            Command::new(&executable)
                .arg("server")
                .env("SHELL", "/bin/sh")
                .env_remove("HERDR_ENV")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(s.0.join("server.log")).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + WAIT;
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "real server startup: {}",
                fs::read_to_string(s.0.join("server.log")).unwrap()
            );
            thread::sleep(Duration::from_millis(25));
        }
        rpc(&socket, "ping", json!({}));
        let mut browser = OwnedProcess(
            Command::new(s.bin().join("tb"))
                .arg("--hold")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        BufReader::new(browser.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        Self {
            s,
            _environment: environment,
            server,
            executable,
            socket,
            old,
            browser,
        }
    }
    fn entry(&self) -> Option<crate::api::schema::InstalledPluginInfo> {
        crate::persist::plugin_registry::try_load()
            .unwrap()
            .into_iter()
            .find(|p| p.plugin_id == PLUGIN)
    }
    fn driver(&self, proxy: &Path) -> OwnedProcess {
        OwnedProcess(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", DRIVER, "--nocapture"])
                .env("HERDR_SOCKET_PATH", proxy)
                .env("HERDR_TB_RECOVERY_ROOT", self.s.root())
                .env("HERDR_TB_RECOVERY_BIN", self.s.bin())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(self.s.0.join("driver.log")).unwrap())
                .spawn()
                .unwrap(),
        )
    }
    fn aligned(&self, version: &str) {
        assert_eq!(
            fs::read_to_string(self.s.root().join("current-version")).unwrap(),
            version
        );
        assert_eq!(launch(&self.s), format!("{version}\n--version\n"));
        assert_eq!(
            self.entry().unwrap().manifest_path,
            self.s
                .root()
                .join(version)
                .join("herdr-plugin/herdr-plugin.toml")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
        );
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        // This command is addressed to the test-owned socket and sandbox HOME,
        // never the user's installed server. OwnedProcess handles failed tests.
        // Resume only this owned server if an assertion interrupted SIGSTOP.
        let _ = unsafe { libc::kill(self.server.0.id() as i32, libc::SIGCONT) };
        let _ = Command::new(&self.executable)
            .args(["server", "stop"])
            .env("HERDR_SOCKET_PATH", &self.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.server.0.wait();
        if let Some(mut stdin) = self.browser.0.stdin.take() {
            let _ = stdin.write_all(b"done\n");
        }
        let _ = self.browser.0.wait();
    }
}

#[derive(Clone, Copy)]
enum Cut {
    BeforeLink,
    AfterPersist,
    Preflight,
}
struct Relay {
    path: PathBuf,
    ready: Receiver<()>,
    release: Option<Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Relay {
    fn new(h: &Harness, cut: Cut) -> Self {
        let path = h.s.0.join("relay.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let target = h.socket.clone();
        let (ready_tx, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let worker = thread::spawn(move || {
            for client in listener.incoming() {
                let mut client = client.unwrap();
                client.set_read_timeout(Some(WAIT)).unwrap();
                let mut request = String::new();
                BufReader::new(client.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                if request.is_empty() {
                    return;
                }
                let value: Value = serde_json::from_str(&request).unwrap();
                let is_link = value["method"] == "plugin.link";
                let stop_before = is_link && matches!(cut, Cut::BeforeLink);
                if stop_before {
                    ready_tx.send(()).unwrap();
                    if resume.recv().is_err() {
                        return;
                    }
                }
                let mut server = UnixStream::connect(&target).unwrap();
                server.set_read_timeout(Some(WAIT)).unwrap();
                server.write_all(request.as_bytes()).unwrap();
                let mut response = String::new();
                BufReader::new(server).read_line(&mut response).unwrap();
                assert!(!response.is_empty(), "real server response required");
                if (is_link && matches!(cut, Cut::AfterPersist))
                    || (!is_link && matches!(cut, Cut::Preflight))
                {
                    ready_tx.send(()).unwrap();
                    let _ = resume.recv();
                    let _ = client.shutdown(Shutdown::Both);
                    return;
                }
                client.write_all(response.as_bytes()).unwrap();
                if is_link {
                    return;
                }
            }
        });
        Self {
            path,
            ready,
            release: Some(release),
            worker: Some(worker),
        }
    }
    fn ready(&self) {
        self.ready
            .recv_timeout(WAIT)
            .expect("real request reached relay barrier");
    }
    fn resume(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        drop(self.release.take());
        let _ = UnixStream::connect(&self.path);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn finished(child: &mut OwnedProcess) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "test CLI did not finish");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn tb_sigint_and_sigterm_defer_until_real_registration_boundary() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut h = Harness::new();
        let mut relay = Relay::new(&h, Cut::BeforeLink);
        let mut driver = h.driver(&relay.path);
        relay.ready();
        assert_eq!(
            fs::read_to_string(h.s.root().join("current-version")).unwrap(),
            "0.7.2"
        );
        assert_eq!(
            h.entry(),
            Some(h.old.clone()),
            "link has not reached the real server yet"
        );
        // Stop the actual sandbox server, then let the real link request reach
        // its socket. No response is fabricated or returned while it is stopped.
        eprintln!(
            "owned server={} SIGSTOP; installer={} signal={signal}; old tb={} must remain alive",
            h.server.0.id(),
            driver.0.id(),
            h.browser.0.id()
        );
        assert_eq!(
            unsafe { libc::kill(h.server.0.id() as i32, libc::SIGSTOP) },
            0
        );
        relay.resume();
        assert_eq!(unsafe { libc::kill(driver.0.id() as i32, signal) }, 0);
        thread::sleep(Duration::from_millis(100));
        assert!(
            driver.0.try_wait().unwrap().is_none(),
            "termination must be deferred"
        );
        assert_eq!(h.entry(), Some(h.old.clone()));
        assert!(h.browser.0.try_wait().unwrap().is_none());
        assert_eq!(
            unsafe { libc::kill(h.server.0.id() as i32, libc::SIGCONT) },
            0
        );
        assert_eq!(finished(&mut driver).signal(), Some(signal));
        h.aligned("0.7.2");
        eprintln!(
            "signal={signal} replayed after current-version/wrapper/real registry aligned to 0.7.2"
        );
        assert!(h.browser.0.try_wait().unwrap().is_none());
    }
}

#[test]
fn tb_link_for_tb_restores_real_registration_after_saved_reply_is_lost() {
    let mut h = Harness::new();
    let mut relay = Relay::new(&h, Cut::AfterPersist);
    let mut driver = h.driver(&relay.path);
    relay.ready();
    assert_ne!(
        h.entry(),
        Some(h.old.clone()),
        "real server persisted the new registration"
    );
    eprintln!(
        "real server persisted {:?}; dropping only its reply",
        h.entry().unwrap().source.installed_unix_ms
    );
    relay.resume();
    assert_eq!(finished(&mut driver).code(), Some(1));
    assert_eq!(
        h.entry(),
        Some(h.old.clone()),
        "production link_for_tb must perform rollback"
    );
    h.aligned("0.7.1");
    assert!(h.browser.0.try_wait().unwrap().is_none());
}

#[test]
fn tb_real_registry_preserves_concurrent_link_disable_unlink_before_and_after_send() {
    for cut in [Cut::Preflight, Cut::AfterPersist] {
        for operation in ["link", "same-link", "disable", "unlink"] {
            let mut h = Harness::new();
            let mut relay = Relay::new(&h, cut);
            let mut driver = h.driver(&relay.path);
            relay.ready();
            match operation {
                "link" => {
                    rpc(&h.socket, "plugin.link", json!({"path":h.s.0.join("dev")}));
                }
                "same-link" => {
                    rpc(
                        &h.socket,
                        "plugin.link",
                        json!({"path":h.s.root().join("0.7.2/herdr-plugin")}),
                    );
                }
                "disable" => {
                    rpc(&h.socket, "plugin.disable", json!({"plugin_id":PLUGIN}));
                }
                "unlink" => {
                    rpc(&h.socket, "plugin.unlink", json!({"plugin_id":PLUGIN}));
                }
                _ => unreachable!(),
            }
            let concurrent = h.entry();
            relay.resume();
            assert_eq!(finished(&mut driver).code(), Some(1));
            assert_eq!(
                h.entry(),
                concurrent,
                "concurrent {operation} must be preserved"
            );
            let error = fs::read_to_string(h.s.root().join("driver-error")).unwrap();
            assert!(
                error.contains("registration conflict"),
                "{operation}: {error}"
            );
            eprintln!("concurrent {operation} preserved; explicit error: {error}");
            assert_eq!(launch(&h.s), "0.7.1\n--version\n");
            assert!(h.browser.0.try_wait().unwrap().is_none());
        }
    }
}
