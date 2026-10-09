//! Saved endpoints attach to an existing session socket; they never launch Herdr.
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{RemoteHerdr, RemotePlatform};

#[path = "saved/process.rs"]
mod process;

/// tailssh holds a `check` connection open for up to 30 minutes awaiting
/// approval; matching it keeps one ssh process alive instead of throwing away
/// a fresh authentication URL on every reconnect attempt.
const SSH_AUTH_WAIT: Duration = Duration::from_secs(30 * 60);
/// The browser is opened at most once per machine per this window: every new
/// ssh attempt mints a fresh URL, and without a cap overnight reconnects would
/// pile up tabs when approval never comes.
const AUTH_URL_BROWSER_OPEN_INTERVAL: Duration = Duration::from_secs(30 * 60);

static AUTH_URL_BROWSER_OPENED_AT: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

fn timeout() -> Duration {
    #[cfg(test)]
    if let Some(timeout) = test_hooks::connect_timeout() {
        return timeout;
    }
    Duration::from_secs(crate::machine::SSH_CONNECT_TIMEOUT_SECS)
}

fn ssh_program() -> OsString {
    #[cfg(test)]
    if let Some(program) = test_hooks::ssh_program() {
        return program;
    }
    OsString::from("ssh")
}

fn connect_cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "saved SSH connection attempt was cancelled",
    )
}

fn ssh_command(target: &str) -> Command {
    let mut command = Command::new(ssh_program());
    command.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "UpdateHostKeys=no",
        "-o",
        "ControlMaster=no",
        "-S",
        "none",
        "-o",
        "PermitLocalCommand=no",
    ]);
    command.arg("-o").arg(format!(
        "ConnectTimeout={}",
        crate::machine::SSH_CONNECT_TIMEOUT_SECS
    ));
    command.arg("--").arg(target);
    command
}

fn open_auth_url(url: &str) -> io::Result<()> {
    #[cfg(test)]
    if let Some(hook) = test_hooks::open_url_hook() {
        return hook(url);
    }
    crate::platform::open_url(url)
}

/// Surface a Tailscale SSH check URL: the machine's status line always carries
/// the newest URL so it can be opened from another terminal, while the local
/// browser is opened at most once per machine per AUTH_URL_BROWSER_OPEN_INTERVAL.
fn note_tailscale_auth_url(profile_id: &str, url: &str, status: &dyn Fn(&str)) {
    status(&format!("waiting for Tailscale SSH approval: {url}"));
    let should_open = {
        let mut opened_at = AUTH_URL_BROWSER_OPENED_AT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        let due = opened_at
            .iter()
            .find(|(id, _)| id == profile_id)
            .is_none_or(|(_, previous)| {
                now.duration_since(*previous) >= AUTH_URL_BROWSER_OPEN_INTERVAL
            });
        if due {
            if let Some(entry) = opened_at.iter_mut().find(|(id, _)| id == profile_id) {
                entry.1 = now;
            } else {
                opened_at.push((profile_id.to_owned(), now));
            }
        }
        due
    };
    if should_open {
        if let Err(error) = open_auth_url(url) {
            tracing::warn!(%error, %url, "failed to open the Tailscale SSH approval URL");
        }
    }
}

fn read_remote(
    target: &str,
    script: &str,
    hooks: &crate::remote::SavedSshHooks<'_>,
) -> io::Result<std::process::Output> {
    if hooks.cancel.load(Ordering::Acquire) {
        return Err(connect_cancelled());
    }
    let mut command = ssh_command(target);
    command
        .arg(format!("/bin/sh -c {}", super::shell_quote(script)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut on_auth_url = |url: &str| note_tailscale_auth_url(hooks.profile_id, url, hooks.status);
    process::wait_with_output_timeout(
        command.spawn()?,
        timeout(),
        &mut process::WaitHooks {
            cancel: hooks.cancel,
            auth_wait: auth_wait(),
            on_auth_url: &mut on_auth_url,
        },
    )
}

fn auth_wait() -> Duration {
    #[cfg(test)]
    if let Some(auth_wait) = test_hooks::auth_wait() {
        return auth_wait;
    }
    SSH_AUTH_WAIT
}

#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<RemoteSession>,
}

#[derive(Deserialize)]
struct RemoteSession {
    name: String,
    running: bool,
    socket_path: String,
}

fn existing_session_socket(bytes: &[u8], session: &str) -> io::Result<String> {
    let list: SessionList = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut matching = list
        .sessions
        .into_iter()
        .filter(|item| item.name == session);
    let item = matching.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the saved remote session is not ready; start its server separately",
        )
    })?;
    if matching.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote session identity is ambiguous",
        ));
    }
    if !item.running {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "the saved remote session is not running; start its server separately",
        ));
    }
    if !item.socket_path.starts_with('/') || item.socket_path.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote session socket is not an absolute Unix path",
        ));
    }
    // This only transforms a remote path string; it must never be opened on the Local machine.
    let path = crate::server::socket_paths::derive_client_socket_from_api_socket(Path::new(
        &item.socket_path,
    ));
    path.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote socket path is not UTF-8",
        )
    })
}

