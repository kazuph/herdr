//! Launch evidence is retained with the runtime; a marker alone cannot prove
//! that an already running process inherited the guard environment.
use std::path::PathBuf;

use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GuardLaunch {
    supported: bool,
    enforcing: bool,
    ready: Option<PathBuf>,
}

impl GuardLaunch {
    pub(crate) fn from_command(cmd: &CommandBuilder) -> Option<Self> {
        let startup = cmd.get_env("HERDR_GUARD_STARTUP")?.to_str()?;
        Some(Self {
            supported: startup == "supported",
            enforcing: cmd.get_env("HERDR_GUARD_MODE").and_then(|v| v.to_str()) == Some("enforce"),
            ready: cmd.get_env("HERDR_GUARD_READY").map(PathBuf::from),
        })
    }

    pub(crate) fn reason(&self, mode: crate::config::GuardModeConfig) -> Option<&'static str> {
        if !self.supported {
            Some("shell の起動後に shim の PATH を保証できません")
        } else if self.enforcing != (mode == crate::config::GuardModeConfig::Enforce) {
            Some("起動時と現在の見張りモードが異なります")
        } else if self.ready.as_ref().is_some_and(|path| !path.is_file()) {
            Some("bash の最初の prompt で shim を先頭にした確認がありません")
        } else {
            None
        }
    }
}

pub(crate) fn warning(pane: &str, reason: &str) -> String {
    format!(
        "見張れていない agent pane {pane}: {reason}。新しい pane で agent を起動し直してください"
    )
}

fn directory() -> PathBuf {
    super::guard_root().join("uncovered")
}

pub(crate) fn sync(pane: &str, message: Option<&str>) {
    let path = directory().join(format!("{pane}.json"));
    if let Some(message) = message {
        let result = std::fs::create_dir_all(directory()).and_then(|()| {
            let bytes = serde_json::to_vec(message)?;
            let temp = path.with_extension(format!("tmp-{}", std::process::id()));
            std::fs::write(&temp, bytes)?;
            std::fs::rename(temp, &path)
        });
        if let Err(error) = result {
            tracing::warn!(%error, %pane, "guard: failed to save coverage warning");
        }
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn warnings() -> Vec<String> {
    let mut warnings: Vec<String> = std::fs::read_dir(directory())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
        .collect();
    warnings.sort();
    warnings
}

pub(crate) fn clear_all() {
    if let Ok(entries) = std::fs::read_dir(directory()) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_requires_supported_matching_launch_and_bash_prompt() {
        let mode = crate::config::GuardModeConfig::Enforce;
        let mut cmd = CommandBuilder::new("bash");
        assert!(GuardLaunch::from_command(&cmd).is_none());
        cmd.env("HERDR_GUARD_STARTUP", "unsupported");
        cmd.env("HERDR_GUARD_MODE", "enforce");
        assert!(GuardLaunch::from_command(&cmd)
            .unwrap()
            .reason(mode)
            .is_some());
        cmd.env("HERDR_GUARD_STARTUP", "supported");
        assert!(GuardLaunch::from_command(&cmd)
            .unwrap()
            .reason(mode)
            .is_none());
        assert!(GuardLaunch::from_command(&cmd)
            .unwrap()
            .reason(crate::config::GuardModeConfig::Record)
            .is_some());
        let ready = std::env::temp_dir().join(format!("guard-ready-{}", std::process::id()));
        let _ = std::fs::remove_file(&ready);
        cmd.env("HERDR_GUARD_READY", &ready);
        let launch = GuardLaunch::from_command(&cmd).unwrap();
        assert!(launch.reason(mode).is_some());
        std::fs::write(&ready, "").unwrap();
        assert!(launch.reason(mode).is_none());
        std::fs::remove_file(ready).unwrap();
    }
}
