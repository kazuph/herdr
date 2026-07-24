use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionLedgerEntry {
    pub pane_id: u32,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: PathBuf,
    pub agent: String,
    pub session_id: String,
    pub observed_at: u128,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSessionLedger {
    #[serde(default)]
    pub entries: BTreeMap<String, AgentSessionLedgerEntry>,
}

impl AgentSessionLedger {
    pub fn key(workspace_id: &str, tab_id: &str, pane_id: u32) -> String {
        format!("{workspace_id}:{tab_id}:{pane_id}")
    }

    pub fn upsert(&mut self, entry: AgentSessionLedgerEntry) {
        self.entries.insert(
            Self::key(&entry.workspace_id, &entry.tab_id, entry.pane_id),
            entry,
        );
    }

    pub fn get(
        &self,
        workspace_id: &str,
        tab_id: &str,
        pane_id: u32,
    ) -> Option<&AgentSessionLedgerEntry> {
        self.entries.get(&Self::key(workspace_id, tab_id, pane_id))
    }
}

pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

pub fn path() -> PathBuf {
    crate::session::data_dir().join("agent-session-ledger.json")
}

pub fn load() -> AgentSessionLedger {
    load_from_path(&path())
}

pub(crate) fn load_from_path(path: &Path) -> AgentSessionLedger {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return AgentSessionLedger::default();
        }
        Err(err) => {
            warn!(err = %err, "failed to read agent session ledger");
            return AgentSessionLedger::default();
        }
    };
    match serde_json::from_str(&content) {
        Ok(ledger) => ledger,
        Err(err) => {
            warn!(err = %err, "failed to parse agent session ledger");
            AgentSessionLedger::default()
        }
    }
}

#[cfg(test)]
fn save_to_path(path: &Path, ledger: &AgentSessionLedger) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(ledger)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_round_trips_entries() {
        let dir = std::env::temp_dir().join(format!("herdr-ledger-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("agent-session-ledger.json");
        let mut ledger = AgentSessionLedger::default();
        ledger.upsert(AgentSessionLedgerEntry {
            pane_id: 7,
            terminal_id: "term_test".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:1".into(),
            cwd: PathBuf::from("/tmp"),
            agent: "codex".into(),
            session_id: "019f1140-6d40-7883-b6bc-3413eea89323".into(),
            observed_at: 1,
            source: "test".into(),
            title: Some("task".into()),
        });

        save_to_path(&path, &ledger).unwrap();
        let loaded = load_from_path(&path);

        assert_eq!(
            loaded
                .get("w1", "w1:1", 7)
                .map(|entry| entry.session_id.as_str()),
            Some("019f1140-6d40-7883-b6bc-3413eea89323")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
