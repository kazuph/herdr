use std::io;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

use portable_pty::CommandBuilder;

pub(crate) const HERDR_PANE_ID_ENV_VAR: &str = "HERDR_PANE_ID";
pub(crate) const HERDR_TAB_ID_ENV_VAR: &str = "HERDR_TAB_ID";
pub(crate) const HERDR_WORKSPACE_ID_ENV_VAR: &str = "HERDR_WORKSPACE_ID";

pub(crate) const PI_CODING_AGENT_DIR_ENV_VAR: &str = "PI_CODING_AGENT_DIR";
pub(crate) const CLAUDE_CONFIG_DIR_ENV_VAR: &str = "CLAUDE_CONFIG_DIR";
pub(crate) const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
pub(crate) const KIMI_CODE_HOME_ENV_VAR: &str = "KIMI_CODE_HOME";
pub(crate) const COPILOT_HOME_ENV_VAR: &str = "COPILOT_HOME";
pub(crate) const QODERCLI_CONFIG_DIR_ENV_VAR: &str = "QODER_CONFIG_DIR";
pub(crate) const CURSOR_CONFIG_DIR_ENV_VAR: &str = "CURSOR_CONFIG_DIR";

/// Environment variables that identify one specific running agent session.
///
/// Herdr servers are often started from inside an agent session, so these
/// markers can end up in the server environment and would otherwise leak into
/// every pane the server spawns. A pane is a fresh top-level context, never a
/// child of the agent session that hosted the server, so the markers must not
/// propagate. Claude Code in particular treats an inherited
/// `CLAUDE_CODE_CHILD_SESSION` as proof it is a sub-agent session and turns
/// transcript persistence off, which also removes the
/// `sessions/<pid>.json` record Herdr uses to observe the session id.
///
/// Only session- or instance-scoped markers belong here, measured from a
/// real Claude Code child environment (`AI_AGENT`, `CLAUDECODE`,
/// `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_ENTRYPOINT`,
/// `CLAUDE_CODE_EXECPATH`, `CLAUDE_CODE_MESSAGING_SOCKET`,
/// `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SESSION_ATTENDED`,
/// `CLAUDE_CODE_SESSION_ID`). Configuration-style variables
/// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, effort/feature toggles) must keep
/// flowing to pane children.
const INHERITED_AGENT_SESSION_ENV_VARS: &[&str] = &[
    // Claude Code marks the processes it spawns so tools know they run
    // inside an agent session; a fresh pane is not that session's child.
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    // Codex exposes its session id to spawned children; a new pane starts a
    // new thread, not the inherited one.
    "CODEX_THREAD_ID",
    // Herdr's own foreground-process identity hint; a plain pane shell is
    // not the wrapped agent it names.
    "HERDR_AGENT",
];

/// Guard coverage and plumbing state belongs to the pane the guard injected
/// it into. A server started inside a guarded pane inherits these markers and
/// would leak them into every spawn, where `GuardLaunch::from_command` would
/// read a covered launch out of a command the guard never injected — marking
/// an unwatched pane as watched. `apply_guard_env` always sets fresh values
/// after this scrub, so removing the inherited copies is fail-closed.
///
/// `HERDR_GUARD_PANE_SHELL` is deliberately absent: pane-shell spawns set it
/// on the command before this scrub runs, and an inherited copy can only make
/// the supported-shell check stricter (fail-closed), never looser.
const INHERITED_GUARD_ENV_VARS: &[&str] = &[
    "HERDR_GUARD_STARTUP",
    "HERDR_GUARD_MODE",
    "HERDR_GUARD_READY",
    "HERDR_GUARD_ENFORCING",
    "HERDR_GUARD_MARKER",
    "HERDR_GUARD_SHIM_DIR",
    "HERDR_GUARD_SHELL_DIR",
    "HERDR_GUARD_HERDR",
    "HERDR_GUARD_VIA",
    "HERDR_GUARD_BASH_ENV_ORIG",
    "HERDR_GUARD_ZDOTDIR_ORIG",
];

