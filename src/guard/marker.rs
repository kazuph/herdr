//! Per-pane agent marker files.
//!
//! A marker file at `<state>/guard/agent-panes/<HERDR_PANE_ID>.json` means
//! "herdr believes an agent is running in this pane". Shims and shell hooks
//! only judge commands when the marker named by `HERDR_GUARD_MARKER` exists,
//! which is how the guard stays limited to agent panes.
//!
//! Markers are written/removed when the agent label of a pane changes
//! (detected, released, pane died) and the whole directory is wiped at app
//! startup, so a crash can never leave a stale marker that wrongly marks a
//! later human pane.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentMarker {
    /// Effective agent label (`terminal.effective_agent_label()`).
    pub agent: String,
    /// Pane cwd at the time the marker was written. Used as the ai_guard
    /// "launch dir" for the rm path-safety rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Public pane id (`p<n>`) for log correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_pane_id: Option<String>,
    pub updated_epoch: i64,
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Read and parse a marker file. Missing or malformed files return `None`,
/// which callers treat as "no agent in this pane".
pub(crate) fn read(path: &Path) -> Option<AgentMarker> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write the marker for `pane_env_id` when `agent` is `Some`, remove it when
/// `None`. Errors are logged at debug level and otherwise ignored: a failed
/// marker update only ever makes the guard quieter, never breaks the pane.
pub(crate) fn sync(
    pane_env_id: &str,
    agent: Option<&str>,
    cwd: Option<&Path>,
    public_pane_id: Option<&str>,
) {
    let path = crate::guard::marker_path(pane_env_id);
    match agent {
        Some(agent) => write_marker(
            &path,
            &AgentMarker {
                agent: agent.to_string(),
                cwd: cwd.map(|p| p.to_string_lossy().to_string()),
                public_pane_id: public_pane_id.map(str::to_string),
                updated_epoch: now_epoch(),
            },
        ),
        None => remove_path(&path),
    }
}

fn write_marker(path: &Path, marker: &AgentMarker) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::debug!(path = %path.display(), %error, "guard: failed to create marker dir");
        return;
    }
    let Ok(content) = serde_json::to_string(marker) else {
        return;
    };
    // Write-then-rename keeps concurrent readers from seeing a torn file.
    let tmp = tmp_path_for(path);
    let result = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(content.as_bytes())?;
        std::fs::rename(&tmp, path)
    });
    if let Err(error) = result {
        tracing::debug!(path = %path.display(), %error, "guard: failed to write marker");
        let _ = std::fs::remove_file(&tmp);
    }
}

fn remove_path(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(path = %path.display(), %error, "guard: failed to remove marker");
        }
    }
}

/// Remove every marker. Called at app startup so markers from a dead server
/// cannot mislabel fresh panes.
pub(crate) fn clear_all() {
    let dir = crate::guard::marker_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("herdr-guard-marker-{}", std::process::id()));
        let path = dir.join("p_1.json");
        write_marker(
            &path,
            &AgentMarker {
                agent: "codex".to_string(),
                cwd: Some("/repo".to_string()),
                public_pane_id: Some("p1".to_string()),
                updated_epoch: 1,
            },
        );
        let marker = read(&path).expect("marker should parse");
        assert_eq!(marker.agent, "codex");
        assert_eq!(marker.cwd.as_deref(), Some("/repo"));
        remove_path(&path);
        assert!(read(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
