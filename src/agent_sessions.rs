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

use std::path::Path;

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

pub fn session_id_from_open_session_file(agent: &str, path: &Path, cwd: &Path) -> Option<String> {
    match agent {
        "claude" => session_id_from_claude_file(path, cwd),
        "codex" => session_id_from_codex_file(path, cwd),
        _ => None,
    }
}

pub fn session_id_from_claude_process_record(pid: u32, cwd: &Path) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = Path::new(&home)
        .join(".claude")
        .join("sessions")
        .join(format!("{pid}.json"));
    session_id_from_claude_process_record_file(pid, cwd, &path)
}

fn session_id_from_claude_process_record_file(pid: u32, cwd: &Path, path: &Path) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if value.get("pid")?.as_u64()? != u64::from(pid) {
        return None;
    }
    let session_id = value.get("sessionId")?.as_str()?;
    if !is_safe_session_id(session_id) {
        return None;
    }
    let session_cwd = Path::new(value.get("cwd")?.as_str()?);
    if session_cwd != cwd {
        return None;
    }
    Some(session_id.to_string())
}

fn session_id_from_codex_file(path: &Path, cwd: &Path) -> Option<String> {
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
    Some(session_id.to_string())
}

fn session_id_from_claude_file(path: &Path, cwd: &Path) -> Option<String> {
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
        if Path::new(session_cwd) == cwd {
            return Some(session_id.to_string());
        }
    }
    None
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
    fn codex_open_session_file_match_accepts_exact_path() {
        let root = test_temp_dir("codex-session-resume-late-match");
        let day = root.join("2026").join("06").join("30");
        std::fs::create_dir_all(&day).unwrap();
        let cwd = Path::new("/tmp/project");
        let id = "019f15fb-61cf-72e1-bd27-d6625b2d7d21";
        let path =
            day.join("rollout-2026-06-30T00-43-43-019f15fb-61cf-72e1-bd27-d6625b2d7d21.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\",\"cwd\":\"{}\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        assert_eq!(
            session_id_from_open_session_file("codex", &path, cwd),
            Some(id.into()),
            "the exact file opened by the agent is authoritative"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn codex_open_session_file_rejects_other_cwd() {
        let root = test_temp_dir("codex-session-other-cwd");
        let day = root.join("2026").join("06").join("30");
        std::fs::create_dir_all(&day).unwrap();
        let id = "019f15fb-61cf-72e1-bd27-d6625b2d7d21";
        let path =
            day.join("rollout-2026-06-30T00-43-43-019f15fb-61cf-72e1-bd27-d6625b2d7d21.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\",\"cwd\":\"/tmp/other\"}}}}\n"
            ),
        )
        .unwrap();

        assert_eq!(
            session_id_from_open_session_file("codex", &path, Path::new("/tmp/project")),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_open_session_file_reads_cwd_session_id() {
        let root = test_temp_dir("claude-session-match");
        let project = root.join("-tmp-project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = Path::new("/tmp/project");
        let id = "41a7c4e9-1f20-4a92-9f12-1f8b98d3a9b4";
        let session_path = project.join(format!("{id}.jsonl"));
        std::fs::write(
            &session_path,
            format!(
                "{{\"type\":\"ai-title\",\"sessionId\":\"{id}\"}}\n{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{id}\",\"timestamp\":\"2026-06-30T00:43:43.979Z\"}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        assert_eq!(
            session_id_from_open_session_file("claude", &session_path, cwd),
            Some(id.into())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_open_session_file_rejects_other_cwd() {
        let root = test_temp_dir("claude-session-other-cwd");
        let project = root.join("-tmp-project");
        std::fs::create_dir_all(&project).unwrap();
        let id = "41a7c4e9-1f20-4a92-9f12-1f8b98d3a9b4";
        let session_path = project.join(format!("{id}.jsonl"));
        std::fs::write(
            &session_path,
            format!("{{\"type\":\"user\",\"cwd\":\"/tmp/other\",\"sessionId\":\"{id}\"}}\n"),
        )
        .unwrap();

        assert_eq!(
            session_id_from_open_session_file("claude", &session_path, Path::new("/tmp/project")),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_process_record_reads_exact_pid_cwd_session_id() {
        let root = test_temp_dir("claude-process-record");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("47341.json");
        let cwd = Path::new("/Users/kazuph");
        let id = "5c9b2148-f225-4422-a524-cd0565cbc625";
        std::fs::write(
            &path,
            format!(
                r#"{{"pid":47341,"sessionId":"{id}","cwd":"{}","status":"idle"}}"#,
                cwd.display()
            ),
        )
        .unwrap();

        assert_eq!(
            session_id_from_claude_process_record_file(47341, cwd, &path),
            Some(id.into())
        );
        assert_eq!(
            session_id_from_claude_process_record_file(47342, cwd, &path),
            None
        );
        assert_eq!(
            session_id_from_claude_process_record_file(47341, Path::new("/tmp/project"), &path),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("herdr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }
}