/// `PROMPT_COMMAND` tail appended by the guard: the shim-pin case line from
/// `pin_path_script`, an optional enforcing export, and the ready-file write.
const GUARD_PIN_PATH_PREFIX: &str = "case \"$PATH\" in \"$HERDR_GUARD_SHIM_DIR\"";
const GUARD_ENFORCING_EXPORT: &str = "export HERDR_GUARD_ENFORCING=1";
const GUARD_READY_WRITE: &str = "builtin printf '' > \"$HERDR_GUARD_READY\"";

/// True when `value` provably points at a file the guard injected. Matches
/// this process's guard state directory and, for servers launched inside a
/// pane of a *different* herdr namespace, the well-known injected paths
/// (`.../guard/guard.bash`, `.../guard/zdotdir`) — both are internal layout,
/// so user values elsewhere still survive.
fn is_guard_owned_path(value: Option<&std::ffi::OsStr>) -> bool {
    value.map(PathBuf::from).is_some_and(|path| {
        if path.starts_with(crate::guard::guard_root()) {
            return true;
        }
        let file_name = path.file_name().and_then(|name| name.to_str());
        let parent_name = path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        parent_name == Some("guard") && matches!(file_name, Some("guard.bash") | Some("zdotdir"))
    })
}

/// Returns the prompt command with the injected guard tail removed, or None
/// when no injection is present. The caller's own lines are preserved.
fn strip_injected_prompt_command(value: &str) -> Option<String> {
    if !value.contains("HERDR_GUARD_READY") {
        return None;
    }
    let cut = [
        value.find(GUARD_PIN_PATH_PREFIX),
        value.find(GUARD_ENFORCING_EXPORT),
        value.find(GUARD_READY_WRITE),
    ]
    .into_iter()
    .flatten()
    .min()?;
    Some(value[..cut].trim_end().to_string())
}

/// Decides what to do with the generic-name variables a guard injection
/// writes, given a read-only `get` accessor. `BASH_ENV`/`ZDOTDIR` are removed
/// only when they provably point at files the guard injected (user values
/// survive); `PROMPT_COMMAND` keeps the caller's prefix with only the
/// injected guard tail stripped.
fn injected_shell_env_plan(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> (Vec<&'static str>, Vec<(&'static str, String)>) {
    let mut remove = Vec::new();
    let mut set = Vec::new();
    for name in ["BASH_ENV", "ZDOTDIR"] {
        if is_guard_owned_path(get(name).as_deref()) {
            remove.push(name);
        }
    }
    if let Some(stripped) =
        get("PROMPT_COMMAND").and_then(|value| strip_injected_prompt_command(value.to_str()?))
    {
        if stripped.is_empty() {
            remove.push("PROMPT_COMMAND");
        } else {
            set.push(("PROMPT_COMMAND", stripped));
        }
    }
    (remove, set)
}

pub(crate) fn scrub_inherited_agent_session_env(cmd: &mut CommandBuilder) {
    for name in INHERITED_AGENT_SESSION_ENV_VARS
        .iter()
        .chain(INHERITED_GUARD_ENV_VARS)
    {
        cmd.env_remove(name);
    }
    let (remove, set) =
        injected_shell_env_plan(|name| cmd.get_env(name).map(std::ffi::OsStr::to_os_string));
    for name in remove {
        cmd.env_remove(name);
    }
    for (name, value) in set {
        cmd.env(name, value);
    }
}

