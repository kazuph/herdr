//! Saved endpoints attach to an existing session socket; they never launch Herdr.
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
#[cfg(test)]
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{RemoteHerdr, RemotePlatform};

#[path = "saved/approval.rs"]
mod approval;
#[path = "saved/process.rs"]
mod process;

/// tailssh holds a `check` connection open for up to 30 minutes awaiting
/// approval; matching it keeps one ssh process alive instead of throwing away
/// a fresh authentication URL on every reconnect attempt.
const SSH_AUTH_WAIT: Duration = Duration::from_secs(30 * 60);

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
    {
        if let Some(hook) = test_hooks::open_url_hook() {
            return hook(url);
        }
        // A test build never opens the real browser: with no hook installed the
        // open is only recorded so a hookless test still observes it.
        test_hooks::record_auth_url_open(url);
        Ok(())
    }
    #[cfg(not(test))]
    {
        crate::platform::open_url(url)
    }
}

/// Surface a Tailscale SSH check URL: the machine's status line carries the
/// newest URL so it can be opened from another terminal, and the local browser
/// opens it only for the first claim on the machine's shared approval record
/// — once per machine across every client and every automatic reconnect of
/// the same wait, until the connection goes through or a user-clicked retry
/// rotates the record. A wait that ends unapproved parks the machine instead
/// of reconnecting, so a tab can only reappear after a new wait — the first
/// connect of a fresh client or a user retry.
fn note_tailscale_auth_url(url: &str, hooks: &crate::remote::SavedSshHooks<'_>) {
    (hooks.status)(&format!("waiting for Tailscale SSH approval: {url}"));
    if !approval::claim(hooks.machine, url, hooks.retry_open) {
        return;
    }
    if let Err(error) = open_auth_url(url) {
        tracing::warn!(%error, %url, "failed to open the Tailscale SSH approval URL");
        // Nothing reached the user; release our claim so a later attempt may
        // open the page instead.
        approval::clear_if_current(hooks.machine, url);
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
    let mut on_auth_url = |url: &str| note_tailscale_auth_url(url, hooks);
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
) -> io::Result<RemoteHerdr> {
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
            Ok(_client_socket) => return Ok(candidate),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// First line the remote shell prints right before `exec`ing the client
/// bridge; reaching it proves the exec channel came up, which is the only
/// readiness signal the Tailscale-ssh-safe transport has.
const SAVED_BRIDGE_READY_LINE: &[u8] = b"herdr-saved-ssh-bridge-ready";
const SAVED_BRIDGE_READY_TEXT: &str = "herdr-saved-ssh-bridge-ready";
/// Lines a chatty remote login shell may print before the ready line; past
/// this many discarded bytes the output is not preamble noise anymore.
const SAVED_BRIDGE_PRELUDE_LIMIT: usize = 64 * 1024;
/// How much of the discarded preamble is kept for the failure message.
const SAVED_BRIDGE_PRELUDE_TAIL: usize = 4 * 1024;

struct SavedSshBridge {
    child: Option<Child>,
    workers: Vec<JoinHandle<()>>,
    directory: Option<PathBuf>,
}

impl Drop for SavedSshBridge {
    fn drop(&mut self) {
        let child = self.child.take().map(|mut child| {
            let _ = child.kill();
            child
        });
        let directory = self.directory.take();
        let workers = std::mem::take(&mut self.workers);
        // Transport cancellation cannot join a bridge worker on the client UI thread.
        std::thread::spawn(move || {
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            for worker in workers {
                let _ = worker.join();
            }
            if let Some(directory) = directory {
                let _ = fs::remove_dir_all(directory);
            }
        });
    }
}

/// Read the bridge's stdout until the ready line arrives. Lines before it are
/// remote login-shell noise (profile echoes, motd) and are discarded; bytes
/// already received past the ready line's newline belong to the bridge stream
/// and are returned so nothing is lost.
fn read_ready_line(stdout: &mut impl io::Read) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut skipped = 0_usize;
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match stdout.read(&mut chunk) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "saved SSH bridge closed before it was ready",
                ))
            }
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let mut rest = &chunk[..count];
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&rest[..end]);
            if line == SAVED_BRIDGE_READY_LINE {
                return Ok(rest[end + 1..].to_vec());
            }
            skipped += line.len() + 1;
            line.push(b'\n');
            tail.extend_from_slice(&line);
            line.clear();
            rest = &rest[end + 1..];
            if tail.len() > SAVED_BRIDGE_PRELUDE_TAIL {
                tail.drain(..tail.len() - SAVED_BRIDGE_PRELUDE_TAIL);
            }
            if skipped > SAVED_BRIDGE_PRELUDE_LIMIT {
                return Err(prelude_overflow(&tail));
            }
        }
        line.extend_from_slice(rest);
        if skipped + line.len() > SAVED_BRIDGE_PRELUDE_LIMIT {
            tail.extend_from_slice(&line);
            if tail.len() > SAVED_BRIDGE_PRELUDE_TAIL {
                tail.drain(..tail.len() - SAVED_BRIDGE_PRELUDE_TAIL);
            }
            return Err(prelude_overflow(&tail));
        }
    }
}