fn discover_socket(
    target: &str,
    session: &str,
    hooks: &crate::remote::SavedSshHooks<'_>,
) -> io::Result<String> {
    let output = read_remote(target, "uname -s\nuname -m\n", hooks)?;
    if !output.status.success() {
        return Err(super::command_failed(
            "remote platform detection failed",
            &output,
        ));
    }
    let platform_text = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = platform_text.lines();
    let platform = RemotePlatform::from_uname(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )
    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "unsupported remote platform"))?;
    let template = RemoteHerdr::for_platform(platform);
    let binary = crate::machine::remote_binary();
    let mut script = format!("command -v {} || true\n", super::shell_quote(&binary));
    if std::env::var(crate::machine::MACHINE_REMOTE_BINARY_ENV_VAR)
        .ok()
        .is_none_or(|value| value.is_empty())
    {
        script.push_str(&super::known_remote_binary_candidate_script(
            &template.platform,
        ));
    }
    let output = read_remote(target, &script, hooks)?;
    if !output.status.success() {
        return Err(super::command_failed(
            "remote installed binary discovery failed",
            &output,
        ));
    }
    let paths = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut candidates = Vec::new();
    for candidate in super::remote_herdrs_from_path_discovery(&template, &paths) {
        super::push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }
    let mut last_error = io::Error::new(
        io::ErrorKind::NotFound,
        "installed remote Herdr is not ready",
    );
    for candidate in candidates {
        // Explicit session lookup is read-only, independent of either binary's private wire version.
        let script = format!(
            "exec {} --session {} session list --json",
            candidate.shell_path,
            super::shell_quote(session)
        );
        let output = read_remote(target, &script, hooks)?;
        if !output.status.success() {
            last_error = super::command_failed("remote session discovery failed", &output);
            continue;
        }
        match existing_session_socket(&output.stdout, session) {
            Ok(path) => return Ok(path),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// OpenSSH expands ${ENV} before parsing forwarding fields. Escape the brace before that
/// pass and backslash/colon for parse_fwd_field, so paths stay literal on both passes.
fn forwarding_path(path: &str) -> String {
    let mut escaped = String::new();
    let mut previous = None;
    for ch in path.chars() {
        if matches!(ch, ':' | '\\') || (ch == '{' && previous == Some('$')) {
            escaped.push('\\');
        }
        escaped.push(ch);
        previous = Some(ch);
    }
    escaped
}

struct SavedSshBridge {
    child: Option<Child>,
    directory: Option<PathBuf>,
}

impl Drop for SavedSshBridge {
    fn drop(&mut self) {
        let child = self.child.take().map(|mut child| {
            let _ = child.kill();
            child
        });
        let directory = self.directory.take();
        // Transport cancellation cannot join a bridge worker on the client UI thread.
        std::thread::spawn(move || {
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            if let Some(directory) = directory {
                let _ = fs::remove_dir_all(directory);
            }
        });
    }
}

fn forward_stream(
    target: &str,
    remote_path: &str,
    hooks: &crate::remote::SavedSshHooks<'_>,
) -> io::Result<(crate::ipc::LocalStream, Box<dyn Send>)> {
    let directory = super::private_ssh_config_dir()?;
    let mut bridge = SavedSshBridge {
        child: None,
        directory: Some(directory.clone()),
    };
    let digest = format!("{:x}", Sha256::digest(hooks.profile_id.as_bytes()));
    // The private directory itself is unique. Keep the readable digest as long as the
    // existing portable Unix socket byte limit permits, without rewriting profile IDs.
    let mut prefix = digest.as_str();
    let path = loop {
        let path = directory.join(format!("{prefix}.s"));
        if super::fits_unix_socket_path(&path) {
            break path;
        }
        prefix = prefix
            .get(..prefix.len().saturating_sub(1))
            .filter(|prefix| !prefix.is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private bridge socket path is too long",
                )
            })?;
    };
    let path_text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bridge path is not UTF-8"))?;
    let stderr_path = directory.join("stderr");
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stderr_path)?;
    let mut command = ssh_command(target);
    // Move the destination behind forwarding options; no remote command or Herdr bootstrap runs.
    let mut args = command
        .get_args()
        .map(|arg| arg.to_os_string())
        .collect::<Vec<_>>();
    args.truncate(args.len().saturating_sub(2));
    command = Command::new(ssh_program());
    command
        .args(args)
        .arg("-N")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("StreamLocalBindMask=0177")
        .arg("-o")
        .arg("StreamLocalBindUnlink=no")
        .arg("-L")
        .arg(format!(
            "{}:{}",
            forwarding_path(path_text),
            forwarding_path(remote_path)
        ))
        .arg("--")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    bridge.child = Some(command.spawn()?);
    let mut deadline = Instant::now() + timeout();
    let mut auth_wait_started = false;
    loop {
        let child = bridge
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned SSH bridge process missing"))?;
        if let Some(status) = child.try_wait()? {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("saved SSH bridge exited {status}: {}", stderr.trim()),
            ));
        }
        if hooks.cancel.load(Ordering::Acquire) {
            return Err(connect_cancelled());
        }
        if !auth_wait_started {
            // The bridge's stderr lands in this file; watch it so a Tailscale
            // check banner starts the approval wait instead of the 15s timeout.
            if let Some(url) = fs::read(&stderr_path)
                .ok()
                .and_then(|bytes| process::tailscale_check_url(&bytes))
            {
                auth_wait_started = true;
                deadline = Instant::now() + auth_wait();
                note_tailscale_auth_url(hooks.profile_id, &url, hooks.status);
            }
        }
        match crate::ipc::connect_local_stream(&path) {
            Ok(stream) => return Ok((stream, Box::new(bridge))),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                if auth_wait_started {
                    "saved SSH socket forwarding timed out waiting for Tailscale SSH approval"
                } else {
                    "saved SSH socket forwarding timed out"
                },
            ));
        }
        std::thread::sleep(
            super::BRIDGE_ACCEPT_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(crate) fn connect_saved_ssh(
    target: &str,
    session: &str,
    hooks: &crate::remote::SavedSshHooks<'_>,
) -> io::Result<(crate::ipc::LocalStream, Box<dyn Send>)> {
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if target.is_empty() || target.starts_with('-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid saved SSH target",
        ));
    }
    let remote_path = discover_socket(target, session, hooks)?;
    forward_stream(target, &remote_path, hooks)
}

