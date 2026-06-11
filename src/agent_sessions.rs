//! Discover resumable agent CLI sessions and build restore commands.
//!
//! Used by `[agent_restore]`: after a server restart the panes come back as
//! plain shells, and this module figures out which `claude --resume` /
//! `codex resume` command re-creates the agent that was running in each pane.
//!
//! Session ids reported explicitly by integrations (`pane.report_agent`)
//! take precedence; the filesystem discovery here is the fallback for agents
//! that never reported one.

use std::path::{Path, PathBuf};

/// Maximum number of codex rollout files inspected per discovery, newest
/// date directories first. Bounds startup cost on long-lived machines.
const CODEX_SCAN_FILE_LIMIT: usize = 2000;

pub fn builtin_restore_template(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("claude --resume {session_id}"),
        "codex" => Some("codex resume {session_id}"),
        _ => None,
    }
}

/// Resolve the restore command template for `agent`: user config entries
/// overlay the built-in defaults.
pub fn restore_template<'a>(
    commands: &'a std::collections::BTreeMap<String, String>,
    agent: &'a str,
) -> Option<&'a str> {
    commands
        .get(agent)
        .map(String::as_str)
        .or_else(|| builtin_restore_template(agent))
}

/// Session ids are typed into a shell, so only allow a conservative charset
/// (uuids and similar tokens). Anything else is rejected rather than quoted.
pub fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Render `template` into the command typed into the pane.
///
/// Templates containing `{session_id}` require a safe session id and return
/// `None` without one; templates without the placeholder are plain relaunch
/// commands and ignore the session id.
pub fn render_restore_command(template: &str, session_id: Option<&str>) -> Option<String> {
    if !template.contains("{session_id}") {
        return Some(template.to_string());
    }
    let session_id = session_id.filter(|id| is_safe_session_id(id))?;
    Some(template.replace("{session_id}", session_id))
}

pub fn discover_session_id(agent: &str, cwd: &Path) -> Option<String> {
    match agent {
        "claude" => discover_claude_session(&claude_projects_root()?, cwd),
        "codex" => discover_codex_session(&codex_sessions_root()?, cwd),
        _ => None,
    }
}

fn claude_projects_root() -> Option<PathBuf> {
    let base = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs_home().map(|home| home.join(".claude")))?;
    Some(base.join("projects"))
}

fn codex_sessions_root() -> Option<PathBuf> {
    let base = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs_home().map(|home| home.join(".codex")))?;
    Some(base.join("sessions"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").map(PathBuf::from).ok()
}

/// Claude Code stores transcripts as
/// `<projects_root>/<cwd with [/.] replaced by '-'>/<uuid>.jsonl`.
/// The most recently modified transcript is the session that was running.
pub fn discover_claude_session(projects_root: &Path, cwd: &Path) -> Option<String> {
    let encoded = cwd
        .to_string_lossy()
        .chars()
        .map(|ch| if ch == '/' || ch == '.' { '-' } else { ch })
        .collect::<String>();
    let project_dir = projects_root.join(encoded);
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(project_dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if !looks_like_uuid(stem) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(at, _)| modified > *at) {
            best = Some((modified, stem.to_string()));
        }
    }
    best.map(|(_, id)| id)
}

/// Codex stores rollouts as
/// `<sessions_root>/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl` whose first
/// line carries `payload.cwd` and `payload.id`. Scan newest date directories
/// first and keep the most recently modified rollout matching `cwd`.
pub fn discover_codex_session(sessions_root: &Path, cwd: &Path) -> Option<String> {
    let cwd = cwd.to_string_lossy();
    let mut scanned = 0usize;
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for day_dir in date_dirs_newest_first(sessions_root) {
        for entry in std::fs::read_dir(&day_dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            scanned += 1;
            if scanned > CODEX_SCAN_FILE_LIMIT {
                return best.map(|(_, id)| id);
            }
            let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
                continue;
            };
            if best.as_ref().is_some_and(|(at, _)| modified <= *at) {
                continue;
            }
            if let Some(id) = rollout_session_id_for_cwd(&entry.path(), &cwd) {
                best = Some((modified, id));
            }
        }
        // Older date directories cannot beat an existing match on mtime in
        // practice (rollouts are only appended while the session runs), so
        // stop descending once a match exists.
        if best.is_some() {
            break;
        }
    }
    best.map(|(_, id)| id)
}

