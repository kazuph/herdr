//! Discover resumable agent CLI sessions and build restore commands.
//!
//! Used by `[agent_restore]`: after a server restart the panes come back as
//! plain shells, and this module figures out which `claude --resume` /
//! `codex resume` command re-creates the agent that was running in each pane.
//!
//! Session ids are reported explicitly by integrations (`pane.report_agent`)
//! or observed from each pane's foreground process. Restore deliberately does
//! not guess from cwd because multiple panes can share a directory while
//! running different conversations.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        && !session_id.starts_with('-')
        && session_id != "last"
        && session_id != "--last"
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn is_safe_restore_template(template: &str) -> bool {
    template.contains("{session_id}") && !template.split_whitespace().any(|token| token == "--last")
}

/// Render `template` into the command typed into the pane.
///
/// Templates must contain `{session_id}` and require a safe session id. Herdr
/// never falls back to "last session" style commands because they can restore
/// the wrong conversation when panes share a cwd.
pub fn render_restore_command(template: &str, session_id: Option<&str>) -> Option<String> {
    if !is_safe_restore_template(template) {
        return None;
    }
    let session_id = session_id.filter(|id| is_safe_session_id(id))?;
    Some(template.replace("{session_id}", session_id))
}

pub fn session_id_from_cmdline(agent: &str, cmdline: &str) -> Option<String> {
    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    session_id_from_tokens(agent, &tokens)
}

fn session_id_from_tokens(agent: &str, tokens: &[&str]) -> Option<String> {
    match agent {
        "claude" => session_id_from_claude_tokens(tokens),
        "codex" => session_id_from_codex_tokens(tokens),
        _ => None,
    }
}

fn session_id_from_claude_tokens(tokens: &[&str]) -> Option<String> {
    for (index, token) in tokens.iter().enumerate() {
        if *token == "--resume" {
            return tokens
                .get(index + 1)
                .copied()
                .filter(|id| is_safe_session_id(id))
                .map(str::to_string);
        }
        if let Some(session_id) = token.strip_prefix("--resume=") {
            return is_safe_session_id(session_id).then(|| session_id.to_string());
        }
    }
    None
}

fn session_id_from_codex_tokens(tokens: &[&str]) -> Option<String> {
    for (index, token) in tokens.iter().enumerate() {
        if *token == "resume" {
            return tokens
                .get(index + 1)
                .copied()
                .filter(|id| is_safe_session_id(id))
                .map(str::to_string);
        }
    }
    None
}

pub fn codex_session_id_from_session_files(
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    codex_session_id_from_session_files_in(
        &home.join(".codex").join("sessions"),
        cwd,
        process_started_at,
    )
}

pub fn claude_session_id_from_session_files(
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    claude_session_id_from_session_files_in(
        &home.join(".claude").join("projects"),
        cwd,
        process_started_at,
    )
}

fn codex_session_id_from_session_files_in(
    root: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    let mut matches = Vec::new();
    collect_matching_codex_sessions(root, cwd, process_started_at, &mut matches);
    (matches.len() == 1).then(|| matches.remove(0))
}

fn claude_session_id_from_session_files_in(
    root: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    let mut matches = Vec::new();
    collect_matching_claude_sessions(root, cwd, process_started_at, &mut matches);
    (matches.len() == 1).then(|| matches.remove(0))
}

fn collect_matching_codex_sessions(
    dir: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
    matches: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_matching_codex_sessions(&path, cwd, process_started_at, matches);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(session_id) = matching_codex_session_file(&path, cwd, process_started_at) {
            matches.push(session_id);
        }
    }
}

fn collect_matching_claude_sessions(
    dir: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
    matches: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_matching_claude_sessions(&path, cwd, process_started_at, matches);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(session_id) = matching_claude_session_file(&path, cwd, process_started_at) {
            matches.push(session_id);
        }
    }
}

fn matching_codex_session_file(
    path: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).ok()?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first_line = lines.next()?.ok()?;
    let value: serde_json::Value = serde_json::from_str(&first_line).ok()?;
    if value.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    let session_id = payload
        .get("session_id")
        .or_else(|| payload.get("id"))?
        .as_str()?;
    if !is_safe_session_id(session_id) {
        return None;
    }
    let session_cwd = Path::new(payload.get("cwd")?.as_str()?);
    if session_cwd != cwd {
        return None;
    }
    let started_at = parse_rfc3339_z(payload.get("timestamp")?.as_str()?)?;
    let delta = duration_abs(started_at, process_started_at)?;
    (delta <= Duration::from_secs(120)).then(|| session_id.to_string())
}

fn matching_claude_session_file(
    path: &Path,
    cwd: &Path,
    process_started_at: SystemTime,
) -> Option<String> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).ok()?;
    let lines = std::io::BufReader::new(file).lines().take(64);
    for line in lines.flatten() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(session_id) = value.get("sessionId").and_then(|id| id.as_str()) else {
            continue;
        };
        if !is_safe_session_id(session_id) {
            continue;
        }
        let Some(session_cwd) = value.get("cwd").and_then(|cwd| cwd.as_str()) else {
            continue;
        };
        if Path::new(session_cwd) != cwd {
            continue;
        }
        let Some(timestamp) = value
            .get("timestamp")
            .and_then(|timestamp| timestamp.as_str())
        else {
            continue;
        };
        let Some(started_at) = parse_rfc3339_z(timestamp) else {
            continue;
        };
        let Some(delta) = duration_abs(started_at, process_started_at) else {
            continue;
        };
        if delta <= Duration::from_secs(120) {
            return Some(session_id.to_string());
        }
    }
    None
}