fn prelude_overflow(tail: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "saved SSH bridge never became ready; last preamble bytes: {}",
            String::from_utf8_lossy(tail).trim()
        ),
    )
}

fn forward_stream(
    target: &str,
    remote_herdr: &RemoteHerdr,
    session: &str,
    hooks: &crate::remote::SavedSshHooks<'_>,
) -> io::Result<(crate::ipc::LocalStream, Box<dyn Send>)> {
    let directory = super::private_ssh_config_dir()?;
    let mut bridge = SavedSshBridge {
        child: None,
        workers: Vec::new(),
        directory: Some(directory.clone()),
    };
    let stderr_path = directory.join("stderr");
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stderr_path)?;
    // The bridge rides the ssh exec channel's stdio instead of a forwarded
    // Unix socket: Tailscale SSH refuses streamlocal forwarding channels but
    // runs remote commands like any other sshd.
    let script = format!(
        "printf '%s\\n' {}\n{}",
        SAVED_BRIDGE_READY_TEXT,
        super::remote_bridge_command(remote_herdr, session)
    );
    let mut command = ssh_command(target);
    command
        .arg(format!("/bin/sh -c {}", super::shell_quote(&script)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    let mut child = command.spawn()?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin missing"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdout missing"))?;
    bridge.child = Some(child);

    let (client, relay) = UnixStream::pair()?;
    let client = crate::ipc::LocalStream::UdSocket(client.into());
    let (ready, ready_rx) = mpsc::channel::<io::Result<()>>();
    let mut upload_source = relay.try_clone()?;
    bridge.workers.push(thread::spawn(move || {
        // Dropping child_stdin closes the remote bridge's stdin, which makes it
        // exit on its own once the local stream is gone.
        let _ = super::copy_flush(&mut upload_source, &mut child_stdin);
    }));
    bridge.workers.push(thread::spawn(move || {
        let mut sink = relay;
        match read_ready_line(&mut child_stdout) {
            Ok(pending) => {
                let _ = ready.send(Ok(()));
                let _ = sink.write_all(&pending);
                let _ = super::copy_flush(&mut child_stdout, &mut sink);
            }
            Err(error) => {
                let _ = ready.send(Err(error));
            }
        }
        // Wake the upload direction too when the remote side is done so the
        // bridge does not linger on a live client stream.
        let _ = sink.shutdown(std::net::Shutdown::Both);
    }));

    let mut deadline = Instant::now() + timeout();
    let mut auth_wait_url: Option<String> = None;
    loop {
        let child = bridge
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned SSH bridge process missing"))?;
        if let Some(status) = child.try_wait()? {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            // A preamble overflow makes the reader drop stdout, which can kill
            // the ssh child before this loop sees the reader's verdict; report
            // the overflow rather than the exit it caused.
            if let Ok(Err(error)) = ready_rx.recv_timeout(Duration::from_secs(1)) {
                if error.kind() == io::ErrorKind::InvalidData {
                    return Err(error);
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("saved SSH bridge exited {status}: {}", stderr.trim()),
            ));
        }
        match ready_rx.try_recv() {
            Ok(Ok(())) => return Ok((client, Box::new(bridge))),
            Ok(Err(error)) => {
                let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
                let stderr = stderr.trim();
                return Err(if stderr.is_empty() {
                    error
                } else {
                    io::Error::new(error.kind(), format!("{error}: {stderr}"))
                });
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(io::Error::other("saved SSH bridge worker stopped"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if hooks.cancel.load(Ordering::Acquire) {
            return Err(connect_cancelled());
        }
        if auth_wait_url.is_none() {
            // The bridge's stderr lands in this file; watch it so a Tailscale
            // check banner starts the approval wait instead of the 15s timeout.
            if let Some(url) = fs::read(&stderr_path)
                .ok()
                .and_then(|bytes| process::tailscale_check_url(&bytes))
            {
                deadline = Instant::now() + auth_wait();
                note_tailscale_auth_url(&url, hooks);
                auth_wait_url = Some(url);
            }
        }
        if Instant::now() >= deadline {
            return Err(match &auth_wait_url {
                Some(url) => io::Error::new(
                    io::ErrorKind::TimedOut,
                    crate::remote::TailscaleApprovalTimeout::new("saved SSH bridge timed out", url),
                ),
                None => io::Error::new(io::ErrorKind::TimedOut, "saved SSH bridge timed out"),
            });
        }
        std::thread::sleep(
            super::BRIDGE_ACCEPT_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// True while the machine's shared approval record stands — an approval page
/// was opened for it and no completed connection has cleared the record yet.
pub(crate) fn approval_pending(machine: &str) -> bool {
    approval::pending(machine)
}

/// A user-clicked retry while the machine's approval is still outstanding:
/// rotate the record so the next sighted banner opens the page once more.
pub(crate) fn retry_approval_open(machine: &str) {
    approval::renew(machine);
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
    let remote_herdr = discover_socket(target, session, hooks)?;
    let result = forward_stream(target, &remote_herdr, session, hooks);
    if result.is_ok() {
        // The connection went through — an approval was granted or none was
        // needed — so the next approval wait may open the page once again.
        approval::clear(hooks.machine);
    }
    result
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
    static APPROVAL_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
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

    pub(crate) fn set_approval_dir(dir: Option<PathBuf>) {
        put(&APPROVAL_DIR, dir);
    }

    pub(crate) fn approval_dir() -> Option<PathBuf> {
        take(&APPROVAL_DIR)
    }

    static AUTH_URL_OPENS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Where a test build records a browser open when no hook is installed.
    pub(crate) fn record_auth_url_open(url: &str) {
        AUTH_URL_OPENS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(url.to_owned());
    }

    pub(crate) fn auth_url_opens() -> Vec<String> {
        AUTH_URL_OPENS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn clear_auth_url_opens() {
        AUTH_URL_OPENS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
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
import sys
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

# The saved-ssh transport runs `herdr remote-client-bridge` over the exec
# channel's stdio; the fake reproduces the ready line and echoes stdin back
# like the real bridge relaying the remote client socket.
is_bridge = "remote-client-bridge" in command
banner = env("FAKE_SSH_BANNER_URL")
gate = env("FAKE_SSH_APPROVE_FILE")
if env("FAKE_SSH_BANNER_FORWARD_ONLY") and not is_bridge:
    banner = ""
if banner and not (gate and os.path.exists(gate)):
    sys.stderr.write("# Tailscale SSH requires an additional check.\n")
    sys.stderr.write("# To authenticate, visit: %s\n" % banner)
    sys.stderr.flush()
    if gate:
        wait_ms = env("FAKE_SSH_APPROVE_WAIT_MS")
        deadline = time.time() + (float(wait_ms) / 1000 if wait_ms else 120)
        while not os.path.exists(gate):
            if time.time() > deadline:
                sys.stderr.write("check approval timed out\n")
                sys.exit(1)
            time.sleep(0.05)
        sys.stderr.write("# Authentication checked with Tailscale SSH.\n")
        sys.stderr.flush()

remote_socket = env("FAKE_SSH_SESSION_SOCKET") or "/tmp/fake-remote/herdr.sock"
session = env("FAKE_SSH_SESSION") or "saved"
if is_bridge:
    command_log = env("FAKE_SSH_COMMAND_LOG")
    if command_log:
        with open(command_log, "a") as handle:
            handle.write(command + "\n")
    failure = env("FAKE_SSH_BRIDGE_FAIL")
    if failure:
        sys.stderr.write(failure + "\n")
        sys.stderr.flush()
        sys.exit(3)
    if env("FAKE_SSH_BRIDGE_FLOOD"):
        # More than 64KiB of login-shell-style noise and no ready line. Stay
        # alive so the overflow, not the exit status, fails the connect.
        os.write(1, b"profile noise, not the bridge ready line\n" * 4096)
        while True:
            time.sleep(1)
    prelude = env("FAKE_SSH_BRIDGE_PRELUDE")
    if prelude:
        os.write(1, prelude.encode() + b"\n")
    os.write(1, b"herdr-saved-ssh-bridge-ready\n")
    greeting = env("FAKE_SSH_BRIDGE_GREETING")
    if greeting:
        os.write(1, greeting.encode())
    while True:
        chunk = os.read(0, 65536)
        if not chunk:
            break
        os.write(1, chunk)
elif "session list" in command:
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
            // Every test gets its own approval-record dir so records never leak
            // between tests or into the real state dir.
            test_hooks::set_approval_dir(Some(directory.join("approvals")));
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

        /// Only the `remote-client-bridge` exec ssh prints the check banner;
        /// the discovery commands answer immediately, so the approval wait
        /// lands in the bridge path instead of a remote probe.
        pub(crate) fn with_forward_only_banner(self, url: &str) -> Self {
            self.with_banner(url)
                .with_env_text("FAKE_SSH_BANNER_FORWARD_ONLY", "1")
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

        /// The approval lapsed again (tailssh checks expire): the banner and
        /// gate come back for later connections.
        pub(crate) fn revoke_approval(&self) {
            let _ = fs::remove_file(&self.approve_file);
        }

        /// A path inside the test directory where the fake appends the bridge
        /// command it was exec'd with, for `FAKE_SSH_COMMAND_LOG`.
        pub(crate) fn command_log(&self) -> PathBuf {
            self.directory.join("commands")
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
            test_hooks::set_approval_dir(None);
            test_hooks::clear_auth_url_opens();
            for name in [
                "FAKE_SSH_PID_FILE",
                "FAKE_SSH_APPROVE_FILE",
                "FAKE_SSH_APPROVE_WAIT_MS",
                "FAKE_SSH_BANNER_URL",
                "FAKE_SSH_BANNER_FORWARD_ONLY",
                "FAKE_SSH_FAIL_TARGET",
                "FAKE_SSH_SESSION_SOCKET",
                "FAKE_SSH_SESSION",
                "FAKE_SSH_COMMAND_LOG",
                "FAKE_SSH_BRIDGE_FAIL",
                "FAKE_SSH_BRIDGE_GREETING",
                "FAKE_SSH_BRIDGE_PRELUDE",
                "FAKE_SSH_BRIDGE_FLOOD",
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
    use std::io::Read as _;
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

    /// Hooks for a fake-host connect: `machine` keys the shared approval
    /// record, `retry_open` marks a user-clicked retry.
    fn connect_hooks<'a>(
        cancel: &'a AtomicBool,
        sink: &'a dyn Fn(&str),
        retry_open: bool,
    ) -> crate::remote::SavedSshHooks<'a> {
        crate::remote::SavedSshHooks {
            cancel,
            status: sink,
            machine: "fake-host",
            retry_open,
        }
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
    fn open_auth_url_without_a_hook_is_only_recorded_never_opened() {
        let _lock = test_hooks::lock();
        test_hooks::clear_auth_url_opens();
        // No open_url hook installed: a test build must record the open instead
        // of calling the platform browser (the real call is cfg(not(test))
        // gated out of this binary entirely).
        open_auth_url("https://login.tailscale.com/a/recorded")
            .unwrap_or_else(|error| panic!("recorded open failed: {error}"));
        assert_eq!(
            test_hooks::auth_url_opens(),
            ["https://login.tailscale.com/a/recorded"]
        );
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
        assert!(crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error));
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
                connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
            });
            // The machine's status now carries the full URL while the very same
            // ssh process keeps waiting for the check approval.
            wait_for_status(
                &messages,
                "https://login.tailscale.com/a/testcode",
                Duration::from_secs(10),
            );
            assert!(!worker.is_finished());
            // The open was claimed in the shared record while the wait runs.
            assert!(approval::recorded("fake-host").is_some());
            fake.approve();
            let (stream, bridge) = worker
                .join()
                .unwrap_or_else(|_| panic!("connect worker panicked"))
                .unwrap_or_else(|error| panic!("approved connection should complete: {error}"));
            drop(stream);
            drop(bridge);
        });
        // The completed connection cleared the record.
        assert!(approval::recorded("fake-host").is_none());
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

    /// After a completed connection cleared the record, the next approval wait
    /// opens the page once again — and a concurrent second client's wait for
    /// the same machine does not open another tab.
    #[test]
    fn approval_completion_clears_the_record_and_the_next_wait_opens_once_across_clients() {
        let fake = FakeSsh::new("reapprove")
            .with_banner("https://login.tailscale.com/a/again")
            .with_approve_gate();
        test_hooks::set_auth_wait(Some(Duration::from_millis(400)));
        let opened = open_recorder();
        let cancel = AtomicBool::new(false);
        let (messages, sink) = status_recorder();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
            });
            wait_for_status(
                &messages,
                "https://login.tailscale.com/a/again",
                Duration::from_secs(10),
            );
            assert!(approval::recorded("fake-host").is_some());
            fake.approve();
            worker
                .join()
                .unwrap_or_else(|_| panic!("connect worker panicked"))
                .unwrap_or_else(|error| panic!("approved connection should complete: {error}"));
        });
        assert!(approval::recorded("fake-host").is_none());
        assert_eq!(opened_count(&opened), 1);

        // The check lapses again: two clients reconnect at once, the banner is
        // back, and exactly one of them opens the page — once in total.
        fake.revoke_approval();
        std::thread::scope(|scope| {
            let messages_a = Arc::new(Mutex::new(Vec::new()));
            let messages_b = Arc::new(Mutex::new(Vec::new()));
            let first = scope.spawn({
                let messages = Arc::clone(&messages_a);
                let cancel = &cancel;
                move || {
                    let sink = move |message: &str| {
                        messages
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(message.to_owned());
                    };
                    connect_saved_ssh("fake-host", "saved", &connect_hooks(cancel, &sink, false))
                }
            });
            let second = scope.spawn({
                let messages = Arc::clone(&messages_b);
                let cancel = &cancel;
                move || {
                    let sink = move |message: &str| {
                        messages
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(message.to_owned());
                    };
                    connect_saved_ssh("fake-host", "saved", &connect_hooks(cancel, &sink, false))
                }
            });
            for worker in [first, second] {
                match worker
                    .join()
                    .unwrap_or_else(|_| panic!("connect worker panicked"))
                {
                    Ok(_) => panic!("the unapproved wait should not have connected"),
                    Err(error) => assert!(
                        crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error),
                        "expected the approval timeout, got: {error}"
                    ),
                }
            }
        });
        assert_eq!(
            opened_count(&opened),
            2,
            "two clients waiting on the same machine must open the page once in total"
        );
        assert!(
            approval::recorded("fake-host")
                .unwrap_or_default()
                .contains("url=https://login.tailscale.com/a/again"),
            "the record should name the URL that was opened"
        );
        assert!(fake.all_dead(Duration::from_secs(5)), "ssh child survived");
    }

    fn opened_count(opened: &Arc<Mutex<Vec<String>>>) -> usize {
        opened
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// The shared record is claimed once, rotated by a user-clicked retry,
    /// released by the claimant's failed open, and cleared by a completed
    /// connection.
    #[test]
    fn approval_record_is_claimed_once_until_a_retry_or_a_completed_connect() {
        let _fake = FakeSsh::new("record");
        let url = |name: &str| format!("https://login.tailscale.com/a/{name}");
        assert!(approval::claim("fake-host", &url("one"), false));
        // A second client (or a later automatic attempt) loses the claim.
        assert!(!approval::claim("fake-host", &url("two"), false));
        assert!(approval::recorded("fake-host")
            .unwrap_or_default()
            .starts_with(&format!("url={}\n", url("one"))));
        // A user-clicked retry rotates the record; automatic attempts lose
        // again afterwards.
        assert!(approval::claim("fake-host", &url("retry"), true));
        assert!(approval::recorded("fake-host")
            .unwrap_or_default()
            .starts_with(&format!("url={}\n", url("retry"))));
        assert!(!approval::claim("fake-host", &url("three"), false));
        // A failed open releases only its own claim, never a newer one.
        approval::clear_if_current("fake-host", &url("someone-else"));
        assert!(approval::recorded("fake-host").is_some());
        approval::clear_if_current("fake-host", &url("retry"));
        assert!(approval::recorded("fake-host").is_none());
        assert!(approval::claim("fake-host", &url("fresh"), false));
        // A completed connection clears the record.
        approval::clear("fake-host");
        assert!(approval::recorded("fake-host").is_none());
    }

    /// One connect attempt that ends in its unapproved approval wait, on a
    /// scoped worker so the caller can watch the status meanwhile.
    /// `retry_open` marks the attempt as a user-clicked retry of a parked wait.
    fn unapproved_attempt<'scope, 'env>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        cancel: &'env AtomicBool,
        messages: &'env Arc<Mutex<Vec<String>>>,
        retry_open: bool,
    ) -> std::thread::ScopedJoinHandle<'scope, io::Result<(crate::ipc::LocalStream, Box<dyn Send>)>>
    {
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
            connect_saved_ssh(
                "fake-host",
                "saved",
                &connect_hooks(cancel, &sink, retry_open),
            )
        })
    }

    #[test]
    fn saved_ssh_unapproved_approval_wait_opens_once_and_a_manual_retry_opens_once_more() {
        let _fake = FakeSsh::new("parked")
            .with_banner("https://login.tailscale.com/a/first")
            .with_approve_gate();
        test_hooks::set_auth_wait(Some(Duration::from_millis(300)));
        let opened = open_recorder();
        let cancel = AtomicBool::new(false);
        let messages = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            let worker = unapproved_attempt(scope, &cancel, &messages, false);
            wait_for_status(
                &messages,
                "https://login.tailscale.com/a/first",
                Duration::from_secs(10),
            );
            // The wait expires unapproved: exactly one tab opened, and the error
            // is the distinct approval timeout that parks the machine.
            let error = match worker
                .join()
                .unwrap_or_else(|_| panic!("connect worker panicked"))
            {
                Ok(_) => panic!("the unapproved wait should not have connected"),
                Err(error) => error,
            };
            assert!(crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error));
            assert!(error.to_string().contains("click the machine to retry"));
            assert!(error
                .to_string()
                .contains("https://login.tailscale.com/a/first"));
            assert_eq!(opened_count(&opened), 1);

            // An automatic reconnect sees the record and never opens again,
            // however many times it re-enters the approval wait.
            for _ in 0..3 {
                let automatic = unapproved_attempt(scope, &cancel, &messages, false);
                wait_for_status(&messages, "a/first", Duration::from_secs(10));
                match automatic
                    .join()
                    .unwrap_or_else(|_| panic!("automatic reconnect worker panicked"))
                {
                    Ok(_) => panic!("an unapproved reconnect should not have connected"),
                    Err(error) => assert!(
                        crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error)
                    ),
                }
            }
            assert_eq!(
                opened_count(&opened),
                1,
                "automatic reconnects must not reopen the approval page"
            );

            // The manual retry is a new attempt: its wait opens the new URL
            // exactly once, and an unapproved end parks it the same way.
            let retry = unapproved_attempt(scope, &cancel, &messages, true);
            wait_for_status(&messages, "a/first", Duration::from_secs(10));
            let error = match retry
                .join()
                .unwrap_or_else(|_| panic!("retry worker panicked"))
            {
                Ok(_) => panic!("the unapproved retry should not have connected"),
                Err(error) => error,
            };
            assert!(crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error));
            assert_eq!(opened_count(&opened), 2);

            // After the clicked retry too, automatic reconnects stay silent.
            let automatic = unapproved_attempt(scope, &cancel, &messages, false);
            match automatic
                .join()
                .unwrap_or_else(|_| panic!("automatic reconnect worker panicked"))
            {
                Ok(_) => panic!("an unapproved reconnect should not have connected"),
                Err(error) => {
                    assert!(crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error))
                }
            }
            assert_eq!(opened_count(&opened), 2);
        });
        assert!(
            _fake.all_dead(Duration::from_secs(5)),
            "unapproved waits left ssh children behind"
        );
    }

    #[test]
    fn saved_ssh_forwarding_wait_unapproved_opens_once_and_reports_tailscale_timeout() {
        let _fake = FakeSsh::new("forward-parked")
            .with_forward_only_banner("https://login.tailscale.com/a/forward")
            .with_approve_gate();
        test_hooks::set_auth_wait(Some(Duration::from_millis(300)));
        let opened = open_recorder();
        let cancel = AtomicBool::new(false);
        let (messages, sink) = status_recorder();
        let error =
            match connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false)) {
                Ok(_) => panic!("the unapproved forwarding wait should not have connected"),
                Err(error) => error,
            };
        assert!(crate::remote::saved_ssh_failure_is_tailscale_approval_timeout(&error));
        assert!(error.to_string().contains("saved SSH bridge timed out"));
        assert!(error
            .to_string()
            .contains("https://login.tailscale.com/a/forward"));
        assert_eq!(opened_count(&opened), 1);
        let messages = messages.lock().unwrap_or_else(|p| p.into_inner());
        assert!(messages
            .iter()
            .any(|message| message.contains("https://login.tailscale.com/a/forward")));
        assert!(
            _fake.all_dead(Duration::from_secs(5)),
            "the unapproved forwarding wait left an ssh child behind"
        );
    }

    #[test]
    fn saved_ssh_forwarding_approval_completes_the_same_forwarding_connection() {
        let fake = FakeSsh::new("forward-approve")
            .with_forward_only_banner("https://login.tailscale.com/a/forwardok")
            .with_approve_gate();
        let opened = open_recorder();
        let cancel = AtomicBool::new(false);
        let (messages, sink) = status_recorder();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
            });
            wait_for_status(
                &messages,
                "https://login.tailscale.com/a/forwardok",
                Duration::from_secs(10),
            );
            assert!(!worker.is_finished());
            fake.approve();
            let (stream, bridge) = worker
                .join()
                .unwrap_or_else(|_| panic!("connect worker panicked"))
                .unwrap_or_else(|error| {
                    panic!("an approved forwarding connection should complete: {error}")
                });
            drop(stream);
            drop(bridge);
        });
        assert_eq!(
            opened.lock().unwrap_or_else(|p| p.into_inner()).as_slice(),
            ["https://login.tailscale.com/a/forwardok"],
            "the forwarding wait opens the URL exactly once"
        );
        assert!(fake.all_dead(Duration::from_secs(5)), "ssh child survived");
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
            &connect_hooks(&cancel, &sink, false),
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
                connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
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

    /// Bytes the remote bridge emits right after the ready line reach the
    /// caller untouched, and bytes written to the returned stream reach the
    /// remote and echo back: the exec-channel relay is lossless both ways.
    #[test]
    fn saved_ssh_bridge_relays_post_ready_bytes_in_both_directions() {
        let fake = FakeSsh::new("relay")
            .with_env_text("FAKE_SSH_BRIDGE_GREETING", "greeting-from-remote\n");
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let (mut stream, bridge) =
            connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
                .unwrap_or_else(|error| panic!("bridge should connect: {error}"));

        let expected = b"greeting-from-remote\n";
        let mut greeting = vec![0_u8; expected.len()];
        stream
            .read_exact(&mut greeting)
            .unwrap_or_else(|error| panic!("post-ready bytes never arrived: {error}"));
        assert_eq!(greeting, expected);

        let payload = b"client-input-bytes\x00\x01\xff";
        stream
            .write_all(payload)
            .unwrap_or_else(|error| panic!("client bytes were not accepted: {error}"));
        let mut echoed = vec![0_u8; payload.len()];
        stream
            .read_exact(&mut echoed)
            .unwrap_or_else(|error| panic!("echo never arrived: {error}"));
        assert_eq!(echoed, payload);

        drop(stream);
        drop(bridge);
        assert!(fake.all_dead(Duration::from_secs(5)), "ssh child survived");
    }

    /// Login-shell noise printed before the ready line (profile echoes, motd)
    /// is skipped; the bridge still connects and post-ready bytes are intact.
    #[test]
    fn saved_ssh_bridge_skips_prelude_lines_before_the_ready_marker() {
        let fake = FakeSsh::new("prelude")
            .with_env_text(
                "FAKE_SSH_BRIDGE_PRELUDE",
                "kazuph custom motd\necho from .bashrc\nherdr-saved-ssh-bridge-ready-ish",
            )
            .with_env_text("FAKE_SSH_BRIDGE_GREETING", "greeting-after-noise");
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let (mut stream, bridge) =
            connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
                .unwrap_or_else(|error| {
                    panic!("prelude noise should not fail the bridge: {error}")
                });

        let expected = b"greeting-after-noise";
        let mut greeting = vec![0_u8; expected.len()];
        stream
            .read_exact(&mut greeting)
            .unwrap_or_else(|error| panic!("post-ready bytes never arrived: {error}"));
        assert_eq!(greeting, expected);
        drop(stream);
        drop(bridge);
        assert!(fake.all_dead(Duration::from_secs(5)), "ssh child survived");
    }

    /// Without a ready line, preamble past 64KiB fails with InvalidData that
    /// carries the last preamble lines instead of waiting forever.
    #[test]
    fn saved_ssh_bridge_prelude_over_limit_is_invalid_data() {
        let _fake = FakeSsh::new("flood").with_env_text("FAKE_SSH_BRIDGE_FLOOD", "1");
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let error =
            match connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false)) {
                Ok(_) => panic!("an endless noise preamble should not connect"),
                Err(error) => error,
            };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("never became ready"),
            "the overflow error should explain the missing ready line: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("profile noise, not the bridge ready line"),
            "the error should carry the last preamble lines: {error}"
        );
        assert!(
            _fake.all_dead(Duration::from_secs(5)),
            "a flooded bridge left an ssh child behind"
        );
    }

    /// A remote `remote-client-bridge` that fails before the ready line fails
    /// the connect, and the error carries the remote's stderr text.
    #[test]
    fn saved_ssh_bridge_remote_failure_surfaces_remote_stderr() {
        let _fake = FakeSsh::new("bridgefail").with_env_text(
            "FAKE_SSH_BRIDGE_FAIL",
            "herdr remote-client-bridge: no such session",
        );
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let error =
            match connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false)) {
                Ok(_) => panic!("a failing remote bridge should not connect"),
                Err(error) => error,
            };
        assert!(
            error
                .to_string()
                .contains("herdr remote-client-bridge: no such session"),
            "remote stderr is missing from the error: {error}"
        );
        assert!(
            _fake.all_dead(Duration::from_secs(5)),
            "a failed bridge left an ssh child behind"
        );
    }

    /// Dropping the returned stream and its lifetime guard kills the bridge's
    /// ssh child; no relay worker keeps it alive.
    #[test]
    fn dropping_the_bridge_stops_the_ssh_child() {
        let fake = FakeSsh::new("dropkill");
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let (stream, bridge) =
            connect_saved_ssh("fake-host", "saved", &connect_hooks(&cancel, &sink, false))
                .unwrap_or_else(|error| panic!("bridge should connect: {error}"));
        drop(stream);
        drop(bridge);
        assert!(
            fake.all_dead(Duration::from_secs(5)),
            "dropping the returned values left an ssh child behind: {:?}",
            fake.pids()
        );
    }

    /// A non-default saved session reaches the remote bridge as
    /// `--session <name>`, the same shape `remote_bridge_command` produces for
    /// the interactive `--remote` path.
    #[test]
    fn saved_ssh_bridge_execs_remote_bridge_with_the_saved_session() {
        let fake = FakeSsh::new("sessionflag").with_env_text("FAKE_SSH_SESSION", "fork");
        let log = fake.command_log();
        let _fake = fake.with_env("FAKE_SSH_COMMAND_LOG", &log);
        let cancel = AtomicBool::new(false);
        let (_messages, sink) = status_recorder();
        let (stream, bridge) =
            connect_saved_ssh("fake-host", "fork", &connect_hooks(&cancel, &sink, false))
                .unwrap_or_else(|error| panic!("bridge should connect: {error}"));
        drop(stream);
        drop(bridge);
        let commands = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            commands.contains("--session fork") && commands.contains("remote-client-bridge"),
            "the bridge was not exec'd with the saved session: {commands}"
        );
        assert!(_fake.all_dead(Duration::from_secs(5)), "ssh child survived");
    }
}
