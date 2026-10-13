//! Command guard for agent panes.
//!
//! Ports the dotfiles `ai_guard` judgement into herdr. When a pane is spawned,
//! herdr prepends a shim directory to `PATH` and injects zsh/bash startup
//! hooks (see `assets`). The shims and shell functions call
//! `herdr guard exec -- <cmd> <args...>`, which judges the command when the
//! pane is known to run an agent and appends a JSONL record. What happens
//! next is chosen by `[guard] mode`: `record` always executes the command;
//! `enforce` (the default) stops `block` verdicts and turns `ask` verdicts
//! into a `command_guard` decision wait.

pub(crate) mod assets;
pub(crate) mod compare;
pub(crate) mod coverage;
pub(crate) mod enforce;
pub(crate) mod judge;
pub(crate) mod marker;
pub(crate) mod record;

use std::path::PathBuf;

pub(crate) use assets::apply_guard_env;

/// Commands wrapped by the guard. Mirrors ai_guard `_AI_GUARD_TARGETS`.
pub(crate) const DEFAULT_TARGETS: &[&str] = &[
    "rm",
    "rmdir",
    "rimraf",
    "trash",
    "mv",
    "dd",
    "mkfs",
    "fdisk",
    "diskutil",
    "format",
    "parted",
    "gparted",
    "git",
    "gh",
    "curl",
    "wget",
    "http",
    "https",
    "aws",
    "npm",
    "npx",
    "pnpm",
    "pnpx",
    "yarn",
    "bun",
    "bunx",
    "deno",
    "cargo",
    "env",
    "arch",
    "firebase",
    "vercel",
    "flyctl",
    "fly",
    "wrangler",
    "sqlite3",
    "psql",
    "mysql",
    "netlify",
    "railway",
    "render",
    "amplify",
    "cdk",
    "serverless",
    "sls",
    "pulumi",
    "terraform",
    "herdr",
    "touch",
    "tee",
    "cp",
    "ln",
];

/// Words that escalate a command to `ask`. Mirrors ai_guard `_AI_GUARD_DANGER_WORDS`.
pub(crate) const DEFAULT_DANGER_WORDS: &[&str] = &["publish", "deploy"];

/// Substrings that immediately `block` a command line for agents.
/// Mirrors ai_guard `_AI_GUARD_PROTECTED_PATTERNS`.
pub(crate) const DEFAULT_PROTECTED_PATTERNS: &[&str] =
    &[".allow-main", ".allow_main", "allow-main", "allow_main"];

/// `wrangler` subcommand pairs allowed without asking.
/// Mirrors ai_guard `_AI_GUARD_WRANGLER_ALLOW` (`.ai_guard_config.zsh`).
pub(crate) const DEFAULT_WRANGLER_ALLOW: &[&str] = &["r2 put"];

/// Environment variable pointing at the guard shim directory.
pub(crate) const SHIM_DIR_ENV_VAR: &str = "HERDR_GUARD_SHIM_DIR";
/// Environment variable holding the absolute path of the herdr binary the
/// shims should call.
pub(crate) const HERDR_EXE_ENV_VAR: &str = "HERDR_GUARD_HERDR";
/// Environment variable holding the per-pane agent marker file path. When the
/// file exists the pane is believed to host an agent and commands are judged.
pub(crate) const MARKER_ENV_VAR: &str = "HERDR_GUARD_MARKER";
/// Environment variable set to `1` on panes where the guard enforces
/// (`[guard] mode = "enforce"`). The dotfiles ai_guard loader checks this
/// marker and skips itself so a pane never sees both confirmation systems.
pub(crate) const ENFORCING_ENV_VAR: &str = "HERDR_GUARD_ENFORCING";
/// Environment variable recording the user's original `ZDOTDIR` (or HOME when
/// unset) so the injected zsh startup files can chain-load the real ones.
pub(crate) const ZDOTDIR_ORIG_ENV_VAR: &str = "HERDR_GUARD_ZDOTDIR_ORIG";
/// Environment variable recording the user's original `BASH_ENV` so the
/// injected bash startup file can chain-load it.
pub(crate) const BASH_ENV_ORIG_ENV_VAR: &str = "HERDR_GUARD_BASH_ENV_ORIG";

