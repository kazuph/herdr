//! Saved SSH machine catalog (G11, P1).
//!
//! Stores named SSH connection profiles (`label`, SSH target, explicit Herdr
//! session, enabled state) in `machines.json` next to the session data dir.
//! Only the observable surface of upstream `herdr machine` is re-implemented
//! (no cherry-pick): the remote preparation pipeline, metadata cache, and
//! open-client propagation are intentionally not adopted (see SPEC G11).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// SSH probe binary and timing. BatchMode never prompts; a bounded
/// ConnectTimeout keeps stalls fail-closed instead of hanging the CLI.
const SSH_CONNECT_TIMEOUT_SECS: u64 = 15;

/// A single saved SSH machine profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineProfile {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) session: String,
    pub(crate) enabled: bool,
}

/// The persisted catalog. Serialized as a JSON array of profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineCatalog {
    #[serde(default)]
    pub(crate) profiles: Vec<MachineProfile>,
}

fn new_profile_id(label: &str, target: &str, session: &str, existing: &[MachineProfile]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let mut hasher = DefaultHasher::new();
        label.hash(&mut hasher);
        target.hash(&mut hasher);
        session.hash(&mut hasher);
        nanos.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        attempt.hash(&mut hasher);
        let id = format!("m{:012x}", hasher.finish() & 0xffff_ffff_ffff);
        if !existing.iter().any(|profile| profile.id == id) {
            return id;
        }
    }
    // Practically unreachable: fall back to a nanos-suffixed id.
    format!("m{nanos:x}")
}

fn validate_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("--label is required".to_string());
    }
    if label.len() > 120 {
        return Err("--label must be at most 120 characters".to_string());
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("missing SSH target".to_string());
    }
    if target.starts_with('-') {
        return Err("SSH target must not start with '-'".to_string());
    }
    Ok(())
}

impl MachineCatalog {
    /// Insert a profile. Rejects empty/oversized labels, bad targets, duplicate
    /// labels, and duplicate `(target, session)` pairs. Returns the new id.
    pub(crate) fn add(
        &mut self,
        label: &str,
        target: &str,
        session: &str,
    ) -> Result<String, String> {
        validate_label(label)?;
        validate_target(target)?;
        if self.profiles.iter().any(|p| p.label == label) {
            return Err(format!("machine label {label:?} is already saved"));
        }
        if self
            .profiles
            .iter()
            .any(|p| p.target == target && p.session == session)
        {
            return Err(format!(
                "machine for target {target:?} and session {session:?} is already saved"
            ));
        }
        let id = new_profile_id(label, target, session, &self.profiles);
        self.profiles.push(MachineProfile {
            id: id.clone(),
            label: label.to_string(),
            target: target.to_string(),
            session: session.to_string(),
            enabled: true,
        });
        Ok(id)
    }