/// Same scrub for detached `std::process::Command` children (`herdr run`
/// jobs and similar non-pty spawns).
pub(crate) fn scrub_inherited_agent_session_env_process(command: &mut std::process::Command) {
    for name in INHERITED_AGENT_SESSION_ENV_VARS
        .iter()
        .chain(INHERITED_GUARD_ENV_VARS)
    {
        command.env_remove(name);
    }
    let (remove, set) = injected_shell_env_plan(|name| {
        command
            .get_envs()
            .find(|(key, _)| key.to_str() == Some(name))
            .and_then(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
    });
    for name in remove {
        command.env_remove(name);
    }
    for (name, value) in set {
        command.env(name, value);
    }
}

/// Drop inherited agent-session markers from the running process itself,
/// e.g. a server that was started inside an agent session. Call before
/// spawning worker threads so no thread reads the environment mid-scrub.
pub(crate) fn scrub_inherited_agent_session_env_process_env() {
    for name in INHERITED_AGENT_SESSION_ENV_VARS
        .iter()
        .chain(INHERITED_GUARD_ENV_VARS)
    {
        std::env::remove_var(name);
    }
    let (remove, set) = injected_shell_env_plan(|name| std::env::var_os(name));
    for name in remove {
        std::env::remove_var(name);
    }
    for (name, value) in set {
        std::env::set_var(name, value);
    }
}

pub(crate) fn apply_pane_base_env(cmd: &mut CommandBuilder) {
    cmd.env(crate::api::SOCKET_PATH_ENV_VAR, crate::api::socket_path());
    cmd.env(crate::api::SOCKET_PATH_EXPLICIT_ENV_VAR, "1");
}

pub(crate) fn pi_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])?
            .join("extensions"),
    )
}

pub(crate) fn omp_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".omp", "agent"])?
            .join("extensions"),
    )
}

pub(crate) fn claude_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CLAUDE_CONFIG_DIR_ENV_VAR, &[".claude"])
}

pub(crate) fn codex_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CODEX_HOME_ENV_VAR, &[".codex"])
}

pub(crate) fn kimi_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(KIMI_CODE_HOME_ENV_VAR, &[".kimi-code"])
}

pub(crate) fn copilot_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(COPILOT_HOME_ENV_VAR, &[".copilot"])
}

pub(crate) fn devin_dir() -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value)).map(|path| path.join("devin"));
    }

    Ok(home_dir()?.join(".config").join("devin"))
}

pub(crate) fn droid_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".factory"))
}

pub(crate) fn config_dir_from_env_or_home(
    env_var: &str,
    home_relative_segments: &[&str],
) -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(env_var).filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value));
    }

    let mut path = home_dir()?;
    for segment in home_relative_segments {
        path.push(segment);
    }
    Ok(path)
}

pub(crate) fn expand_tilde_path(path: PathBuf) -> io::Result<PathBuf> {
    let Some(raw) = path.to_str() else {
        return Ok(path);
    };

    if raw == "~" {
        return home_dir();
    }

    if let Some(rest) = raw
        .strip_prefix("~/")
        .or_else(|| raw.strip_prefix("~\\"))
        .or_else(|| raw.strip_prefix('~'))
    {
        return Ok(home_dir()?.join(rest));
    }

    Ok(path)
}

pub(crate) fn opencode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/opencode"))
}

pub(crate) fn kilo_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/kilo"))
}

pub(crate) fn hermes_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".hermes"))
}

pub(crate) fn hermes_plugin_dir() -> io::Result<PathBuf> {
    Ok(hermes_dir()?
        .join("plugins")
        .join(super::HERMES_PLUGIN_INSTALL_NAME))
}

pub(crate) fn qodercli_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(QODERCLI_CONFIG_DIR_ENV_VAR, &[".qoder"])
}

pub(crate) fn cursor_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CURSOR_CONFIG_DIR_ENV_VAR, &[".cursor"])
}

pub(crate) fn mastracode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".mastracode"))
}

pub(crate) fn home_dir() -> io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }

    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(profile));
        }
        if let (Some(drive), Some(path)) = (
            std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty()),
            std::env::var_os("HOMEPATH").filter(|value| !value.is_empty()),
        ) {
            let mut home = PathBuf::from(drive);
            home.push(path);
            return Ok(home);
        }
    }

    Err(io::Error::other(
        "home directory is not set; cannot locate home directory",
    ))
}