/// All `<root>/<YYYY>/<MM>/<DD>` directories, newest first.
fn date_dirs_newest_first(root: &Path) -> Vec<PathBuf> {
    fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|ty| ty.is_dir()))
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        dirs.reverse();
        dirs
    }

    let mut days = Vec::new();
    for year in sorted_subdirs_desc(root) {
        for month in sorted_subdirs_desc(&year) {
            days.extend(sorted_subdirs_desc(&month));
        }
    }
    days
}

fn rollout_session_id_for_cwd(path: &Path, cwd: &str) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    let meta: serde_json::Value = serde_json::from_str(&first_line).ok()?;
    let payload = meta.get("payload")?;
    if payload.get("cwd")?.as_str()? != cwd {
        return None;
    }
    Some(payload.get("id")?.as_str()?.to_string())
}

fn looks_like_uuid(text: &str) -> bool {
    text.len() == 36 && text.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-agent-sessions-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn restore_template_overlays_user_commands_on_builtins() {
        let mut commands = std::collections::BTreeMap::new();
        assert_eq!(
            restore_template(&commands, "claude"),
            Some("claude --resume {session_id}")
        );
        commands.insert("claude".into(), "ccam --resume {session_id}".into());
        assert_eq!(
            restore_template(&commands, "claude"),
            Some("ccam --resume {session_id}")
        );
        assert_eq!(
            restore_template(&commands, "codex"),
            Some("codex resume {session_id}")
        );
        assert_eq!(restore_template(&commands, "pi"), None);
        commands.insert("pi".into(), "pi".into());
        assert_eq!(restore_template(&commands, "pi"), Some("pi"));
    }

    #[test]
    fn render_restore_command_requires_safe_session_id() {
        assert_eq!(
            render_restore_command("claude --resume {session_id}", Some("abc-123")),
            Some("claude --resume abc-123".into())
        );
        assert_eq!(
            render_restore_command("claude --resume {session_id}", None),
            None
        );
        assert_eq!(
            render_restore_command("claude --resume {session_id}", Some("evil; rm -rf /")),
            None
        );
        assert_eq!(render_restore_command("pi", None), Some("pi".into()));
    }

    #[test]
    fn discover_claude_session_picks_latest_transcript_for_cwd() {
        let root = temp_dir("claude");
        let project = root.join("-Users-me-src-github-com-me-app");
        std::fs::create_dir_all(&project).unwrap();
        let old = project.join("11111111-1111-1111-1111-111111111111.jsonl");
        let new = project.join("22222222-2222-2222-2222-222222222222.jsonl");
        std::fs::write(&old, "{}").unwrap();
        std::fs::write(&new, "{}").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let file = std::fs::File::options().append(true).open(&old).unwrap();
        file.set_modified(past).unwrap();
        std::fs::write(project.join("agent-not-a-session.jsonl"), "{}").unwrap();

        assert_eq!(
            discover_claude_session(&root, Path::new("/Users/me/src/github.com/me/app")),
            Some("22222222-2222-2222-2222-222222222222".into())
        );
        assert_eq!(
            discover_claude_session(&root, Path::new("/Users/me/elsewhere")),
            None
        );
    }

    #[test]
    fn discover_codex_session_matches_cwd_from_rollout_meta() {
        let root = temp_dir("codex");
        let day = root.join("2026").join("06").join("11");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-2026-06-11T10-00-00-aaa.jsonl"),
            r#"{"payload":{"id":"id-match","cwd":"/Users/me/app"}}"#,
        )
        .unwrap();
        std::fs::write(
            day.join("rollout-2026-06-11T11-00-00-bbb.jsonl"),
            r#"{"payload":{"id":"id-other","cwd":"/Users/me/other"}}"#,
        )
        .unwrap();

        assert_eq!(
            discover_codex_session(&root, Path::new("/Users/me/app")),
            Some("id-match".into())
        );
        assert_eq!(
            discover_codex_session(&root, Path::new("/Users/me/nowhere")),
            None
        );
    }
}