    /// Remove a profile by exact id. Returns false when not found.
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        self.profiles.len() != before
    }

    /// Rename a profile by exact id. Returns false when not found.
    pub(crate) fn rename(&mut self, id: &str, label: &str) -> Result<bool, String> {
        validate_label(label)?;
        if self.profiles.iter().any(|p| p.id != id && p.label == label) {
            return Err(format!("machine label {label:?} is already saved"));
        }
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(profile) => {
                profile.label = label.to_string();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Enable/disable a profile by exact id. Returns false when not found.
    pub(crate) fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(profile) => {
                profile.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Resolve a label-or-id to an enabled profile. Exact id wins; otherwise
    /// the label must match exactly one profile. Disabled, unknown, and
    /// ambiguous references are rejected (fail-closed, no guessing).
    pub(crate) fn resolve(&self, label_or_id: &str) -> Result<&MachineProfile, String> {
        if let Some(profile) = self.profiles.iter().find(|p| p.id == label_or_id) {
            return Self::require_enabled(profile);
        }
        let mut matches = self.profiles.iter().filter(|p| p.label == label_or_id);
        match (matches.next(), matches.next()) {
            (Some(profile), None) => Self::require_enabled(profile),
            (Some(_), Some(_)) => Err(format!(
                "machine label {label_or_id:?} is ambiguous; use the profile id"
            )),
            (None, _) => Err(format!("machine {label_or_id:?} was not found")),
        }
    }

    fn require_enabled(profile: &MachineProfile) -> Result<&MachineProfile, String> {
        if profile.enabled {
            Ok(profile)
        } else {
            Err(format!(
                "machine {:?} is disabled; enable it before use",
                profile.label
            ))
        }
    }
}

fn default_path() -> PathBuf {
    crate::session::data_dir().join("machines.json")
}

fn save_json_to_path(path: &Path, catalog: &MachineCatalog) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&catalog.profiles)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    #[cfg(windows)]
    if path.exists() {
        if let Err(err) = std::fs::remove_file(path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// Load the catalog from the default path. Missing or corrupt files yield an
/// empty catalog so a bad file never blocks CLI startup.
pub(crate) fn load() -> MachineCatalog {
    load_from_path(&default_path())
}

pub(crate) fn load_from_path(path: &Path) -> MachineCatalog {
    if !path.exists() {
        return MachineCatalog::default();
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to read machine catalog");
            return MachineCatalog::default();
        }
    };
    match serde_json::from_str::<Vec<MachineProfile>>(&content) {
        Ok(profiles) => MachineCatalog { profiles },
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to parse machine catalog, starting empty");
            MachineCatalog::default()
        }
    }
}

/// Persist the catalog to the default path. Write failures are returned, never
/// swallowed.
pub(crate) fn save(catalog: &MachineCatalog) -> std::io::Result<()> {
    save_to_path(&default_path(), catalog)
}

pub(crate) fn save_to_path(path: &Path, catalog: &MachineCatalog) -> std::io::Result<()> {
    save_json_to_path(path, catalog)
}

/// Probe SSH reachability without prompts and with a bounded timeout.
/// Failure means the machine must not be saved (fail-closed).
pub(crate) fn probe_ssh_reachable(target: &str) -> std::io::Result<()> {
    probe_ssh_reachable_with_timeout(target, SSH_CONNECT_TIMEOUT_SECS)
}

fn probe_ssh_reachable_with_timeout(target: &str, timeout_secs: u64) -> std::io::Result<()> {
    let status = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={timeout_secs}"))
        .arg("--")
        .arg(target)
        .arg("true")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "ssh probe to {target:?} failed with status {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-machine-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("test temp dir");
        dir.join("machines.json")
    }

    #[test]
    fn add_rejects_bad_and_duplicate_profiles() {
        let mut catalog = MachineCatalog::default();
        assert!(catalog.add("", "host", "default").is_err());
        assert!(catalog.add("a", "", "default").is_err());
        assert!(catalog.add("a", "-oProxy", "default").is_err());
        let id = catalog.add("office", "ssh.example.com", "default").unwrap();
        assert!(id.starts_with('m'));
        assert!(catalog
            .add("office", "other.example.com", "default")
            .is_err());
        assert!(catalog.add("other", "ssh.example.com", "default").is_err());
        // Same target with a different session is a distinct profile.
        assert!(catalog.add("office2", "ssh.example.com", "s2").is_ok());
        assert_eq!(catalog.profiles.len(), 2);
    }

    #[test]
    fn rename_remove_enable_roundtrip() {
        let mut catalog = MachineCatalog::default();
        let id = catalog.add("a", "h1", "default").unwrap();
        assert!(!catalog.remove("missing"));
        assert!(!catalog.set_enabled("missing", false));
        assert_eq!(catalog.rename("missing", "x").unwrap(), false);
        assert!(catalog.rename(&id, "b").is_ok());
        assert_eq!(catalog.resolve("b").unwrap().id, id);
        assert!(catalog.set_enabled(&id, false));
        assert!(catalog.resolve("b").is_err());
        assert!(catalog.set_enabled(&id, true));
        assert!(catalog.remove(&id));
        assert!(catalog.resolve("b").is_err());
    }

    #[test]
    fn resolve_rejects_unknown_and_ambiguous_references() {
        let mut catalog = MachineCatalog::default();
        assert!(catalog.resolve("nope").is_err());
        let id1 = catalog.add("dup", "h1", "default").unwrap();
        // Force a second profile with the same label (bypasses add's guard)
        // to prove ambiguity is rejected instead of first-match wins.
        catalog.profiles.push(MachineProfile {
            id: "mdeadbeef000".to_string(),
            label: "dup".to_string(),
            target: "h2".to_string(),
            session: "default".to_string(),
            enabled: true,
        });
        assert!(catalog.resolve("dup").is_err());
        // Exact id still wins over the ambiguous label.
        assert_eq!(catalog.resolve(&id1).unwrap().target, "h1");
    }

    #[test]
    fn catalog_persists_and_tolerates_corrupt_files() {
        let path = temp_path("persist");
        let _ = std::fs::remove_file(&path);
        assert!(load_from_path(&path).profiles.is_empty());
        let mut catalog = MachineCatalog::default();
        let id = catalog.add("office", "h1", "default").unwrap();
        save_to_path(&path, &catalog).unwrap();
        let loaded = load_from_path(&path);
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].id, id);
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_from_path(&path).profiles.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ssh_probe_to_unroutable_host_fails_fast() {
        let err = probe_ssh_reachable_with_timeout("invalid.invalid.invalid", 2).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