fn duration_abs(a: SystemTime, b: SystemTime) -> Option<Duration> {
    match a.duration_since(b) {
        Ok(duration) => Some(duration),
        Err(err) => Some(err.duration()),
    }
}

fn parse_rfc3339_z(value: &str) -> Option<SystemTime> {
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.strip_suffix('Z')?;
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add((hour as i64) * 3_600)?
        .checked_add((minute as i64) * 60)?
        .checked_add(second as i64)?;
    if secs < 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    let year = year - (month <= 2) as i32;
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u32;
    let month_prime = month + if month > 2 { 9 } else { 21 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some((era as i64) * 146_097 + (day_of_era as i64) - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            render_restore_command("codex resume {session_id}", Some("--last")),
            None
        );
        assert_eq!(
            render_restore_command("codex resume {session_id} --last", Some("abc-123")),
            None
        );
        assert_eq!(render_restore_command("pi", None), None);
        assert_eq!(
            render_restore_command("claude --resume --last", Some("abc-123")),
            None
        );
    }

    #[test]
    fn session_id_from_agent_command_extracts_resume_ids() {
        assert_eq!(
            session_id_from_cmdline(
                "codex",
                "node /path/bin/codex --sandbox workspace-write resume 019ef3a2-749c-7b52-b324-2c20cb0b2379"
            ),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379".into())
        );
        assert_eq!(
            session_id_from_cmdline(
                "claude",
                "claude --permission-mode acceptEdits --resume 11111111-2222-3333-4444-555555555555"
            ),
            Some("11111111-2222-3333-4444-555555555555".into())
        );
        assert_eq!(
            session_id_from_cmdline("claude", "claude --resume=abc-123"),
            Some("abc-123".into())
        );
        assert_eq!(
            session_id_from_cmdline("codex", "codex resume evil;rm"),
            None
        );
        assert_eq!(session_id_from_cmdline("codex", "codex resume"), None);
    }

    #[test]
    fn codex_session_file_match_requires_unique_cwd_and_start_time() {
        let root = test_temp_dir("codex-session-match");
        let day = root.join("2026").join("06").join("30");
        std::fs::create_dir_all(&day).unwrap();
        let cwd = Path::new("/tmp/project");
        let id = "019f15fb-61cf-72e1-bd27-d6625b2d7d21";
        std::fs::write(
            day.join("rollout-2026-06-30T00-43-43-019f15fb-61cf-72e1-bd27-d6625b2d7d21.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\",\"cwd\":\"{}\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        let started_at = parse_rfc3339_z("2026-06-30T00:43:41Z").unwrap();
        assert_eq!(
            codex_session_id_from_session_files_in(&root, cwd, started_at),
            Some(id.into())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn codex_session_file_match_rejects_ambiguous_candidates() {
        let root = test_temp_dir("codex-session-ambiguous");
        let day = root.join("2026").join("06").join("30");
        std::fs::create_dir_all(&day).unwrap();
        let cwd = Path::new("/tmp/project");
        for id in [
            "019f15fb-61cf-72e1-bd27-d6625b2d7d21",
            "019f15fb-61cf-72e1-bd27-d6625b2d7d22",
        ] {
            std::fs::write(
                day.join(format!("rollout-2026-06-30T00-43-43-{id}.jsonl")),
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\",\"cwd\":\"{}\"}}}}\n",
                    cwd.display()
                ),
            )
            .unwrap();
        }
        let started_at = parse_rfc3339_z("2026-06-30T00:43:41Z").unwrap();
        assert_eq!(
            codex_session_id_from_session_files_in(&root, cwd, started_at),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_session_file_match_reads_cwd_session_id_and_start_time() {
        let root = test_temp_dir("claude-session-match");
        let project = root.join("-tmp-project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = Path::new("/tmp/project");
        let id = "41a7c4e9-1f20-4a92-9f12-1f8b98d3a9b4";
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            format!(
                "{{\"type\":\"ai-title\",\"sessionId\":\"{id}\"}}\n{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\"}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        let started_at = parse_rfc3339_z("2026-06-30T00:43:41Z").unwrap();
        assert_eq!(
            claude_session_id_from_session_files_in(&root, cwd, started_at),
            Some(id.into())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_session_file_match_rejects_ambiguous_candidates() {
        let root = test_temp_dir("claude-session-ambiguous");
        let project = root.join("-tmp-project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = Path::new("/tmp/project");
        for id in [
            "41a7c4e9-1f20-4a92-9f12-1f8b98d3a9b4",
            "51a7c4e9-1f20-4a92-9f12-1f8b98d3a9b4",
        ] {
            std::fs::write(
                project.join(format!("{id}.jsonl")),
                format!(
                    "{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\"}}\n",
                    cwd.display()
                ),
            )
            .unwrap();
        }
        let started_at = parse_rfc3339_z("2026-06-30T00:43:41Z").unwrap();
        assert_eq!(
            claude_session_id_from_session_files_in(&root, cwd, started_at),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("herdr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }
}
