//! Shared record of which client already opened a saved machine's Tailscale
//! SSH approval page.
//!
//! Every herdr client on this machine shares the records under the state dir,
//! so the approval page opens once per machine no matter how many screens are
//! connected and reconnecting into the same approval wait. The record is keyed
//! by the saved machine id, survives client restarts, and is removed when an
//! ssh connection actually completes; a user-clicked retry rotates it so that
//! attempt opens the page once more.
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn directory() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = super::test_hooks::approval_dir() {
        return dir;
    }
    crate::config::state_dir().join("tailscale-approvals")
}

/// The record filename carries the saved machine id; anything outside a small
/// path-safe alphabet is flattened so an unusual id cannot escape the dir.
fn record_path(machine: &str) -> PathBuf {
    let key: String = machine
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    directory().join(if key.is_empty() { "unknown" } else { &key })
}

/// Claim the shared record for `machine`: only the first claimant opens the
/// approval page; every other client and every later automatic reconnect sees
/// the record and leaves the browser alone. `renew` is a user-clicked retry of
/// a parked wait: it replaces the record so that attempt opens the page once
/// more. The file records which client (pid) opened the page and when.
pub(super) fn claim(machine: &str, url: &str, renew: bool) -> bool {
    let dir = directory();
    let path = record_path(machine);
    if renew {
        let _ = fs::remove_file(&path);
    }
    if let Err(error) = fs::create_dir_all(&dir) {
        tracing::warn!(
            %error,
            dir = %dir.display(),
            "cannot record a Tailscale approval open; suppressing the browser open"
        );
        return false;
    }
    let record = format!(
        "url={url}\npid={}\nopened_unix_secs={}\n",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default()
    );
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(record.as_bytes()) {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "cannot write the Tailscale approval record"
                );
                let _ = fs::remove_file(&path);
                return false;
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "cannot claim the Tailscale approval record; suppressing the browser open"
            );
            false
        }
    }
}

/// True while the machine's record stands — an approval page was opened and
/// no completed connection has cleared it — so the next claim would lose.
pub(super) fn pending(machine: &str) -> bool {
    record_path(machine).is_file()
}

/// A user-clicked retry drops the standing record so the next sighted banner
/// claims it and opens the page once more; a no-op when nothing was claimed.
pub(super) fn renew(machine: &str) {
    let _ = fs::remove_file(record_path(machine));
}

/// The ssh connection completed — the approval went through or none was
/// needed — so drop the record and let the next approval wait open once.
pub(super) fn clear(machine: &str) {
    let _ = fs::remove_file(record_path(machine));
}

/// Release this claim only while the record still names this exact open — for
/// example after the browser open itself failed — without clobbering a newer
/// claim another client may have just made.
pub(super) fn clear_if_current(machine: &str, url: &str) {
    let path = record_path(machine);
    if fs::read_to_string(&path)
        .map(|content| content.starts_with(&format!("url={url}\n")))
        .unwrap_or(false)
    {
        let _ = fs::remove_file(&path);
    }
}

#[cfg(test)]
pub(super) fn recorded(machine: &str) -> Option<String> {
    fs::read_to_string(record_path(machine)).ok()
}