#[cfg(test)]
pub(crate) mod test_hooks {
    //! Test-only seams so unit tests can run a fake `ssh` and capture browser
    //! opens instead of launching real ones. Nothing here exists in normal
    //! builds; no user-facing setting is added.
    use super::*;
    use std::sync::Arc;

    pub(crate) type OpenUrlHook = Arc<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

    static SSH_PROGRAM: Mutex<Option<OsString>> = Mutex::new(None);
    static CONNECT_TIMEOUT: Mutex<Option<Duration>> = Mutex::new(None);
    static AUTH_WAIT: Mutex<Option<Duration>> = Mutex::new(None);
    static OPEN_URL: Mutex<Option<OpenUrlHook>> = Mutex::new(None);
    /// Serializes tests that share these process-wide seams.
    static LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn take<T>(slot: &Mutex<Option<T>>) -> Option<T>
    where
        T: Clone,
    {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn put<T>(slot: &Mutex<Option<T>>, value: Option<T>) {
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
    }

    pub(crate) fn set_ssh_program(program: Option<OsString>) {
        put(&SSH_PROGRAM, program);
    }

    pub(crate) fn ssh_program() -> Option<OsString> {
        take(&SSH_PROGRAM)
    }

    pub(crate) fn set_connect_timeout(timeout: Option<Duration>) {
        put(&CONNECT_TIMEOUT, timeout);
    }

    pub(crate) fn connect_timeout() -> Option<Duration> {
        take(&CONNECT_TIMEOUT)
    }

    pub(crate) fn set_auth_wait(auth_wait: Option<Duration>) {
        put(&AUTH_WAIT, auth_wait);
    }

    pub(crate) fn auth_wait() -> Option<Duration> {
        take(&AUTH_WAIT)
    }

    pub(crate) fn set_open_url_hook(hook: Option<OpenUrlHook>) {
        put(&OPEN_URL, hook);
    }

    pub(crate) fn open_url_hook() -> Option<OpenUrlHook> {
        take(&OPEN_URL)
    }

    pub(crate) fn clear_auth_url_opens() {
        AUTH_URL_BROWSER_OPENED_AT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Pretend the machine's last browser open happened `age` ago so the
    /// 30-minute reopen interval can be exercised without sleeping.
    pub(crate) fn age_auth_url_open(profile_id: &str, age: Duration) {
        let mut opened_at = AUTH_URL_BROWSER_OPENED_AT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, previous)) = opened_at.iter_mut().find(|(id, _)| id == profile_id) {
            *previous = Instant::now() - age;
        }
    }
}

