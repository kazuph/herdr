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
}