#[cfg(test)]
pub(crate) fn integration_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_inherited_agent_session_markers_but_keeps_config() {
        let mut command = CommandBuilder::new("/bin/sh");
        for name in [
            "AI_AGENT",
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_EXECPATH",
            "CODEX_THREAD_ID",
            "HERDR_AGENT",
        ] {
            command.env(name, "leaked");
        }
        command.env(CLAUDE_CONFIG_DIR_ENV_VAR, "/tmp/claude-config");
        command.env(CODEX_HOME_ENV_VAR, "/tmp/codex-home");
        command.env("CLAUDE_EFFORT", "medium");

        scrub_inherited_agent_session_env(&mut command);

        for name in [
            "AI_AGENT",
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_EXECPATH",
            "CODEX_THREAD_ID",
            "HERDR_AGENT",
        ] {
            assert!(command.get_env(name).is_none(), "{name} must be scrubbed");
        }
        assert_eq!(
            command
                .get_env(CLAUDE_CONFIG_DIR_ENV_VAR)
                .and_then(std::ffi::OsStr::to_str),
            Some("/tmp/claude-config")
        );
        assert_eq!(
            command
                .get_env(CODEX_HOME_ENV_VAR)
                .and_then(std::ffi::OsStr::to_str),
            Some("/tmp/codex-home")
        );
        assert_eq!(
            command
                .get_env("CLAUDE_EFFORT")
                .and_then(std::ffi::OsStr::to_str),
            Some("medium")
        );
    }

    #[test]
    fn scrub_removes_inherited_guard_state_but_preserves_user_shell_env() {
        let guard_root = crate::guard::guard_root();
        let mut command = CommandBuilder::new("/bin/zsh");
        for name in INHERITED_GUARD_ENV_VARS {
            command.env(name, "leaked");
        }
        command.env("BASH_ENV", guard_root.join("guard.bash"));
        command.env("ZDOTDIR", "/tmp/other-namespace/guard/zdotdir");
        command.env(
            "PROMPT_COMMAND",
            format!(
                "my-prompt\n{GUARD_PIN_PATH_PREFIX}|\"$HERDR_GUARD_SHIM_DIR\":*) ;; *) export PATH=\"$HERDR_GUARD_SHIM_DIR:$PATH\";; esac\n{GUARD_ENFORCING_EXPORT}\n{GUARD_READY_WRITE}"
            ),
        );

        scrub_inherited_agent_session_env(&mut command);

        for name in INHERITED_GUARD_ENV_VARS {
            assert!(command.get_env(name).is_none(), "{name} must be scrubbed");
        }
        assert!(command.get_env("BASH_ENV").is_none());
        assert!(command.get_env("ZDOTDIR").is_none());
        assert_eq!(
            command
                .get_env("PROMPT_COMMAND")
                .and_then(std::ffi::OsStr::to_str),
            Some("my-prompt")
        );

        // User-owned values pointing outside the guard directory survive.
        let mut user = CommandBuilder::new("/bin/zsh");
        user.env("BASH_ENV", "/etc/profile.d/custom.sh");
        user.env("ZDOTDIR", "/home/user/.zsh");
        user.env("PROMPT_COMMAND", "my-prompt");
        scrub_inherited_agent_session_env(&mut user);
        assert_eq!(
            user.get_env("BASH_ENV").and_then(std::ffi::OsStr::to_str),
            Some("/etc/profile.d/custom.sh")
        );
        assert_eq!(
            user.get_env("ZDOTDIR").and_then(std::ffi::OsStr::to_str),
            Some("/home/user/.zsh")
        );
        assert_eq!(
            user.get_env("PROMPT_COMMAND")
                .and_then(std::ffi::OsStr::to_str),
            Some("my-prompt")
        );
    }

    #[test]
    fn pane_base_env_marks_parent_socket_as_explicit() {
        let mut command = CommandBuilder::new("/bin/sh");

        apply_pane_base_env(&mut command);

        assert!(command.get_env(crate::api::SOCKET_PATH_ENV_VAR).is_some());
        assert_eq!(
            command
                .get_env(crate::api::SOCKET_PATH_EXPLICIT_ENV_VAR)
                .and_then(std::ffi::OsStr::to_str),
            Some("1")
        );
    }
}
