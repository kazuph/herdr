//! Saved endpoints attach to an existing session socket; they never launch Herdr.
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{RemoteHerdr, RemotePlatform};

#[path = "saved/process.rs"]
mod process;

fn timeout() -> Duration {
    Duration::from_secs(crate::machine::SSH_CONNECT_TIMEOUT_SECS)
}

fn ssh_command(target: &str) -> Command {
    let mut command = Command::new("ssh");
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

fn read_remote(target: &str, script: &str) -> io::Result<std::process::Output> {
    let mut command = ssh_command(target);
    command
        .arg(format!("/bin/sh -c {}", super::shell_quote(script)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::wait_with_output_timeout(command.spawn()?, timeout())
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

fn discover_socket(target: &str, session: &str) -> io::Result<String> {
    let output = read_remote(target, "uname -s\nuname -m\n")?;
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
    let output = read_remote(target, &script)?;
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
        let output = read_remote(target, &script)?;
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
    profile_id: &str,
    target: &str,
    remote_path: &str,
) -> io::Result<(crate::ipc::LocalStream, Box<dyn Send>)> {
    let directory = super::private_ssh_config_dir()?;
    let mut bridge = SavedSshBridge {
        child: None,
        directory: Some(directory.clone()),
    };
    let digest = format!("{:x}", Sha256::digest(profile_id.as_bytes()));
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
    command = Command::new("ssh");
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
    let deadline = Instant::now() + timeout();
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
                "saved SSH socket forwarding timed out",
            ));
        }
        std::thread::sleep(
            super::BRIDGE_ACCEPT_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(crate) fn connect_saved_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
) -> io::Result<(crate::ipc::LocalStream, Box<dyn Send>)> {
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if target.is_empty() || target.starts_with('-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid saved SSH target",
        ));
    }
    let remote_path = discover_socket(target, session)?;
    forward_stream(profile_id, target, &remote_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_endpoint_session_discovery_is_exact_and_never_falls_back_to_default() {
        let bytes = br#"{"sessions":[{"name":"default","running":true,"socket_path":"/remote/default/herdr.sock"},{"name":"named","running":true,"socket_path":"/remote/other:host/herdr.sock"}]}"#;
        assert_eq!(
            existing_session_socket(bytes, "named").unwrap(),
            "/remote/other:host/herdr-client.sock"
        );
        assert!(existing_session_socket(bytes, "absent").is_err());
        assert!(existing_session_socket(br#"{"sessions":[{"name":"named","running":false,"socket_path":"/remote/herdr.sock"}]}"#, "named").is_err());
        assert!(existing_session_socket(br#"{"sessions":[{"name":"named","running":true,"socket_path":"/r/herdr.sock"},{"name":"named","running":true,"socket_path":"/s/herdr.sock"}]}"#, "named").is_err());
    }

    #[test]
    fn saved_endpoint_forwarding_paths_and_noninteractive_commands_preserve_opaque_names() {
        assert_eq!(
            forwarding_path("/remote/a:b/${HOME}/c\\d"),
            "/remote/a\\:b/$\\{HOME}/c\\\\d"
        );
        let command = ssh_command("host with spaces;literal");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[args.len() - 2..], &["--", "host with spaces;literal"]);
        for required in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "ControlMaster=no",
            "PermitLocalCommand=no",
            "ConnectTimeout=15",
        ] {
            assert!(args.iter().any(|arg| arg == required));
        }
        assert!(args
            .iter()
            .all(|arg| !arg.contains("remote-client-bridge") && !arg.contains("server")));
    }
}
