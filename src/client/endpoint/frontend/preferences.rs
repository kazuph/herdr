// Ported from fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0.
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClientCollapsedSection {
    pub(crate) profile_id: Option<String>,
    pub(crate) section: crate::workspace::WorkspaceSection,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClientRemoteCollapsedGroups {
    pub(crate) profile_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) collapsed_groups: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ClientChromePreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) collapsed_sections: Option<Vec<ClientCollapsedSection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sidebar_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sidebar_section_split: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sidebar_collapsed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_panel_sort: Option<crate::config::AgentPanelSortConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) collapsed_groups: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) remote_collapsed_groups: Vec<ClientRemoteCollapsedGroups>,
}

impl ClientChromePreferences {
    pub(super) fn local_collapsed_groups(
        &self,
        legacy: &std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        self.collapsed_groups
            .as_ref()
            .map_or_else(|| legacy.clone(), |keys| keys.iter().cloned().collect())
    }
}

pub(crate) fn path_for_local_endpoint(socket_path: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in socket_path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("local-{hash:016x}.json"))
}

pub(crate) fn load(path: &Path) -> Option<ClientChromePreferences> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn store(path: &Path, preferences: ClientChromePreferences) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create client shell state directory: {error}"))?;
    let content = serde_json::to_vec_pretty(&preferences)
        .map_err(|error| format!("failed to encode client shell state: {error}"))?;
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path
        .file_name()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?
        .to_os_string();
    temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, content)
        .map_err(|error| format!("failed to write client shell state: {error}"))?;
    std::fs::rename(&temp_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        format!("failed to replace client shell state: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_only_preferences_inherit_legacy_group_but_saved_expand_reopens_empty() {
        let path = std::env::current_dir()
            .unwrap()
            .join(".local")
            .join(format!(
                "group-presence-{}-{}.json",
                std::process::id(),
                NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::write(&path, r#"{"sidebar_width":72}"#).unwrap();
        let legacy = std::collections::HashSet::from(["opaque Local repo".to_owned()]);
        let mut loaded = load(&path).unwrap();
        assert!(loaded.collapsed_groups.is_none());
        assert_eq!(loaded.local_collapsed_groups(&legacy), legacy);
        loaded.collapsed_groups = Some(Vec::new());
        store(&path, loaded).unwrap();
        let reopened = load(&path).unwrap();
        assert_eq!(reopened.sidebar_width, Some(72));
        assert_eq!(reopened.collapsed_groups, Some(Vec::new()));
        assert!(reopened.local_collapsed_groups(&legacy).is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        let first = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let again = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let second = path_for_local_endpoint(Path::new("/run/herdr/two.sock"));
        assert_eq!(first, again);
        assert_ne!(first, second);
    }

    #[test]
    fn legacy_preferences_default_remote_collapses() {
        let preferences: ClientChromePreferences =
            serde_json::from_str(r#"{"collapsed_groups":["/repo"]}"#)
                .expect("legacy client chrome preferences");

        assert_eq!(preferences.collapsed_groups, Some(vec!["/repo".to_owned()]));
        assert!(preferences.remote_collapsed_groups.is_empty());
    }

    #[test]
    fn concurrent_stores_leave_complete_preferences() {
        let path = std::env::current_dir()
            .expect("test worktree")
            .join(".local")
            .join(format!(
                "herdr-shell-concurrent-preferences-{}.json",
                std::process::id()
            ));
        let _ = std::fs::remove_file(&path);
        let writers = (20..28)
            .map(|width| {
                let path = path.clone();
                std::thread::spawn(move || {
                    store(
                        &path,
                        ClientChromePreferences {
                            sidebar_width: Some(width),
                            ..ClientChromePreferences::default()
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("preference writer").expect("store");
        }
        assert!(load(&path)
            .and_then(|saved| saved.sidebar_width)
            .is_some_and(|width| (20..28).contains(&width)));
        std::fs::remove_file(path).expect("remove preferences");
    }

    #[test]
    fn repeated_store_replaces_existing_preferences() {
        let path = std::env::current_dir()
            .expect("test worktree")
            .join(".local")
            .join(format!(
                "herdr-shell-preferences-{}.json",
                std::process::id()
            ));
        let _ = std::fs::remove_file(&path);
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(24),
                ..ClientChromePreferences::default()
            },
        )
        .expect("first preference store");
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(32),
                ..ClientChromePreferences::default()
            },
        )
        .expect("replacement preference store");
        assert_eq!(load(&path).and_then(|saved| saved.sidebar_width), Some(32));
        std::fs::remove_file(path).expect("remove preferences");
    }
}