#[cfg(test)]
pub(crate) mod test_fakes {
    //! A fake `ssh` program (Python) plus a per-test temp directory harness so
    //! saved-machine connect paths run end to end without a real SSH server.
    //! The fake reproduces the Tailscale `check` banner: it writes the
    //! `To authenticate, visit:` URL to stderr and blocks until the approval
    //! marker file appears, just like tailssh waits for the check to resolve.
    use super::test_hooks;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const FAKE_SSH: &str = r##"#!/usr/bin/env python3
import os
import socket
import sys
import threading
import time

def env(name):
    return os.environ.get(name, "")

args = sys.argv[1:]
try:
    split = args.index("--")
    target = args[split + 1]
    command = args[split + 2] if len(args) > split + 2 else ""
except (ValueError, IndexError):
    target = ""
    command = ""

pid_file = env("FAKE_SSH_PID_FILE")
if pid_file:
    with open(pid_file, "a") as handle:
        handle.write("%d\n" % os.getpid())

if env("FAKE_SSH_FAIL_TARGET") and env("FAKE_SSH_FAIL_TARGET") == target:
    sys.stderr.write("ssh: connect to host %s port 22: refused\n" % target)
    sys.exit(255)

banner = env("FAKE_SSH_BANNER_URL")
gate = env("FAKE_SSH_APPROVE_FILE")
if banner and not (gate and os.path.exists(gate)):
    sys.stderr.write("# Tailscale SSH requires an additional check.\n")
    sys.stderr.write("# To authenticate, visit: %s\n" % banner)
    sys.stderr.flush()
    if gate:
        deadline = time.time() + 120
        while not os.path.exists(gate):
            if time.time() > deadline:
                sys.stderr.write("check approval timed out\n")
                sys.exit(1)
            time.sleep(0.05)
        sys.stderr.write("# Authentication checked with Tailscale SSH.\n")
        sys.stderr.flush()

if "-N" in args:
    spec = args[args.index("-L") + 1]
    local = spec.split(":", 1)[0]
    local = local.replace("\\:", ":").replace("\\{", "{").replace("\\\\", "\\")
    try:
        os.unlink(local)
    except OSError:
        pass
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(local)
    server.listen(4)

    def accept():
        while True:
            try:
                connection, _ = server.accept()
            except OSError:
                return
            connection.close()

    threading.Thread(target=accept, daemon=True).start()
    while True:
        time.sleep(1)

remote_socket = env("FAKE_SSH_SESSION_SOCKET") or "/tmp/fake-remote/herdr.sock"
session = env("FAKE_SSH_SESSION") or "saved"
if "session list" in command:
    sys.stdout.write(
        '{"sessions":[{"name":"%s","running":true,"socket_path":"%s"}]}\n'
        % (session, remote_socket)
    )
elif "uname -s" in command:
    sys.stdout.write("Linux\nx86_64\n")
elif "command -v" in command:
    sys.stdout.write("/home/test/.local/bin/herdr\n")
sys.exit(0)
"##;

    /// Installs the fake ssh plus its environment. The `test_hooks` lock is held
    /// for the whole test so the process-wide env vars stay consistent.
    pub(crate) struct FakeSsh {
        _lock: std::sync::MutexGuard<'static, ()>,
        directory: PathBuf,
        pid_file: PathBuf,
        approve_file: PathBuf,
    }