/// Root directory for guard assets and state: `<state dir>/guard`.
pub(crate) fn guard_root() -> PathBuf {
    crate::config::state_dir().join("guard")
}

/// Root directory holding per-pane shim directories prepended to `PATH`.
pub(crate) fn shim_dir() -> PathBuf {
    guard_root().join("bin")
}

/// Per-pane shim directory (`bin/<env pane id>`). Prepended to that pane's
/// `PATH`; empty while the pane has no agent marker so unmarked commands
/// cost nothing, and populated with forwarders while the marker exists.
pub(crate) fn pane_shim_dir(pane_env_id: &str) -> PathBuf {
    shim_dir().join(pane_env_id)
}

/// Injected `ZDOTDIR` holding the guard `.zshenv` that restores the
/// original `ZDOTDIR` and chain-loads the user's startup file.
pub(crate) fn zdotdir() -> PathBuf {
    guard_root().join("zdotdir")
}

/// `BASH_ENV` target file defining the bash guard functions.
pub(crate) fn bash_env_file() -> PathBuf {
    guard_root().join("guard.bash")
}

/// Directory holding per-pane agent marker files (`<env pane id>.json`).
pub(crate) fn marker_dir() -> PathBuf {
    guard_root().join("agent-panes")
}

/// Marker file for one pane, keyed by the `HERDR_PANE_ID` env id (`p_<n>`).
pub(crate) fn marker_path(pane_env_id: &str) -> PathBuf {
    marker_dir().join(format!("{pane_env_id}.json"))
}

/// Default path of the JSONL decision log.
pub(crate) fn default_log_path() -> PathBuf {
    guard_root().join("decisions.jsonl")
}

/// Resolved guard log path honoring `[guard].log_path`.
pub(crate) fn log_path(config: &crate::config::GuardConfig) -> PathBuf {
    config
        .log_path
        .as_deref()
        .map(expand_tilde)
        .unwrap_or_else(default_log_path)
}

/// Resolved approval file path honoring `[guard].approval_file`.
/// Defaults to the ai_guard temp-approval file so approvals granted through
/// ai_guard suppress `ask` verdicts identically.
pub(crate) fn approval_file_path(config: &crate::config::GuardConfig) -> PathBuf {
    if let Some(path) = config.approval_file.as_deref() {
        return expand_tilde(path);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".ai_guard_temp_approval")
}

/// Resolved ai_guard confirmation log path honoring `[guard].ai_guard_log_path`.
pub(crate) fn ai_guard_log_path(config: &crate::config::GuardConfig) -> PathBuf {
    if let Some(path) = config.ai_guard_log_path.as_deref() {
        return expand_tilde(path);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".ai_extreme_confirm.log")
}

pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    crate::worktree::expand_tilde_path(path)
}

/// Effective target list honoring `[guard].targets`.
pub(crate) fn targets(config: &crate::config::GuardConfig) -> Vec<String> {
    config.targets.clone().unwrap_or_else(|| {
        DEFAULT_TARGETS
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    })
}

/// Effective danger-word list honoring `[guard].danger_words`.
pub(crate) fn danger_words(config: &crate::config::GuardConfig) -> Vec<String> {
    config.danger_words.clone().unwrap_or_else(|| {
        DEFAULT_DANGER_WORDS
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    })
}

/// Effective protected-pattern list honoring `[guard].protected_patterns`.
pub(crate) fn protected_patterns(config: &crate::config::GuardConfig) -> Vec<String> {
    config.protected_patterns.clone().unwrap_or_else(|| {
        DEFAULT_PROTECTED_PATTERNS
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    })
}

/// Effective wrangler allow pairs honoring `[guard].wrangler_allow`.
pub(crate) fn wrangler_allow(config: &crate::config::GuardConfig) -> Vec<String> {
    config.wrangler_allow.clone().unwrap_or_else(|| {
        DEFAULT_WRANGLER_ALLOW
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    })
}