    impl FakeSsh {
        pub(crate) fn new(test_name: &str) -> Self {
            let lock = test_hooks::lock();
            let directory = std::env::temp_dir().join(format!(
                "herdr-saved-ssh-{test_name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            ));
            fs::create_dir_all(&directory).unwrap();
            let program = directory.join("ssh");
            fs::write(&program, FAKE_SSH).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut permissions = fs::metadata(&program).unwrap().permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&program, permissions).unwrap();
            }
            let pid_file = directory.join("pids");
            let approve_file = directory.join("approved");
            test_hooks::set_ssh_program(Some(program.into_os_string()));
            let pid = pid_file.clone();
            Self {
                _lock: lock,
                directory,
                pid_file,
                approve_file,
            }
            .with_env("FAKE_SSH_PID_FILE", &pid)
        }

        /// Point the approval gate at this test's marker file; the fake blocks
        /// until `approve()` writes it, exactly like a pending Tailscale check.
        pub(crate) fn with_approve_gate(self) -> Self {
            let gate = self.approve_file.clone();
            self.with_env("FAKE_SSH_APPROVE_FILE", &gate)
        }

        pub(crate) fn with_banner(self, url: &str) -> Self {
            self.with_env_text("FAKE_SSH_BANNER_URL", url)
        }

        pub(crate) fn with_env(self, name: &str, value: &Path) -> Self {
            std::env::set_var(name, value);
            self
        }

        pub(crate) fn with_env_text(self, name: &str, value: &str) -> Self {
            std::env::set_var(name, value);
            self
        }

        /// The check was approved in the browser: the fake proceeds and later
        /// connections skip the banner, like tailssh's cached check state.
        pub(crate) fn approve(&self) {
            fs::write(&self.approve_file, b"approved\n").unwrap();
        }

        pub(crate) fn pids(&self) -> Vec<i32> {
            fs::read_to_string(&self.pid_file)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect()
        }

        pub(crate) fn wait_for_pid(&self, timeout: Duration) -> Vec<i32> {
            let deadline = Instant::now() + timeout;
            loop {
                let pids = self.pids();
                if !pids.is_empty() || Instant::now() >= deadline {
                    return pids;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// Every fake ssh pid from this test must be gone; kill(2) signal 0 is
        /// a pure existence probe.
        pub(crate) fn all_dead(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            let pids = self.pids();
            while Instant::now() < deadline {
                if pids.iter().all(|pid| unsafe { libc::kill(*pid, 0) } != 0) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        }
    }

    impl Drop for FakeSsh {
        fn drop(&mut self) {
            test_hooks::set_ssh_program(None);
            test_hooks::set_connect_timeout(None);
            test_hooks::set_auth_wait(None);
            test_hooks::set_open_url_hook(None);
            test_hooks::clear_auth_url_opens();
            for name in [
                "FAKE_SSH_PID_FILE",
                "FAKE_SSH_APPROVE_FILE",
                "FAKE_SSH_BANNER_URL",
                "FAKE_SSH_FAIL_TARGET",
                "FAKE_SSH_SESSION_SOCKET",
                "FAKE_SSH_SESSION",
            ] {
                std::env::remove_var(name);
            }
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_fakes::FakeSsh;
    use super::test_hooks;
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn status_recorder() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str)) {
        let messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let messages = Arc::clone(&messages);
            move |message: &str| {
                messages
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(message.to_owned());
            }
        };
        (messages, sink)
    }

    fn open_recorder() -> Arc<Mutex<Vec<String>>> {
        let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        test_hooks::set_open_url_hook(Some(Arc::new({
            let opened = Arc::clone(&opened);
            move |url: &str| {
                opened
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(url.to_owned());
                Ok(())
            }
        })));
        opened
    }

    fn wait_for_status(messages: &Arc<Mutex<Vec<String>>>, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let seen = messages
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .any(|message| message.contains(needle));
            if seen {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "status containing {needle:?} never arrived; got {:?}",
                messages.lock().unwrap_or_else(|p| p.into_inner())
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn tailscale_check_url_only_accepts_complete_http_urls() {
        let url = process::tailscale_check_url(
            b"# Tailscale SSH requires an additional check.\n# To authenticate, visit: https://login.tailscale.com/a/abc123\n",
        );
        assert_eq!(url.as_deref(), Some("https://login.tailscale.com/a/abc123"));
        // A partial line may still be arriving; it must not open a truncated URL.
        assert!(
            process::tailscale_check_url(b"# To authenticate, visit: https://login.ta").is_none()
        );
        assert!(
            process::tailscale_check_url(b"To authenticate, visit: file:///etc/passwd\n").is_none()
        );
        assert!(process::tailscale_check_url(b"Permission denied (publickey).\n").is_none());
    }

    #[test]
    fn wait_with_output_timeout_grants_the_auth_wait_once_per_banner() {
        let cancel = AtomicBool::new(false);
        let urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut on_auth_url = {
            let urls = Arc::clone(&urls);
            move |url: &str| {
                urls.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(url.to_owned());
            }
        };
        // The banner arrives immediately; the process then stays blocked on
        // approval far past the normal connect timeout.
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("printf '%s\\n' '# To authenticate, visit: https://login.tailscale.com/a/xyz' >&2; exec sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let error = process::wait_with_output_timeout(
            child,
            Duration::from_millis(50),
            &mut process::WaitHooks {
                cancel: &cancel,
                auth_wait: Duration::from_millis(300),
                on_auth_url: &mut on_auth_url,
            },
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error
            .to_string()
            .contains("waiting for Tailscale SSH approval"));
        assert_eq!(
            urls.lock().unwrap_or_else(|p| p.into_inner()).as_slice(),
            ["https://login.tailscale.com/a/xyz"]
        );
        // The extended deadline, not the 50ms base timeout, ended the wait.
        assert!(
            elapsed >= Duration::from_millis(300),
            "auth wait ended after {elapsed:?}, before the extended deadline"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "auth wait ran far past its deadline: {elapsed:?}"
        );
    }

    #[test]
    fn saved_ssh_tailscale_check_waits_for_approval_and_completes_the_same_connection() {
        let fake = FakeSsh::new("approve")
            .with_banner("https://login.tailscale.com/a/testcode")
            .with_approve_gate();
        let opened = open_recorder();
        let cancel = AtomicBool::new(false);
        let (messages, sink) = status_recorder();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                connect_saved_ssh(
                    "fake-host",
                    "saved",
                    &crate::remote::SavedSshHooks {
                        profile_id: "machine-a",
                        cancel: &cancel,
                        status: &sink,
                    },
                )
            });
            // The machine's status now carries the full URL while the very same
            // ssh process keeps waiting for the check approval.
            wait_for_status(
                &messages,
                "https://login.tailscale.com/a/testcode",
                Duration::from_secs(10),
            );
            assert!(!worker.is_finished());
            fake.approve();
            let (stream, bridge) = worker
                .join()
                .unwrap_or_else(|_| panic!("connect worker panicked"))
                .unwrap_or_else(|error| panic!("approved connection should complete: {error}"));
            drop(stream);
            drop(bridge);
        });
        assert_eq!(
            opened.lock().unwrap_or_else(|p| p.into_inner()).as_slice(),
            ["https://login.tailscale.com/a/testcode"],
            "the approval URL is opened exactly once for the wait"
        );
        let messages = messages.lock().unwrap_or_else(|p| p.into_inner());
        assert!(messages
            .iter()
            .any(|message| message.contains("Tailscale SSH")
                && message.contains("https://login.tailscale.com/a/testcode")));
        assert!(fake.all_dead(Duration::from_secs(5)), "ssh child survived");
    }

    /// Run one connect attempt on a scoped worker; it parks inside the
    /// Tailscale approval wait (the gate file is never created) until `cancel`
    /// is set, then its ssh child is killed and the attempt returns.
    fn auth_wait_attempt<'scope, 'env>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        cancel: &'env AtomicBool,
        messages: &'env Arc<Mutex<Vec<String>>>,
    ) -> std::thread::ScopedJoinHandle<'scope, ()> {
        scope.spawn(move || {
            let sink = {
                let messages = Arc::clone(messages);
                move |message: &str| {
                    messages
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(message.to_owned());
                }
            };
            let _ = connect_saved_ssh(
                "fake-host",
                "saved",
                &crate::remote::SavedSshHooks {
                    profile_id: "machine-a",
                    cancel,
                    status: &sink,
                },
            );
        })
    }

    fn opened_count(opened: &Arc<Mutex<Vec<String>>>) -> usize {
        opened
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    #[test]
    fn saved_ssh_tailscale_check_reopens_browser_only_after_the_interval() {
        let _fake = FakeSsh::new("ratelimit")
            .with_banner("https://login.tailscale.com/a/second")
            .with_approve_gate();
        let opened = open_recorder();
        let cancel1 = AtomicBool::new(false);
        let messages1 = Arc::new(Mutex::new(Vec::new()));
        let cancel2 = AtomicBool::new(false);
        let messages2 = Arc::new(Mutex::new(Vec::new()));
        let cancel3 = AtomicBool::new(false);
        let messages3 = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            // Attempt 1: the banner URL opens the browser once.
            let worker1 = auth_wait_attempt(scope, &cancel1, &messages1);
            wait_for_status(
                &messages1,
                "https://login.tailscale.com/a/second",
                Duration::from_secs(10),
            );
            assert_eq!(opened_count(&opened), 1);
            cancel1.store(true, Ordering::Release);
            worker1.join().unwrap_or_else(|_| panic!("worker panicked"));

            // Attempt 2 inside the interval: the status still carries the newest
            // URL, but the browser is not opened again.
            let worker2 = auth_wait_attempt(scope, &cancel2, &messages2);
            wait_for_status(
                &messages2,
                "https://login.tailscale.com/a/second",
                Duration::from_secs(10),
            );
            assert_eq!(
                opened_count(&opened),
                1,
                "a retry inside 30 minutes must not open another tab"
            );
            cancel2.store(true, Ordering::Release);
            worker2.join().unwrap_or_else(|_| panic!("worker panicked"));

            // Once the interval passed, a fresh auth wait opens the browser again.
            test_hooks::age_auth_url_open(
                "machine-a",
                AUTH_URL_BROWSER_OPEN_INTERVAL + Duration::from_secs(60),
            );
            let worker3 = auth_wait_attempt(scope, &cancel3, &messages3);
            wait_for_status(
                &messages3,
                "https://login.tailscale.com/a/second",
                Duration::from_secs(10),
            );
            assert_eq!(opened_count(&opened), 2);
            cancel3.store(true, Ordering::Release);
            worker3.join().unwrap_or_else(|_| panic!("worker panicked"));
        });
        assert!(
            _fake.all_dead(Duration::from_secs(5)),
            "cancelled attempts left ssh children behind"
        );
    }

    #[test]
    fn saved_ssh_without_auth_banner_keeps_the_existing_timeout_and_errors() {
        let cancel = AtomicBool::new(false);
        let mut on_auth_url = |_: &str| panic!("no banner was printed");
        // A process that never writes still hits the unchanged plain timeout.
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let error = process::wait_with_output_timeout(
            child,
            Duration::from_millis(200),
            &mut process::WaitHooks {
                cancel: &cancel,
                auth_wait: Duration::from_secs(5),
                on_auth_url: &mut on_auth_url,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "noninteractive SSH command timed out");

        // Ordinary stderr failures keep their existing message and classification.
        let fake = FakeSsh::new("plainfail").with_env_text("FAKE_SSH_FAIL_TARGET", "denied-host");
        let (_messages, sink) = status_recorder();
        let error = match connect_saved_ssh(
            "denied-host",
            "saved",
            &crate::remote::SavedSshHooks {
                profile_id: "machine-b",
                cancel: &cancel,
                status: &sink,
            },
        ) {
            Ok(_) => panic!("a refused connection should not succeed"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("remote platform detection failed"));
        drop(fake);
    }

    #[test]
    fn saved_ssh_cancellation_kills_the_waiting_ssh_child() {
        let fake = FakeSsh::new("cancel")
            .with_banner("https://login.tailscale.com/a/cancel")
            .with_approve_gate();
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                connect_saved_ssh(
                    "fake-host",
                    "saved",
                    &crate::remote::SavedSshHooks {
                        profile_id: "machine-c",
                        cancel: &cancel,
                        status: &sink,
                    },
                )
            });
            let pids = fake.wait_for_pid(Duration::from_secs(10));
            assert!(!pids.is_empty(), "the fake ssh never started");
            cancel.store(true, Ordering::Release);
            let result = worker.join().unwrap_or_else(|_| panic!("worker panicked"));
            assert!(result.is_err());
            assert!(
                fake.all_dead(Duration::from_secs(5)),
                "cancelled auth wait left an ssh child behind: {pids:?}"
            );
        });
    }
}
