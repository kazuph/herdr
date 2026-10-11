//! Command classifier ported from `~/dotfiles/.ai_guard.zsh` /
//! `.config/shell/ai-guard-common.sh` / `.ai_guard_config.zsh`.
//!
//! The port mirrors the ai_guard `_ai_guard_dispatch` AI-session path:
//!
//! 1. protected pattern in the command line -> `block`
//! 2. `herdr` wildcard send -> `block` (cannot be approval-overridden)
//! 3. `git wt list` -> `block`
//! 4. git history rewrite (`commit --amend`, `push --force`, `reset`) -> `block`
//! 5. `git push` evaluation -> `allow` / `block` / `ask`
//! 6. read-only `gh` subcommands -> `allow`
//! 7. `gh pr create` evaluation -> `allow` / `block` (AI sessions never prompt)
//! 8. `gh pr merge` -> `allow` (ai_guard's Slack audit is a UX concern, not a
//!    judgement; the command itself is allowed without a prompt)
//! 9. `_ai_guard_need_prompt` equivalent -> `ask`
//! 10. otherwise -> `allow`
//!
//! `ask` verdicts additionally run the ai_guard day/temp-approval check: a
//! valid approval key suppresses the ask into `allow` exactly like
//! `_ai_guard_confirm_with_temp_approval` does.
//!
//! Human-session-only rules from ai_guard (the `git checkout -b` block and the
//! `gh pr create` "prompt" outcome for humans) are intentionally not ported:
//! this classifier only ever evaluates agent panes.

use std::path::{Path, PathBuf};

/// Outcome of a guard judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// ai_guard would run the command without prompting.
    Allow,
    /// ai_guard would create a confirmation request (dialog/Slack).
    Ask,
    /// ai_guard would stop the command immediately; approval cannot override.
    Block,
}

/// Result of judging one command.
#[derive(Debug, Clone)]
pub struct Judgment {
    pub verdict: Verdict,
    /// Stable rule identifier for tests and comparisons, e.g. "git-push-main".
    pub rule: &'static str,
    /// Human readable reason (mirrors the ai_guard block/prompt wording).
    pub reason: String,
    /// ai_guard temp-approval key this command maps to, when derivable.
    pub approval_key: Option<String>,
    /// Set when `verdict` is `allow` only because a valid day/temp approval
    /// suppressed an `ask`. Holds the approval key that matched.
    pub suppressed_by: Option<String>,
}

impl Judgment {
    fn allow(rule: &'static str) -> Self {
        Self {
            verdict: Verdict::Allow,
            rule,
            reason: String::new(),
            approval_key: None,
            suppressed_by: None,
        }
    }

    fn block(rule: &'static str, reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Block,
            rule,
            reason: reason.into(),
            approval_key: None,
            suppressed_by: None,
        }
    }

    fn ask(rule: &'static str, reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Ask,
            rule,
            reason: reason.into(),
            approval_key: None,
            suppressed_by: None,
        }
    }
}

/// Filesystem access needed by the judgement, abstracted for tests.
pub trait GuardFs {
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    /// Resolve symlinks when the path exists (`${var:A}` in zsh).
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

pub(crate) struct SystemFs;

impl GuardFs for SystemFs {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// Runs helper commands (`git`, `gh`) the judgement needs, mirroring the
/// `builtin command git|gh ... 2>/dev/null` shell-outs in ai_guard.
pub trait CommandRunner {
    /// Returns trimmed stdout when the command exits successfully, `None`
    /// otherwise (spawn failure or non-zero exit).
    fn output(&self, argv: &[&str], cwd: &Path) -> Option<String>;
}

pub(crate) struct SystemCommandRunner {
    /// `PATH` to hand to helper commands. Must already exclude the guard shim
    /// directory so `git`/`gh` resolve to the real binaries.
    pub sanitized_path: String,
}

impl CommandRunner for SystemCommandRunner {
    fn output(&self, argv: &[&str], cwd: &Path) -> Option<String> {
        let (program, args) = argv.split_first()?;
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("PATH", &self.sanitized_path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// ai_guard temp-approval store (`key<TAB>expiry_epoch` lines).
pub trait ApprovalStore {
    fn is_valid(&self, key: &str, now: i64) -> bool;
}

pub(crate) struct FileApprovalStore {
    pub path: PathBuf,
}

impl ApprovalStore for FileApprovalStore {
    /// Mirrors `_ai_guard_temp_approval_is_valid` but read-only: expired-entry
    /// pruning is ai_guard's housekeeping and is intentionally not replicated.
    fn is_valid(&self, key: &str, now: i64) -> bool {
        if key.is_empty() {
            return false;
        }
        let Ok(content) = std::fs::read_to_string(&self.path) else {
            return false;
        };
        content.lines().any(|line| {
            let mut parts = line.split('\t');
            let (Some(entry_key), Some(entry_expiry), _) =
                (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            let Ok(expiry) = entry_expiry.trim().parse::<i64>() else {
                return false;
            };
            expiry > now && entry_key == key
        })
    }
}

/// Write one approval entry (`key<TAB>expiry_epoch`) to the ai_guard-format
/// file, mirroring `_ai_guard_temp_approval_set`: expired entries and entries
/// for the same key are dropped, the new entry is appended, and the file is
/// kept mode 600 so approvals do not leak to other users.
pub(crate) fn write_approval_entry(
    path: &Path,
    key: &str,
    expiry_epoch: i64,
    now: i64,
) -> std::io::Result<()> {
    let mut kept = Vec::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let mut parts = line.split('\t');
            let (Some(entry_key), Some(entry_expiry)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(expiry) = entry_expiry.trim().parse::<i64>() else {
                continue;
            };
            if expiry > now && entry_key != key {
                kept.push(format!("{entry_key}\t{expiry}"));
            }
        }
    }
    kept.push(format!("{key}\t{expiry_epoch}"));
    let mut output = kept.join("\n");
    output.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, output)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Seconds from `now` to the end of the local day (`86400 - now % 86400` in
/// local time), used for the `1日許可` expiry. Mirrors ai_guard's
/// `$(($(date -j $(date +%Y-%m-%d) -v+1d +%s) - $(date +%s)))` behaviour; on
/// non-Unix hosts where `localtime` is unavailable it falls back to a full
/// day so a granted approval never outlives the day it was meant for.
pub(crate) fn seconds_until_end_of_local_day(now: i64) -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: single-threaded read of localtime for a valid epoch; the
        // returned pointer is only read within this call.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&now as *const libc::time_t, &mut tm).is_null() {
                let remaining =
                    (24_i64 - tm.tm_hour as i64) * 3600 - tm.tm_min as i64 * 60 - tm.tm_sec as i64;
                return remaining.max(1);
            }
        }
    }
    let _ = now;
    86_400
}

/// `_ai_guard_day_approval_key` for an explicit cwd (`day:<cwd>`).
pub(crate) fn day_approval_key_for(cwd: &Path) -> String {
    format!("day:{}", cwd.to_string_lossy())
}

/// Everything the judgement needs for one command.
pub struct JudgeInput<'a> {
    /// Full argv, `argv[0]` is the command name.
    pub argv: &'a [String],
    /// Working directory of the command (`pwd -P` equivalent).
    pub cwd: &'a Path,
    /// Directory the agent was launched from (ai_guard's PPID cwd). `None`
    /// means unknown, which behaves like ai_guard's empty launch dir.
    pub launch_dir: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub tmpdir: Option<&'a Path>,
    pub fs: &'a dyn GuardFs,
    pub runner: &'a dyn CommandRunner,
    pub approvals: &'a dyn ApprovalStore,
    pub now: i64,
    pub danger_words: &'a [String],
    pub protected_patterns: &'a [String],
    pub wrangler_allow: &'a [String],
    /// ai_guard `AI_GUARD_DANGER_WORD_ACK=1`: skip `need_prompt` entirely.
    pub danger_word_ack: bool,
}

/// Judge a command argv and return `allow`/`ask`/`block` plus the reason.
pub(crate) fn judge_argv(input: &JudgeInput<'_>) -> Judgment {
    let Some((cmd, args)) = input.argv.split_first() else {
        return Judgment::allow("empty-argv");
    };
    let cmd = cmd.as_str();
    let cmd_line = input.argv.join(" ");

    // 1. Protected pattern anywhere in the command line (highest priority).
    if check_protected_pattern(&cmd_line, input.protected_patterns) {
        return Judgment::block(
            "protected-pattern",
            "AIプロセスによる .allow-main 関連操作は完全に禁止されています",
        );
    }

    // 2. herdr wildcard destination send: always block, no approval override.
    if cmd == "herdr" && is_herdr_wildcard_send(args) {
        return Judgment::block(
            "herdr-wildcard-send",
            "herdr の宛先 '*' は別workspace・別project を含む全agentへ一斉配信されます（承認では解除できません）",
        );
    }

    // 3. `git wt list` is blocked; `git wt` must be used instead.
    if cmd == "git" && is_git_wt_list(&cmd_line) {
        return Judgment::block(
            "git-wt-list",
            "git wt list は実行できません。git wt をそのまま実行してください",
        );
    }

    // 4. Git history rewrites: `commit --amend`, `push --force*`, `reset`.
    if cmd == "git" {
        if let Some(judgment) = eval_git_history_rewrite(args) {
            return judgment;
        }
    }

    // 5. `git push` ownership / main-branch evaluation.
    if cmd == "git" && args.first().map(String::as_str) == Some("push") {
        return finish_with_approval(eval_git_push(args, input), input);
    }

    // 6. Read-only `gh` subcommands run without prompting.
    if cmd == "gh" && is_readonly_gh_command(args) {
        return Judgment::allow("gh-readonly");
    }

    // 7. `gh pr create` ownership evaluation (agent sessions never prompt).
    if cmd == "gh" && args.first().map(String::as_str) == Some("pr") {
        return finish_with_approval(eval_gh_pr_create(args, input), input);
    }

    // 9. `_ai_guard_need_prompt` equivalent.
    if let Some(judgment) = need_prompt(input) {
        return finish_with_approval(judgment, input);
    }

    Judgment::allow("default")
}

/// Check ai_guard day/temp approval for `ask` verdicts, mirroring
/// `_ai_guard_confirm_with_temp_approval`. Commands whose temp key is empty
/// (`use_temp_cache=0`: wildcard send, disk-destructive) bypass the approval
/// cache entirely — the day key is not consulted either.
fn finish_with_approval(mut judgment: Judgment, input: &JudgeInput<'_>) -> Judgment {
    if judgment.verdict != Verdict::Ask {
        return judgment;
    }
    let Some(temp_key) = temp_approval_key(input) else {
        return judgment;
    };
    let day_key = day_approval_key(input);
    if input.approvals.is_valid(&day_key, input.now) {
        judgment.verdict = Verdict::Allow;
        judgment.suppressed_by = Some(day_key);
        return judgment;
    }
    if input.approvals.is_valid(&temp_key, input.now) {
        judgment.verdict = Verdict::Allow;
        judgment.suppressed_by = Some(temp_key.clone());
        return judgment;
    }
    judgment.approval_key = Some(temp_key);
    judgment
}

/// Line-level judgement mirroring ai_guard's `_ai_guard_accept_line_protected`
/// for AI sessions: `git wt list` and protected patterns `block`, danger
/// words in non-git commands `ask`. Everything else returns `None`.
///
/// Called by `herdr guard line` for typed input; record-only, never blocks.
pub(crate) fn judge_line(
    cmd_line: &str,
    danger_words: &[String],
    protected_patterns: &[String],
) -> Option<Judgment> {
    if is_git_wt_list(cmd_line) {
        return Some(Judgment::block(
            "git-wt-list",
            "git wt list は実行できません。git wt をそのまま実行してください",
        ));
    }
    if check_protected_pattern(cmd_line, protected_patterns) {
        return Some(Judgment::block(
            "protected-pattern",
            "AIプロセスによる .allow-main 関連操作は完全に禁止されています",
        ));
    }
    // git lines have their own rules; danger-word line check skips them.
    let argv = tokenize_shell_line(cmd_line);
    if let Some((first, rest)) = argv.split_first() {
        if first != "git" && requires_danger_word_prompt(first, rest, danger_words) {
            return Some(Judgment::ask(
                "danger-word-line",
                "deploy/publish を含む入力行は確認対象です",
            ));
        }
    }
    None
}

/// Minimal shell-ish tokenizer for line-level checks (zsh `${(z)}`
/// approximation): splits on whitespace honoring single/double quotes and
/// backslash escapes.
pub(crate) fn tokenize_shell_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

// ---------------------------------------------------------------------------
// Protected patterns / wildcard send / wt list
// ---------------------------------------------------------------------------

/// `_ai_guard_check_protected_pattern`: substring match on the command line.
fn check_protected_pattern(cmd_line: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| cmd_line.contains(pattern.as_str()))
}

/// `_ai_guard_is_herdr_wildcard_send`: `herdr send`/`msg send`/`agent send`
/// whose first positional destination token is a literal `*`.
fn is_herdr_wildcard_send(args: &[String]) -> bool {
    let mut mode = "";
    let mut in_send = false;
    let mut skip_value = false;

    for token in args {
        let token = token.as_str();
        if in_send {
            if skip_value {
                skip_value = false;
                continue;
            }
            match token {
                "--" => {
                    skip_value = false;
                    continue;
                }
                t if (t.starts_with("--") || t.starts_with('-')) && t.contains('=') => continue,
                "--room" | "--reply-to" | "--from" | "--to" | "--label" | "--project"
                | "--caller" | "--workspace" | "--tab" | "--cwd" | "--wait" | "--timeout" => {
                    skip_value = true;
                    continue;
                }
                t if t.starts_with('-') => continue,
                "*" => return true,
                _ => return false,
            }
        }

        match (mode, token) {
            ("", "send") | ("agent", "send") | ("msg", "send") => in_send = true,
            ("", "agent") => mode = "agent",
            ("", "msg") => mode = "msg",
            _ => {}
        }
    }

    false
}

/// `_ai_guard_is_git_wt_list`: literal `git wt list` at a word boundary in the
/// command line.
fn is_git_wt_list(cmd_line: &str) -> bool {
    cmd_line == "git wt list"
        || cmd_line.starts_with("git wt list ")
        || cmd_line.ends_with(" git wt list")
        || cmd_line.contains(" git wt list ")
}

// `_ai_guard_is_checkout_b` is intentionally not ported: it is a
// human-session-only block in ai_guard and the guard only evaluates agent
// panes.

// ---------------------------------------------------------------------------
// Danger words
// ---------------------------------------------------------------------------

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `_ai_guard_contains_danger_word`: token-wise match on the lowered command
/// line after replacing every non-alphanumeric character with a space.
fn contains_danger_word(cmd_line: &str, danger_words: &[String]) -> bool {
    if cmd_line.is_empty() {
        return false;
    }
    let normalized: String = cmd_line
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect();
    normalized
        .split_whitespace()
        .any(|token| danger_words.iter().any(|word| token == word.as_str()))
}

/// `_ai_guard_requires_danger_word_prompt`: git/gh/herdr are exempt because
/// they have their own rules.
fn requires_danger_word_prompt(cmd: &str, args: &[String], danger_words: &[String]) -> bool {
    if cmd == "git" || cmd == "gh" || cmd == "herdr" {
        return false;
    }
    let mut line = String::from(cmd);
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    contains_danger_word(&line, danger_words)
}

// ---------------------------------------------------------------------------
// rm / path safety
// ---------------------------------------------------------------------------

/// `_ai_guard_is_tmp_or_artifacts_path`.
fn is_tmp_or_artifacts_path(target: &str, input: &JudgeInput<'_>) -> bool {
    if target.contains("..") {
        return false;
    }
    let abs = absolutize(target, input.cwd);
    let abs_str = abs.to_string_lossy();

    // /tmp, /private/tmp, /var/folders/* (macOS real temp) are always allowed.
    // Note `/var/folders` itself does not qualify; only paths below it.
    if abs_str == "/tmp"
        || abs_str.starts_with("/tmp/")
        || abs_str == "/private/tmp"
        || abs_str.starts_with("/private/tmp/")
        || abs_str.starts_with("/var/folders/")
        || abs_str.starts_with("/private/var/folders/")
    {
        return true;
    }
    if let Some(tmpdir) = input.tmpdir {
        let tmpdir = tmpdir.to_string_lossy().trim_end_matches('/').to_string();
        if abs_str == tmpdir || abs_str.starts_with(&format!("{tmpdir}/")) {
            return true;
        }
    }
    abs_str.contains("/.artifacts/") || abs_str.ends_with("/.artifacts")
}

/// `_ai_guard_all_rm_paths_tmp_or_artifacts`: every non-flag argument must be
/// inside tmp/.artifacts; zero path arguments is not safe.
fn all_rm_paths_tmp_or_artifacts(args: &[String], input: &JudgeInput<'_>) -> bool {
    let mut has_path = false;
    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        has_path = true;
        if !is_tmp_or_artifacts_path(arg, input) {
            return false;
        }
    }
    has_path
}

/// `_ai_guard_is_safe_rm_path`.
fn is_safe_rm_path(target: &str, input: &JudgeInput<'_>) -> bool {
    if target.contains("..") {
        return false;
    }
    if is_tmp_or_artifacts_path(target, input) {
        return true;
    }
    let abs = absolutize(target, input.cwd);

    let Some(launch_dir) = input.launch_dir else {
        return false;
    };
    // Launching from $HOME (or unknown) always prompts.
    if launch_dir.as_os_str().is_empty() || input.home.is_some_and(|home| home == launch_dir) {
        return false;
    }
    let launch_git_root = input
        .runner
        .output(
            &[
                "git",
                "-C",
                &launch_dir.to_string_lossy(),
                "rev-parse",
                "--show-toplevel",
            ],
            input.cwd,
        )
        .filter(|root| !root.is_empty());
    let Some(_launch_git_root) = launch_git_root else {
        return false;
    };

    let launch_dir_resolved = resolve_path(launch_dir, input.fs);
    let abs_resolved = resolve_path(&abs, input.fs);

    // Only paths strictly below the launch directory qualify.
    if !abs_resolved.starts_with(&launch_dir_resolved) || abs_resolved == launch_dir_resolved {
        return false;
    }

    // Find the directory the target lives in.
    let mut target_dir = abs_resolved.clone();
    if input.fs.is_file(&target_dir) {
        target_dir = parent_or_root(&target_dir);
    }
    if !input.fs.is_dir(&target_dir) {
        target_dir = parent_or_root(&target_dir);
    }

    // Auto-approve only when the target itself is inside a git repository.
    input
        .runner
        .output(
            &[
                "git",
                "-C",
                &target_dir.to_string_lossy(),
                "rev-parse",
                "--show-toplevel",
            ],
            input.cwd,
        )
        .is_some_and(|root| !root.is_empty())
}

/// `_ai_guard_all_rm_paths_safe`.
fn all_rm_paths_safe(args: &[String], input: &JudgeInput<'_>) -> bool {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .all(|arg| is_safe_rm_path(arg, input))
}

fn parent_or_root(path: &Path) -> PathBuf {
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn absolutize(target: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(target);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Lexical fallback for `${var:A}` when the path does not exist: join +
/// collapse `.`/`..` without touching the filesystem.
fn resolve_path(path: &Path, fs: &dyn GuardFs) -> PathBuf {
    if let Some(resolved) = fs.canonicalize(path) {
        return resolved;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        use std::path::Component::*;
        match component {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// git rules
// ---------------------------------------------------------------------------

fn is_main_branch_name(branch: &str) -> bool {
    matches!(
        branch,
        "main" | "master" | "refs/heads/main" | "refs/heads/master"
    )
}

fn has_git_force_flag(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--force"
            || arg == "-f"
            || arg == "--force-with-lease"
            || arg.starts_with("--force-with-lease=")
    })
}

/// `_ai_guard_git_push_targets_main`.
fn git_push_targets_main(args: &[String], current_branch: &str) -> bool {
    let mut non_option_count = 0u32;
    for arg in args {
        if arg == "--" {
            break;
        }
        if arg.starts_with('-') {
            continue;
        }
        non_option_count += 1;
        // First non-option argument is the remote name.
        if non_option_count == 1 {
            continue;
        }
        let mut target: &str = arg;
        if target.contains(':') {
            target = target.rsplit(':').next().unwrap_or_default();
            if target.is_empty() || target == "HEAD" {
                target = current_branch;
            }
        } else if target == "HEAD" {
            target = current_branch;
        }
        if let Some(rest) = target.strip_prefix("refs/heads/") {
            target = rest;
        }
        if is_main_branch_name(target) {
            return true;
        }
    }
    // A push with no refspec targets the current branch.
    non_option_count <= 1 && is_main_branch_name(current_branch)
}

/// `_ai_guard_eval_git_history_rewrite`: `commit --amend`, `push --force*`,
/// `reset`. Returns `Some(block)` or `None` when the rule does not apply.
fn eval_git_history_rewrite(args: &[String]) -> Option<Judgment> {
    match args.first().map(String::as_str) {
        Some("commit") => args
            .iter()
            .any(|arg| arg == "--amend")
            .then(|| {
                Judgment::block(
                    "git-commit-amend",
                    "git commit --amend による履歴上書きは禁止です。修正は追いコミットで積んでください。",
                )
            }),
        Some("push") => has_git_force_flag(&args[1..]).then(|| {
            Judgment::block(
                "git-push-force",
                "git push --force 系は禁止です。履歴を書き換えず通常の push を使ってください。",
            )
        }),
        Some("reset") => Some(Judgment::block(
            "git-reset",
            "git reset による巻き戻しは禁止です。履歴は書き換えず、修正コミットで前に進めてください。",
        )),
        _ => None,
    }
}

/// `_ai_guard_extract_github_repo_full`: `owner/repo` from a github.com URL.
fn extract_github_repo_full(url: &str) -> Option<String> {
    let idx = url.find("github.com")?;
    let rest = &url[idx + "github.com".len()..];
    // `[:/]+` separator.
    let rest = rest.trim_start_matches([':', '/']);
    let mut segments = rest.split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// `_ai_guard_resolve_repo_full`: `gh repo view` first, URL parse fallback.
fn resolve_repo_full(remote_url: &str, input: &JudgeInput<'_>) -> Option<String> {
    if remote_url.is_empty() {
        return None;
    }
    if let Some(full) = input
        .runner
        .output(
            &[
                "gh",
                "repo",
                "view",
                remote_url,
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ],
            input.cwd,
        )
        .filter(|full| !full.is_empty())
    {
        return Some(full);
    }
    extract_github_repo_full(remote_url)
}

/// `_ai_guard_eval_git_push`.
fn eval_git_push(args: &[String], input: &JudgeInput<'_>) -> Judgment {
    let push_args = &args[1.min(args.len())..];
    let current_branch = input
        .runner
        .output(
            &["git", "symbolic-ref", "--quiet", "--short", "HEAD"],
            input.cwd,
        )
        .unwrap_or_default();

    if git_push_targets_main(push_args, &current_branch) {
        let git_root = input
            .runner
            .output(&["git", "rev-parse", "--show-toplevel"], input.cwd)
            .unwrap_or_default();
        if !git_root.is_empty() && input.fs.is_file(&Path::new(&git_root).join(".allow-main")) {
            return Judgment::allow("git-push-main-allowed");
        }
        return Judgment::block(
            "git-push-main",
            "main/master への push は AI からは禁止です。必要なら人間が手動で実行してください。",
        );
    }

    let remote_name = push_args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map_or("origin", String::as_str);

    let Some(remote_url) = input
        .runner
        .output(&["git", "remote", "get-url", remote_name], input.cwd)
    else {
        return Judgment::ask("git-push-owner-check", "push 先リモートを取得できない");
    };
    let Some(repo_full) = resolve_repo_full(&remote_url, input) else {
        return Judgment::ask("git-push-owner-check", "push 先リポジトリを特定できない");
    };
    let owner = repo_full.split('/').next().unwrap_or_default();
    let Some(my_login) = input
        .runner
        .output(&["gh", "api", "/user", "--jq", ".login"], input.cwd)
        .filter(|login| !login.is_empty())
    else {
        return Judgment::ask("git-push-owner-check", "gh 認証情報を取得できない");
    };

    if owner != my_login {
        let Some(owner_type) = input
            .runner
            .output(
                &[
                    "gh",
                    "api",
                    &format!("repos/{repo_full}"),
                    "--jq",
                    ".owner.type",
                ],
                input.cwd,
            )
            .filter(|value| !value.is_empty())
        else {
            return Judgment::ask("git-push-owner-check", "リポジトリ情報を取得できない");
        };
        let push_ok = input
            .runner
            .output(
                &[
                    "gh",
                    "api",
                    &format!("repos/{repo_full}"),
                    "--jq",
                    ".permissions.push",
                ],
                input.cwd,
            )
            .unwrap_or_default();

        if owner_type == "Organization" {
            match push_ok.as_str() {
                "true" => return Judgment::allow("git-push-org-member"),
                "false" => {
                    return Judgment::block(
                        "git-push-org-noperm",
                        format!("組織 ({owner}) への push 権限がないため禁止です。"),
                    );
                }
                _ => {}
            }
            let permission = input
                .runner
                .output(
                    &[
                        "gh",
                        "api",
                        &format!("repos/{repo_full}/collaborators/{my_login}/permission"),
                        "--jq",
                        ".permission",
                    ],
                    input.cwd,
                )
                .unwrap_or_default();
            return match permission.as_str() {
                "admin" | "maintain" | "write" => Judgment::allow("git-push-org-member"),
                "read" | "triage" => Judgment::block(
                    "git-push-org-noperm",
                    format!("組織 ({owner}) への push 権限がないため禁止です。"),
                ),
                _ => Judgment::allow("git-push-org-member"),
            };
        }
        return Judgment::block(
            "git-push-nonowned",
            format!("非所有リポジトリ ({owner}) への push は禁止です。"),
        );
    }

    Judgment::allow("git-push-owned")
}

// ---------------------------------------------------------------------------
// gh rules
// ---------------------------------------------------------------------------

/// `_ai_guard_is_readonly_gh_command`.
fn is_readonly_gh_command(args: &[String]) -> bool {
    let Some(subcmd) = args.first().map(String::as_str) else {
        return false;
    };
    let next = args.get(1).map(String::as_str).unwrap_or("");
    match subcmd {
        "alias" => next == "list",
        "auth" => next == "status",
        "cache" => next == "list",
        "config" => matches!(next, "get" | "list"),
        "gist" => matches!(next, "list" | "view"),
        "issue" => matches!(next, "list" | "status" | "view"),
        "label" => next == "list",
        "org" => next == "list",
        "pr" => matches!(next, "checks" | "diff" | "list" | "status" | "view"),
        "release" => matches!(next, "list" | "view"),
        "repo" => matches!(next, "list" | "view"),
        "run" => matches!(next, "list" | "view"),
        "search" => true,
        "workflow" => matches!(next, "list" | "view"),
        _ => false,
    }
}

/// `_ai_guard_eval_gh_pr_create`. `args` starts at `pr`.
fn eval_gh_pr_create(args: &[String], input: &JudgeInput<'_>) -> Judgment {
    if args.get(1).map(String::as_str) != Some("create") {
        return Judgment::allow("gh-pr-not-create");
    }
    let create_args = &args[2.min(args.len())..];

    // target repo: explicit --repo/-R > gh default resolution > origin URL.
    let mut target_repo = String::new();
    let mut prev = "";
    for arg in create_args {
        if prev == "--repo" || prev == "-R" {
            target_repo = arg.clone();
        }
        prev = arg;
    }
    for arg in create_args {
        if let Some(value) = arg.strip_prefix("--repo=") {
            target_repo = value.to_string();
        } else if arg.starts_with("-R") && arg != "-R" {
            target_repo = arg[2..].to_string();
        }
    }
    if target_repo.is_empty() {
        target_repo = input
            .runner
            .output(
                &[
                    "gh",
                    "repo",
                    "view",
                    "--json",
                    "nameWithOwner",
                    "--jq",
                    ".nameWithOwner",
                ],
                input.cwd,
            )
            .unwrap_or_default();
    }
    if target_repo.is_empty() {
        let origin_url = input
            .runner
            .output(&["git", "remote", "get-url", "origin"], input.cwd)
            .unwrap_or_default();
        target_repo = origin_url_to_repo_full(&origin_url);
    }

    if target_repo.is_empty() || !target_repo.contains('/') {
        return Judgment::block(
            "gh-pr-create-no-target",
            "PR の target repo を特定できません。--repo を明示してください。",
        );
    }

    let Some(my_login) = input
        .runner
        .output(&["gh", "api", "/user", "--jq", ".login"], input.cwd)
        .filter(|login| !login.is_empty())
    else {
        return Judgment::block(
            "gh-pr-create-no-auth",
            "gh 認証情報が取得できないため block。gh auth status を確認してください。",
        );
    };

    let target_owner = target_repo.split('/').next().unwrap_or_default();
    if target_owner == my_login {
        return Judgment::allow("gh-pr-create-owned");
    }

    let owner_type = input
        .runner
        .output(
            &[
                "gh",
                "api",
                &format!("repos/{target_repo}"),
                "--jq",
                ".owner.type",
            ],
            input.cwd,
        )
        .unwrap_or_default();
    if owner_type.is_empty() {
        // ai_guard: AI session -> block (human would prompt).
        return Judgment::block(
            "gh-pr-create-nonowned",
            format!("AI による他人所有リポ ({target_repo}) への PR 作成は禁止です (repo メタ情報を取得できず)。"),
        );
    }
    if owner_type == "Organization" {
        let push_ok = input
            .runner
            .output(
                &[
                    "gh",
                    "api",
                    &format!("repos/{target_repo}"),
                    "--jq",
                    ".permissions.push",
                ],
                input.cwd,
            )
            .unwrap_or_default();
        match push_ok.as_str() {
            "true" => return Judgment::allow("gh-pr-create-org-member"),
            "false" => {
                return Judgment::block(
                    "gh-pr-create-org-noperm",
                    format!("組織 ({target_owner}) への PR 作成権限がないため禁止です。"),
                );
            }
            _ => {}
        }
        let permission = input
            .runner
            .output(
                &[
                    "gh",
                    "api",
                    &format!("repos/{target_repo}/collaborators/{my_login}/permission"),
                    "--jq",
                    ".permission",
                ],
                input.cwd,
            )
            .unwrap_or_default();
        return match permission.as_str() {
            "admin" | "maintain" | "write" => Judgment::allow("gh-pr-create-org-member"),
            "read" | "triage" => Judgment::block(
                "gh-pr-create-org-noperm",
                format!("組織 ({target_owner}) への PR 作成権限がないため禁止です。"),
            ),
            _ => Judgment::allow("gh-pr-create-org-member"),
        };
    }

    // User-owned repo that is not mine: AI session -> block (human would prompt).
    Judgment::block(
        "gh-pr-create-nonowned",
        format!("AI による他人所有リポ ({target_repo}) への PR 作成は禁止です。--repo {my_login}/<name> を明示してください。"),
    )
}

/// `_ai_guard_eval_gh_pr_create` origin URL fallback:
/// `git@host:` / `https?://host/` prefixes and a trailing `.git` are stripped.
fn origin_url_to_repo_full(url: &str) -> String {
    let mut rest = url;
    if let Some(idx) = rest.find(':') {
        if rest[..idx].starts_with("git@") {
            rest = &rest[idx + 1..];
        }
    }
    for prefix in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped
                .split_once('/')
                .map(|(_, tail)| tail)
                .unwrap_or_default();
            break;
        }
    }
    rest.strip_suffix(".git").unwrap_or(rest).to_string()
}

/// `_ai_guard_extract_gh_api_method`: `-X`/`--method` handling.
fn extract_gh_api_method(args: &[String]) -> Option<String> {
    let mut prev = "";
    for arg in args {
        match arg.as_str() {
            "-X" | "--method" => {
                prev = arg;
                continue;
            }
            _ => {}
        }
        if let Some(value) = arg.strip_prefix("-X") {
            return Some(value.to_uppercase());
        }
        if let Some(value) = arg.strip_prefix("--method=") {
            return Some(value.to_uppercase());
        }
        if prev == "-X" || prev == "--method" {
            return Some(arg.to_uppercase());
        }
        prev = "";
    }
    None
}

/// `_ai_guard_extract_gh_api_endpoint`: first bare argument, skipping
/// option/value pairs.
fn extract_gh_api_endpoint(args: &[String]) -> Option<String> {
    const VALUE_OPTIONS: &[&str] = &[
        "-X",
        "--method",
        "-H",
        "--header",
        "-f",
        "--raw-field",
        "-F",
        "--field",
        "-q",
        "--jq",
        "--hostname",
        "--input",
        "--preview",
        "-R",
        "--repo",
        "--cache",
    ];
    let mut prev = "";
    for arg in args {
        if !prev.is_empty() {
            prev = "";
            continue;
        }
        if VALUE_OPTIONS.contains(&arg.as_str()) {
            prev = arg;
            continue;
        }
        if arg.starts_with("--") && arg.contains('=') {
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
}

/// `_ai_guard_is_gh_pr_review_reply_api`.
fn is_gh_pr_review_reply_api(endpoint: &str) -> bool {
    let path = normalize_api_endpoint(endpoint);
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        ["repos", _, _, "pulls", n, "comments", m, "replies"] => is_digits(n) && is_digits(m),
        ["repos", _, _, "pulls", "comments", m, "replies"] => is_digits(m),
        _ => false,
    }
}

/// `_ai_guard_is_gh_comment_update_api`.
fn is_gh_comment_update_api(endpoint: &str) -> bool {
    let path = normalize_api_endpoint(endpoint);
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        ["repos", _, _, "issues", "comments", n] | ["repos", _, _, "pulls", "comments", n] => {
            is_digits(n)
        }
        _ => false,
    }
}

fn normalize_api_endpoint(endpoint: &str) -> &str {
    endpoint
        .strip_prefix("https://api.github.com/")
        .or_else(|| endpoint.strip_prefix("http://api.github.com/"))
        .unwrap_or(endpoint)
        .trim_start_matches('/')
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `_ai_guard_is_high_risk_gh_command`. `args` starts at the gh subcommand.
fn is_high_risk_gh_command(args: &[String]) -> bool {
    let Some(subcmd) = args.first().map(String::as_str) else {
        return false;
    };
    let rest = &args[1.min(args.len())..];
    let next = rest.first().map(String::as_str).unwrap_or("");

    match subcmd {
        "api" => {
            let method = extract_gh_api_method(rest).unwrap_or_else(|| "GET".to_string());
            let endpoint = extract_gh_api_endpoint(rest).unwrap_or_default();
            match method.as_str() {
                "POST" | "PUT" | "PATCH" | "DELETE" => {
                    if method == "PATCH" && is_gh_comment_update_api(&endpoint) {
                        return false;
                    }
                    if method == "POST" && is_gh_pr_review_reply_api(&endpoint) {
                        return false;
                    }
                    true
                }
                _ => false,
            }
        }
        "auth" => matches!(next, "login" | "logout" | "refresh" | "setup-git"),
        "gist" => matches!(next, "create" | "edit" | "delete"),
        "label" => matches!(next, "clone" | "create" | "delete" | "edit"),
        "release" => matches!(
            next,
            "create" | "delete" | "delete-asset" | "edit" | "upload"
        ),
        "repo" => matches!(next, "create" | "delete" | "edit"),
        "secret" => matches!(next, "delete" | "set"),
        "variable" => matches!(next, "delete" | "set"),
        "workflow" => matches!(next, "disable" | "enable" | "run"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// HTTP / aws
// ---------------------------------------------------------------------------

/// `_ai_guard_extract_http_method_for_write`.
fn extract_http_method_for_write(args: &[String]) -> String {
    let mut method = String::new();
    let mut has_body = false;
    let mut prev = "";
    for arg in args {
        match prev {
            "-X" | "--request" | "--method" => {
                method = arg.to_uppercase();
                prev = "";
                continue;
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-urlencode" | "--json"
            | "-F" | "--form" => {
                has_body = true;
                prev = "";
                continue;
            }
            _ => {}
        }
        match arg.as_str() {
            "-X" | "--request" | "--method" => {
                prev = arg;
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-urlencode" | "--json"
            | "-F" | "--form" => {
                has_body = true;
                prev = arg;
            }
            _ => {
                if let Some(value) = arg.strip_prefix("-X") {
                    method = value.to_uppercase();
                } else if arg.starts_with("--request=") || arg.starts_with("--method=") {
                    method = arg.split('=').nth(1).unwrap_or_default().to_uppercase();
                } else if arg.starts_with("--data=")
                    || arg.starts_with("--data-raw=")
                    || arg.starts_with("--data-binary=")
                    || arg.starts_with("--data-urlencode=")
                    || arg.starts_with("--json=")
                    || arg.starts_with("--form=")
                    || arg.starts_with("-d")
                {
                    has_body = true;
                }
            }
        }
    }
    if !method.is_empty() {
        method
    } else if has_body {
        "POST".to_string()
    } else {
        "GET".to_string()
    }
}

/// `_ai_guard_extract_http_url`.
fn extract_http_url(args: &[String]) -> Option<&str> {
    args.iter().find_map(|arg| {
        (arg.starts_with("http://") || arg.starts_with("https://")).then_some(arg.as_str())
    })
}

/// `_ai_guard_is_github_api_write_url`.
fn is_github_api_write_url(method: &str, url: &str) -> bool {
    if !matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        return false;
    }
    let path = url
        .strip_prefix("https://api.github.com/")
        .or_else(|| url.strip_prefix("http://api.github.com/"))
        .unwrap_or(url);
    let path = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    // PR merge is captured explicitly.
    if matches!(
        segments.as_slice(),
        ["repos", _, _, "pulls", n, "merge"] if is_digits(n)
    ) {
        return true;
    }
    matches!(segments.as_slice(), ["repos", _, _, _, ..])
}

/// `_ai_guard_is_high_risk_http_command`.
fn is_high_risk_http_command(cmd: &str, args: &[String]) -> bool {
    if !matches!(cmd, "curl" | "wget" | "http" | "https") {
        return false;
    }
    let first = args.first().map(String::as_str).unwrap_or("");
    let method = if matches!(cmd, "http" | "https")
        && matches!(first, "POST" | "PUT" | "PATCH" | "DELETE")
    {
        first.to_string()
    } else {
        extract_http_method_for_write(args)
    };
    let Some(url) = extract_http_url(args) else {
        return false;
    };
    is_github_api_write_url(&method, url)
}

/// `_ai_guard_is_high_risk_aws_command`: any `put-*` operation name.
fn is_high_risk_aws_command(args: &[String]) -> bool {
    args.iter().any(|arg| lower(arg).starts_with("put-"))
}

// ---------------------------------------------------------------------------
// wrangler / SQL
// ---------------------------------------------------------------------------

/// `_ai_guard_is_d1_execute_command`: token scan for `wrangler...d1 execute`
/// (consecutive `d1 execute` after a `wrangler*` token).
fn is_d1_execute_command(argv: &[String]) -> bool {
    let mut seen_wrangler = false;
    let mut seen_d1 = false;
    for token in argv {
        let token = token.rsplit('/').next().unwrap_or(token.as_str());
        if token == "wrangler" || token.starts_with("wrangler@") {
            seen_wrangler = true;
            seen_d1 = false;
            continue;
        }
        if !seen_wrangler {
            continue;
        }
        if token == "d1" {
            seen_d1 = true;
            continue;
        }
        if seen_d1 && token == "execute" {
            return true;
        }
        if seen_d1 {
            seen_d1 = false;
        }
    }
    false
}

/// `_ai_guard_is_local_wrangler_command`: any `--local` after a `wrangler*`
/// token in the args.
fn is_local_wrangler_command(argv: &[String]) -> bool {
    let mut seen_wrangler = false;
    for token in argv {
        let token = token.rsplit('/').next().unwrap_or(token.as_str());
        if token == "wrangler" || token.starts_with("wrangler@") {
            seen_wrangler = true;
            continue;
        }
        if seen_wrangler && token == "--local" {
            return true;
        }
    }
    false
}

const SQL_LEADING_KEYWORDS: &[&str] = &[
    "select", "insert", "update", "delete", "alter", "create", "drop", "truncate", "show",
    "describe", "desc", "explain", "pragma",
];

/// `_ai_guard_extract_sql_argument`: value after `--command`/`-c`/`--execute`/
/// `-e`, `--command=`/`--execute=`, or a bare arg leading with a SQL keyword.
fn extract_sql_argument(args: &[String]) -> Option<String> {
    let mut next_is_sql = false;
    for arg in args {
        if next_is_sql {
            return Some(arg.clone());
        }
        match arg.as_str() {
            "--command" | "-c" | "--execute" | "-e" => {
                next_is_sql = true;
            }
            _ => {
                if let Some(value) = arg
                    .strip_prefix("--command=")
                    .or_else(|| arg.strip_prefix("--execute="))
                {
                    return Some(value.to_string());
                }
                let arg_lower = arg.to_lowercase();
                let is_sql = SQL_LEADING_KEYWORDS.iter().any(|kw| {
                    arg_lower == *kw
                        || (arg_lower.starts_with(kw)
                            && arg_lower
                                .as_bytes()
                                .get(kw.len())
                                .is_some_and(u8::is_ascii_whitespace))
                });
                if is_sql {
                    return Some(arg.clone());
                }
            }
        }
    }
    None
}

/// `_ai_guard_sql_is_readonly`.
fn sql_is_readonly(sql: &str) -> bool {
    if sql.is_empty() {
        return false;
    }
    // Strip `--` line comments per line, then normalize whitespace.
    let mut stripped = String::new();
    for line in sql.lines() {
        stripped.push(' ');
        stripped.push_str(line.split("--").next().unwrap_or(""));
    }
    let normalized: String = stripped
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t') {
                ' '
            } else {
                ch
            }
        })
        .collect();

    // Strip leading /* block comments */ (ai_guard's loop handles leading
    // comments only; an unterminated comment is treated as not read-only).
    let mut normalized = normalized.trim_start().to_string();
    while let Some(rest) = normalized.strip_prefix("/*") {
        let Some(end) = rest.find("*/") else {
            return false;
        };
        normalized = rest[end + 2..].trim_start().to_string();
    }

    // Multi-statement check: remaining `;` after trimming trailing ones.
    let without_trailing = normalized
        .trim_end_matches(|ch: char| ch.is_ascii_whitespace() || ch == ';')
        .to_string();
    if without_trailing.contains(';') {
        return false;
    }

    let first_word = without_trailing
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_uppercase();
    match first_word.as_str() {
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" => true,
        "PRAGMA" => {
            // `PRAGMA name = value` is a write.
            if without_trailing.contains('=') {
                return false;
            }
            let pragma_rest = without_trailing["PRAGMA".len()..]
                .trim_start()
                .to_lowercase();
            let pragma_name: String = pragma_rest
                .chars()
                .take_while(|ch| ch.is_ascii_lowercase() || *ch == '_')
                .collect();
            matches!(
                pragma_name.as_str(),
                "table_info"
                    | "table_list"
                    | "table_xinfo"
                    | "database_list"
                    | "index_list"
                    | "index_info"
                    | "index_xinfo"
                    | "foreign_key_list"
                    | "foreign_key_check"
                    | "quick_check"
                    | "integrity_check"
                    | "compile_options"
                    | "page_count"
                    | "page_size"
                    | "freelist_count"
                    | "data_version"
                    | "schema_version"
                    | "collation_list"
                    | "function_list"
                    | "module_list"
                    | "pragma_list"
            )
        }
        _ => false,
    }
}

/// `_ai_guard_sql_command_needs_prompt`.
fn sql_command_needs_prompt(cmd: &str, args: &[String], argv: &[String]) -> bool {
    // Help-only invocations get a pass.
    match cmd {
        "wrangler" if args == ["d1", "execute", "--help"] => return false,
        "npx" if args == ["wrangler", "d1", "execute", "--help"] => return false,
        _ => {}
    }

    let joined = format!(" {} ", args.join(" "));
    if is_d1_execute_command(argv) {
        if joined.contains(" --file ") || joined.contains(" --file=") {
            return true;
        }
        let Some(sql) = extract_sql_argument(args) else {
            return true;
        };
        return !sql_is_readonly(&sql);
    }

    let Some(sql) = extract_sql_argument(args) else {
        return true;
    };
    !sql_is_readonly(&sql)
}

// ---------------------------------------------------------------------------
// Temp-approval keys (ai_guard day/temp approval)
// ---------------------------------------------------------------------------

/// `_ai_guard_temp_approval_scope`: git repo root or cwd.
fn temp_approval_scope(input: &JudgeInput<'_>) -> String {
    input
        .runner
        .output(&["git", "rev-parse", "--show-toplevel"], input.cwd)
        .filter(|root| !root.is_empty())
        .unwrap_or_else(|| input.cwd.to_string_lossy().to_string())
}

/// `_ai_guard_day_approval_key`.
fn day_approval_key(input: &JudgeInput<'_>) -> String {
    day_approval_key_for(input.cwd)
}

/// `_ai_guard_is_always_prompt_cmd`.
fn is_always_prompt_cmd(cmd: &str) -> bool {
    matches!(
        cmd,
        "dd" | "mkfs" | "fdisk" | "diskutil" | "format" | "parted" | "gparted"
    )
}

/// `_ai_guard_git_risky_group`.
fn git_risky_group(args: &[String]) -> Option<&'static str> {
    match args.first().map(String::as_str) {
        Some("reset" | "restore") => Some("reset-restore"),
        Some("clean") => Some("clean"),
        Some("stash") => matches!(args.get(1).map(String::as_str), Some("drop" | "clear"))
            .then_some("stash-drop-clear"),
        Some("cherry-pick") => {
            (args.get(1).map(String::as_str) == Some("--abort")).then_some("cherry-pick-abort")
        }
        Some("merge") => {
            (args.get(1).map(String::as_str) == Some("--abort")).then_some("merge-abort")
        }
        Some("push") => Some(if has_git_force_flag(&args[1..]) {
            "push-force"
        } else {
            "push-review"
        }),
        _ => None,
    }
}

/// `_ai_guard_detect_deploy_env`.
fn detect_deploy_env(cmd: &str, args: &[String]) -> &'static str {
    let mut prev = "";
    for token in args {
        if matches!(prev, "--env" | "-e" | "--stage" | "--target" | "--profile") {
            let token = token.to_lowercase();
            if token.contains("prod") || token == "production" {
                return "prod";
            }
            if token == "stg" || token == "stage" || token == "staging" {
                return "stg";
            }
            if token == "preview" || token == "pre" || token == "canary" {
                return "preview";
            }
            if token == "dev" || token == "development" || token == "local" {
                return "dev";
            }
        }
        prev = token;
    }

    let mut line = cmd.to_lowercase();
    for arg in args {
        line.push(' ');
        line.push_str(&arg.to_lowercase());
    }
    if line.contains("prod") || line.contains("production") {
        "prod"
    } else if line.contains("stg") || line.contains("stage") || line.contains("staging") {
        "stg"
    } else if line.contains("preview") || line.contains("canary") {
        "preview"
    } else if line.contains("dev") || line.contains("development") || line.contains("local") {
        "dev"
    } else {
        "unknown"
    }
}

/// `_ai_guard_temp_approval_key`.
fn temp_approval_key(input: &JudgeInput<'_>) -> Option<String> {
    let (cmd, args) = input.argv.split_first()?;

    // herdr wildcard sends and disk-destructive commands never cache.
    if cmd == "herdr" && is_herdr_wildcard_send(args) {
        return None;
    }
    if is_always_prompt_cmd(cmd) {
        return None;
    }

    let scope = temp_approval_scope(input);

    if matches!(cmd.as_str(), "trash" | "rmdir" | "rimraf" | "mv") {
        return Some(format!("delete:{cmd}:{scope}"));
    }

    if cmd == "git" {
        if let Some(group) = git_risky_group(args) {
            return Some(format!("git:{group}:{scope}"));
        }
    }

    if requires_danger_word_prompt(cmd, args, input.danger_words) {
        let deploy_env = detect_deploy_env(cmd, args);
        return Some(format!("danger:{cmd}:{deploy_env}:{scope}"));
    }

    let subcmd = args.first().map_or("-", String::as_str);
    Some(format!("{cmd}:{subcmd}:{scope}"))
}

// ---------------------------------------------------------------------------
// `_ai_guard_need_prompt`
// ---------------------------------------------------------------------------

/// `_ai_guard_need_prompt`: returns `Some(ask-judgment)` when a confirmation
/// would be shown, `None` when the command runs silently.
fn need_prompt(input: &JudgeInput<'_>) -> Option<Judgment> {
    // ai_guard `AI_GUARD_DANGER_WORD_ACK=1` skips every prompt check.
    if input.danger_word_ack {
        return None;
    }
    let (cmd, args) = input.argv.split_first()?;
    let cmd = cmd.as_str();

    // sqlite3 is local-file only; wrangler --local is a local execution.
    if cmd == "sqlite3" {
        return None;
    }
    if is_local_wrangler_command(input.argv) {
        return None;
    }

    if is_d1_execute_command(input.argv) {
        return sql_command_needs_prompt(cmd, args, input.argv)
            .then(|| Judgment::ask("d1-sql-write", "D1 への書き込み SQL は確認が必要です"));
    }
    if requires_danger_word_prompt(cmd, args, input.danger_words) {
        return Some(Judgment::ask(
            "danger-word",
            "deploy/publish 系の操作は確認が必要です",
        ));
    }

    match cmd {
        "rm" => {
            if all_rm_paths_tmp_or_artifacts(args, input) {
                return None;
            }
            if all_rm_paths_safe(args, input) {
                return None;
            }
            Some(Judgment::ask("rm-paths", "削除操作は確認が必要です"))
        }
        "rmdir" | "rimraf" | "trash" => {
            if all_rm_paths_safe(args, input) {
                return None;
            }
            Some(Judgment::ask("rm-paths", "削除操作は確認が必要です"))
        }
        "mv" => {
            let joined = args.join(" ");
            if joined.contains("backup") || joined.contains("bak") {
                Some(Judgment::ask(
                    "mv-backup",
                    "backup/bak を含む mv は確認が必要です",
                ))
            } else {
                None
            }
        }
        "dd" | "mkfs" | "fdisk" | "diskutil" | "format" | "parted" | "gparted" => Some(
            Judgment::ask("disk-destructive", "ディスク破壊系コマンドは確認が必要です"),
        ),
        "git" => {
            let next = || args.get(1).map(String::as_str);
            match args.first().map(String::as_str) {
                Some("restore") => None,
                Some("clean") => Some(Judgment::ask(
                    "git-clean",
                    "git clean は追跡外ファイルを削除します",
                )),
                Some("stash") => matches!(next(), Some("drop" | "clear")).then(|| {
                    Judgment::ask("git-stash-drop", "git stash drop/clear は確認が必要です")
                }),
                Some("cherry-pick") => (next() == Some("--abort"))
                    .then(|| Judgment::ask("git-abort", "cherry-pick --abort は確認が必要です")),
                Some("merge") => (next() == Some("--abort"))
                    .then(|| Judgment::ask("git-abort", "merge --abort は確認が必要です")),
                Some("worktree") => None,
                _ => None,
            }
        }
        "gh" => {
            if is_high_risk_gh_command(args) {
                return Some(Judgment::ask(
                    "gh-high-risk",
                    "GitHub への書き込み操作は確認が必要です",
                ));
            }
            if args.first().map(String::as_str) == Some("repo")
                && args.get(1).map(String::as_str) == Some("edit")
            {
                let padded = format!(" {} ", args.join(" "));
                if padded.contains(" --visibility ")
                    || padded.contains(" --public ")
                    || padded.contains(" --private ")
                {
                    return Some(Judgment::ask(
                        "gh-repo-visibility",
                        "リポジトリ公開設定の変更は確認が必要です",
                    ));
                }
            }
            None
        }
        "curl" | "wget" | "http" | "https" => {
            if is_high_risk_http_command(cmd, args) {
                return Some(Judgment::ask(
                    "http-github-api-write",
                    "GitHub API への書き込みは確認が必要です",
                ));
            }
            None
        }
        "aws" => {
            if is_high_risk_aws_command(args) {
                return Some(Judgment::ask("aws-put", "AWS の更新系操作は確認が必要です"));
            }
            None
        }
        "herdr" => {
            if is_herdr_wildcard_send(args) {
                // Unreachable in practice (blocked earlier); kept for parity.
                return Some(Judgment::ask(
                    "herdr-wildcard-send",
                    "herdr ワイルドカード送信",
                ));
            }
            None
        }
        "wrangler" => {
            let subcmd_pair = format!(
                "{} {}",
                args.first().map(String::as_str).unwrap_or(""),
                args.get(1).map(String::as_str).unwrap_or("")
            );
            if input.wrangler_allow.contains(&subcmd_pair) {
                return None;
            }
            None
        }
        "psql" | "mysql" => sql_command_needs_prompt(cmd, args, input.argv)
            .then(|| Judgment::ask("sql-write", "データベースへの書き込み SQL は確認が必要です")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests: ported cases from ~/dotfiles/tools/test-ai-guard-rules.zsh
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRunner<F: Fn(&[&str]) -> Option<String>>(F);
    type NoGitRunner = FakeRunner<fn(&[&str]) -> Option<String>>;

    impl<F: Fn(&[&str]) -> Option<String>> CommandRunner for FakeRunner<F> {
        fn output(&self, argv: &[&str], _cwd: &Path) -> Option<String> {
            (self.0)(argv)
        }
    }

    struct FakeApprovals {
        valid: Vec<String>,
    }

    impl ApprovalStore for FakeApprovals {
        fn is_valid(&self, key: &str, _now: i64) -> bool {
            self.valid.iter().any(|valid| valid == key)
        }
    }

    fn no_git() -> NoGitRunner {
        FakeRunner(|_| None)
    }

    fn no_approvals() -> FakeApprovals {
        FakeApprovals { valid: Vec::new() }
    }

    fn danger() -> Vec<String> {
        vec!["publish".to_string(), "deploy".to_string()]
    }

    fn protected() -> Vec<String> {
        vec![
            ".allow-main".to_string(),
            ".allow_main".to_string(),
            "allow-main".to_string(),
            "allow_main".to_string(),
        ]
    }

    fn wrangler_allow() -> Vec<String> {
        vec!["r2 put".to_string()]
    }

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    /// Judge `argv` in `/repo` with no git/gh helper output and no approvals.
    fn judge(parts: &[&str]) -> Judgment {
        let argv = v(parts);
        let cwd = PathBuf::from("/repo");
        let fs = SystemFs;
        let runner = no_git();
        let approvals = no_approvals();
        let danger = danger();
        let protected = protected();
        let wrangler = wrangler_allow();
        let input = JudgeInput {
            argv: &argv,
            cwd: &cwd,
            launch_dir: Some(Path::new("/repo")),
            home: Some(Path::new("/Users/test")),
            tmpdir: Some(Path::new("/var/folders/xx/T")),
            fs: &fs,
            runner: &runner,
            approvals: &approvals,
            now: 1_800_000_000,
            danger_words: &danger,
            protected_patterns: &protected,
            wrangler_allow: &wrangler,
            danger_word_ack: false,
        };
        judge_argv(&input)
    }

    fn assert_verdict(parts: &[&str], expected: Verdict) {
        let judgment = judge(parts);
        assert_eq!(
            judgment.verdict,
            expected,
            "{} -> {:?} (rule={} reason={})",
            parts.join(" "),
            expected,
            judgment.rule,
            judgment.reason
        );
    }

    // -- gh api rules (existing test file section) ---------------------------

    #[test]
    fn gh_api_pr_reply_post_is_exempt() {
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/pulls/1/comments/2/replies",
                "--method",
                "POST",
                "-f",
                "body=ok",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "--method",
                "POST",
                "repos/OWNER/REPO/pulls/comments/2/replies",
                "-f",
                "body=ok",
            ],
            Verdict::Allow,
        );
    }

    #[test]
    fn gh_api_pr_reply_patch_delete_is_high_risk() {
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/pulls/comments/2/replies",
                "--method",
                "PATCH",
                "-f",
                "body=updated",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/pulls/comments/2/replies",
                "--method",
                "DELETE",
            ],
            Verdict::Ask,
        );
    }

    #[test]
    fn gh_api_generic_mutations_are_high_risk() {
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/issues",
                "--method",
                "POST",
                "-f",
                "title=test",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/issues/123",
                "--method",
                "PATCH",
                "--raw-field",
                "body=updated",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/git/refs/heads/main",
                "--method",
                "PATCH",
                "--raw-field",
                "sha=abc123",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO",
                "--method",
                "PATCH",
                "--raw-field",
                "private=true",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/issues/comments/123",
                "--method",
                "DELETE",
            ],
            Verdict::Ask,
        );
    }

    #[test]
    fn gh_api_comment_body_updates_are_exempt() {
        assert_verdict(
            &[
                "gh",
                "api",
                "repos/OWNER/REPO/issues/comments/123",
                "-X",
                "PATCH",
                "--raw-field",
                "body=updated",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "gh",
                "api",
                "--method",
                "PATCH",
                "repos/OWNER/REPO/pulls/comments/456",
                "--raw-field",
                "body=updated",
            ],
            Verdict::Allow,
        );
    }

    #[test]
    fn git_restore_branch_and_pr_merge_do_not_prompt() {
        assert_verdict(&["git", "restore", "foo.txt"], Verdict::Allow);
        assert_verdict(&["git", "branch", "-d", "feature"], Verdict::Allow);
        assert_verdict(&["git", "branch", "-D", "feature"], Verdict::Allow);
        assert_verdict(&["gh", "pr", "merge", "1", "--merge"], Verdict::Allow);
    }

    // -- herdr wildcard sends -------------------------------------------------

    #[test]
    fn herdr_wildcard_sends_are_blocked() {
        for args in [
            v(&["herdr", "send", "*", "hello"]),
            v(&["herdr", "agent", "send", "*", "hello"]),
            v(&["herdr", "msg", "send", "*", "hello", "--room", "task-room"]),
            v(&["herdr", "send", "--room", "task-room", "*", "hello"]),
            v(&[
                "herdr",
                "msg",
                "send",
                "--room=task-room",
                "--from",
                "me",
                "*",
                "hello",
            ]),
            v(&["herdr", "send", "--", "*", "hello"]),
        ] {
            let judgment = judge(&args.iter().map(String::as_str).collect::<Vec<_>>());
            assert_eq!(
                judgment.verdict,
                Verdict::Block,
                "{} should block (rule={})",
                args.join(" "),
                judgment.rule
            );
        }
    }

    #[test]
    fn herdr_named_sends_and_other_commands_are_allowed() {
        assert_verdict(&["herdr", "send", "p123", "*"], Verdict::Allow);
        assert_verdict(
            &["herdr", "send", "--room", "task-room", "p123", "hello"],
            Verdict::Allow,
        );
        assert_verdict(
            &["herdr", "pane", "run", "p123", "send * everywhere"],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "herdr",
                "msg",
                "send",
                "p123",
                "hello * body",
                "--room",
                "task-room",
            ],
            Verdict::Allow,
        );
        assert_verdict(&["herdr", "pane", "send-keys", "p123", "*"], Verdict::Allow);
        assert_verdict(
            &["herdr", "send", "p123", "deploy status report"],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "herdr",
                "msg",
                "send",
                "p123",
                "publish status report",
                "--room",
                "task-room",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &["herdr", "run", "--label", "deploy-check", "--", "true"],
            Verdict::Allow,
        );
    }

    #[test]
    fn herdr_wildcard_send_has_no_temp_approval_key() {
        // The wildcard verdict must bypass the approval cache entirely: even
        // with every possible approval valid, the verdict stays `block`.
        let argv = v(&["herdr", "send", "*", "hello"]);
        let cwd = PathBuf::from("/repo");
        let fs = SystemFs;
        let runner = no_git();
        let approvals = FakeApprovals {
            valid: vec!["day:/repo".to_string()],
        };
        let danger = danger();
        let protected = protected();
        let wrangler = wrangler_allow();
        let input = JudgeInput {
            argv: &argv,
            cwd: &cwd,
            launch_dir: None,
            home: None,
            tmpdir: None,
            fs: &fs,
            runner: &runner,
            approvals: &approvals,
            now: 1_800_000_000,
            danger_words: &danger,
            protected_patterns: &protected,
            wrangler_allow: &wrangler,
            danger_word_ack: false,
        };
        let judgment = judge_argv(&input);
        assert_eq!(judgment.verdict, Verdict::Block);
    }

    #[test]
    fn day_and_temp_approvals_suppress_ask() {
        // ai_guard's finish: a valid day key (day:<cwd>) or a valid temp key
        // (danger:<cmd>:<env>:<scope>) turns ask into allow and records which
        // key suppressed the prompt.
        for (key, expected) in [
            ("day:/repo", "day:/repo"),
            ("danger:npm:unknown:/repo", "danger:npm:unknown:/repo"),
        ] {
            let argv = v(&["npm", "publish"]);
            let cwd = PathBuf::from("/repo");
            let fs = SystemFs;
            let runner = no_git();
            let approvals = FakeApprovals {
                valid: vec![key.to_string()],
            };
            let danger = danger();
            let protected = protected();
            let wrangler = wrangler_allow();
            let input = JudgeInput {
                argv: &argv,
                cwd: &cwd,
                launch_dir: None,
                home: None,
                tmpdir: None,
                fs: &fs,
                runner: &runner,
                approvals: &approvals,
                now: 1_800_000_000,
                danger_words: &danger,
                protected_patterns: &protected,
                wrangler_allow: &wrangler,
                danger_word_ack: false,
            };
            let judgment = judge_argv(&input);
            assert_eq!(judgment.verdict, Verdict::Allow, "key={key}");
            assert_eq!(judgment.suppressed_by.as_deref(), Some(expected));
        }
        // An ask with no approval left reports the temp key for caching.
        let judgment = judge(&["npm", "publish"]);
        assert_eq!(judgment.verdict, Verdict::Ask);
        assert_eq!(
            judgment.approval_key.as_deref(),
            Some("danger:npm:unknown:/repo")
        );
    }

    // -- protected patterns / git blocks ---------------------------------------

    #[test]
    fn protected_patterns_block_any_command() {
        assert_verdict(&["touch", ".allow-main"], Verdict::Block);
        assert_verdict(&["git", "status", ".allow-main"], Verdict::Block);
        assert_verdict(&["echo", "allow_main"], Verdict::Block);
    }

    #[test]
    fn git_history_rewrites_are_blocked() {
        assert_verdict(&["git", "commit", "--amend", "-m", "x"], Verdict::Block);
        assert_verdict(&["git", "push", "--force"], Verdict::Block);
        assert_verdict(&["git", "push", "--force-with-lease"], Verdict::Block);
        assert_verdict(&["git", "reset", "--hard", "HEAD~1"], Verdict::Block);
        assert_verdict(&["git", "wt", "list"], Verdict::Block);
    }

    #[test]
    fn git_push_main_is_blocked_without_allow_main() {
        let argv = v(&["git", "push"]);
        let cwd = PathBuf::from("/repo");
        let fs = SystemFs;
        // current branch = main; rev-parse root = /repo (no .allow-main file
        // on the real fs at that path).
        let runner = FakeRunner(|argv: &[&str]| match argv {
            ["git", "symbolic-ref", "--quiet", "--short", "HEAD"] => Some("main".to_string()),
            ["git", "rev-parse", "--show-toplevel"] => Some("/repo".to_string()),
            _ => None,
        });
        let approvals = no_approvals();
        let danger = danger();
        let protected = protected();
        let wrangler = wrangler_allow();
        let input = JudgeInput {
            argv: &argv,
            cwd: &cwd,
            launch_dir: None,
            home: None,
            tmpdir: None,
            fs: &fs,
            runner: &runner,
            approvals: &approvals,
            now: 1_800_000_000,
            danger_words: &danger,
            protected_patterns: &protected,
            wrangler_allow: &wrangler,
            danger_word_ack: false,
        };
        let judgment = judge_argv(&input);
        assert_eq!(judgment.verdict, Verdict::Block);
        assert_eq!(judgment.rule, "git-push-main");
    }

    // -- SQL readonly classification --------------------------------------------

    #[test]
    fn sql_readonly_classification() {
        for (sql, expected) in [
            ("SELECT * FROM users", true),
            ("select * from users", true),
            ("SELECT 1;", true),
            ("SHOW TABLES", true),
            ("DESCRIBE users", true),
            ("DESC users", true),
            ("EXPLAIN SELECT * FROM users", true),
            ("EXPLAIN QUERY PLAN SELECT * FROM users", true),
            ("PRAGMA table_info(users)", true),
            ("PRAGMA database_list", true),
            ("PRAGMA integrity_check", true),
            ("PRAGMA page_count", true),
            ("PRAGMA table_list", true),
            ("PRAGMA journal_mode = wal", false),
            ("PRAGMA user_version = 2", false),
            ("PRAGMA foreign_keys = ON", false),
            ("PRAGMA writable_schema", false),
            ("DELETE FROM users", false),
            ("UPDATE users SET name = 'test'", false),
            ("INSERT INTO users VALUES (1)", false),
            ("ALTER TABLE users ADD COLUMN age INT", false),
            ("DROP TABLE users", false),
            ("CREATE TABLE users (id INTEGER)", false),
            ("TRUNCATE TABLE users", false),
            ("SELECT 1; DELETE FROM users", false),
            ("DELETE FROM users; SELECT 1", false),
            ("SELECT 1; SELECT 2", false),
            ("/* comment */ SELECT 1", true),
            ("/* comment */ DROP TABLE users", false),
            ("-- comment\nDROP TABLE users", false),
            ("-- comment\nSELECT 1", true),
            ("/* unclosed SELECT 1", false),
            ("", false),
            ("   ", false),
        ] {
            assert_eq!(sql_is_readonly(sql), expected, "sql: {sql:?}");
        }
    }

    // -- wrangler d1 execute -----------------------------------------------------

    #[test]
    fn wrangler_d1_help_only_is_allowed() {
        assert_verdict(&["wrangler", "d1", "execute", "--help"], Verdict::Allow);
        assert_verdict(
            &["npx", "wrangler", "d1", "execute", "--help"],
            Verdict::Allow,
        );
    }

    #[test]
    fn wrangler_d1_help_beside_sql_still_asks() {
        assert_verdict(&["wrangler", "d1", "execute"], Verdict::Ask);
        assert_verdict(&["npx", "wrangler", "d1", "execute"], Verdict::Ask);
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "DELETE FROM users",
                "--help",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--file",
                "schema.sql",
                "--help",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "--help",
            ],
            Verdict::Ask,
        );
        assert_verdict(&["wrangler", "d1", "execute", "--help extra"], Verdict::Ask);
    }

    #[test]
    fn wrangler_d1_readonly_sql_is_allowed() {
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "SELECT id FROM users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "SELECT id FROM users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "SELECT 'deploy' AS operation",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "SHOW TABLES",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "DESCRIBE users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "EXPLAIN SELECT * FROM users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "PRAGMA table_info(users)",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "npx",
                "--yes",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "SELECT 1",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &["wrangler", "d1", "execute", "DB", "--command=SELECT 1"],
            Verdict::Allow,
        );
    }

    #[test]
    fn wrangler_d1_mutating_sql_asks() {
        for sql in [
            "DELETE FROM users",
            "UPDATE users SET active = 0",
            "INSERT INTO users (id) VALUES (1)",
            "ALTER TABLE users ADD COLUMN active INTEGER",
            "DROP TABLE users",
            "CREATE TABLE users (id INTEGER)",
            "TRUNCATE TABLE users",
            "SELECT 1; DELETE FROM users",
        ] {
            assert_verdict(
                &[
                    "npx",
                    "wrangler",
                    "d1",
                    "execute",
                    "DB",
                    "--remote",
                    "--command",
                    sql,
                ],
                Verdict::Ask,
            );
        }
        assert_verdict(
            &[
                "npx",
                "--yes",
                "wrangler@latest",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npx",
                "-p",
                "wrangler",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npx",
                "--package",
                "wrangler",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npm",
                "exec",
                "wrangler",
                "--",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "--config",
                "wrangler.toml",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "--config=wrangler.toml",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "--cwd",
                ".",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "--env",
                "prod",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "--profile",
                "production",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "env",
                "WRANGLER_LOG=warn",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "arch",
                "-arm64",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "PRAGMA journal_mode = wal",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--file",
                "schema.sql",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--file",
                "schema.sql",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--remote",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Ask,
        );
    }

    #[test]
    fn wrangler_local_execution_is_allowed() {
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--local",
                "--command",
                "SELECT 1",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--local",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "npx",
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--local",
                "--command",
                "DELETE FROM users",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &["wrangler", "dev", "--local", "--name", "deploy-preview"],
            Verdict::Allow,
        );
    }

    #[test]
    fn wrangler_d1_sql_comments() {
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "-- comment\nDROP TABLE users",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "/* comment */ SELECT 1",
            ],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "wrangler",
                "d1",
                "execute",
                "DB",
                "--command",
                "/* comment */ DELETE FROM users",
            ],
            Verdict::Ask,
        );
    }

    // -- sqlite3 / psql / mysql ---------------------------------------------------

    #[test]
    fn sqlite3_is_always_allowed() {
        assert_verdict(&["sqlite3", "app.db", "SELECT 1"], Verdict::Allow);
        assert_verdict(&["sqlite3", "app.db", "SHOW TABLES"], Verdict::Allow);
        assert_verdict(
            &["sqlite3", "app.db", "PRAGMA table_info(users)"],
            Verdict::Allow,
        );
        assert_verdict(
            &["sqlite3", "app.db", "EXPLAIN SELECT * FROM users"],
            Verdict::Allow,
        );
        assert_verdict(&["sqlite3", "app.db", "DELETE FROM users"], Verdict::Allow);
        assert_verdict(
            &["sqlite3", "app.db", "INSERT INTO users VALUES (1)"],
            Verdict::Allow,
        );
        assert_verdict(&["sqlite3", "app.db", "DROP TABLE users"], Verdict::Allow);
        assert_verdict(
            &["sqlite3", "app.db", "CREATE TABLE users (id INTEGER)"],
            Verdict::Allow,
        );
        assert_verdict(
            &["sqlite3", "app.db", "PRAGMA journal_mode = wal"],
            Verdict::Allow,
        );
        assert_verdict(&["sqlite3", "app.db"], Verdict::Allow);
        assert_verdict(
            &["sqlite3", "app.db", "SELECT 1; DROP TABLE users"],
            Verdict::Allow,
        );
    }

    #[test]
    fn psql_readonly_vs_write() {
        assert_verdict(
            &["psql", "DATABASE", "--command", "SELECT * FROM users"],
            Verdict::Allow,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "SELECT * FROM users"],
            Verdict::Allow,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "SHOW server_version"],
            Verdict::Allow,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "EXPLAIN SELECT * FROM users"],
            Verdict::Allow,
        );
        assert_verdict(
            &[
                "psql",
                "DATABASE",
                "--command",
                "ALTER TABLE users ADD COLUMN active integer",
            ],
            Verdict::Ask,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "DELETE FROM users"],
            Verdict::Ask,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "UPDATE users SET name = $$test$$"],
            Verdict::Ask,
        );
        assert_verdict(
            &["psql", "DATABASE", "-c", "DROP TABLE users"],
            Verdict::Ask,
        );
        assert_verdict(&["psql", "DATABASE"], Verdict::Ask);
    }

    #[test]
    fn mysql_readonly_vs_write() {
        assert_verdict(&["mysql", "-e", "SELECT * FROM users"], Verdict::Allow);
        assert_verdict(
            &["mysql", "--execute", "SELECT * FROM users"],
            Verdict::Allow,
        );
        assert_verdict(&["mysql", "--execute=SELECT * FROM users"], Verdict::Allow);
        assert_verdict(&["mysql", "-e", "SHOW DATABASES"], Verdict::Allow);
        assert_verdict(&["mysql", "-e", "DESCRIBE users"], Verdict::Allow);
        assert_verdict(&["mysql", "-e", "DELETE FROM users"], Verdict::Ask);
        assert_verdict(
            &["mysql", "-e", "INSERT INTO users VALUES (1, \"test\")"],
            Verdict::Ask,
        );
        assert_verdict(
            &["mysql", "-e", "ALTER TABLE users ADD COLUMN age INT"],
            Verdict::Ask,
        );
        assert_verdict(&["mysql", "-u", "root", "mydb"], Verdict::Ask);
    }

    // -- rm path safety (real temp tree, fake git) ---------------------------------

    struct RmFixture {
        root: PathBuf,
        managed: PathBuf,
        launch: PathBuf,
        outside: PathBuf,
        unmanaged: PathBuf,
    }

    impl RmFixture {
        fn new() -> Self {
            // NOT under a temp dir: the tmp/.artifacts rm-rule would allow
            // every path below /var/folders and make the launch-dir checks
            // vacuous. target/ is a real non-tmp location.
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "herdr-guard-rm-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or_default()
                ));
            let managed = root.join("managed");
            let launch = managed.join("started-here");
            let outside = managed.join("outside-launch");
            let unmanaged = root.join("unmanaged");
            std::fs::create_dir_all(launch.join("nested")).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::create_dir_all(&unmanaged).unwrap();
            std::fs::write(launch.join("nested").join("file.txt"), b"x").unwrap();
            std::fs::write(outside.join("file.txt"), b"x").unwrap();
            std::fs::write(unmanaged.join("file.txt"), b"x").unwrap();
            Self {
                root,
                managed,
                launch,
                outside,
                unmanaged,
            }
        }

        fn runner(&self) -> FakeRunner<impl Fn(&[&str]) -> Option<String> + '_> {
            let managed = self.managed.clone();
            FakeRunner(move |argv: &[&str]| {
                // git -C <dir> rev-parse --show-toplevel -> managed root when
                // <dir> is inside the managed tree.
                if let ["git", "-C", dir, "rev-parse", "--show-toplevel"] = argv {
                    let dir = Path::new(dir);
                    if dir == managed || dir.starts_with(&managed) {
                        return Some(managed.to_string_lossy().to_string());
                    }
                    return None;
                }
                None
            })
        }

        fn judge_with(&self, launch_dir: Option<&Path>, parts: &[&str]) -> Judgment {
            let argv = v(parts);
            let fs = SystemFs;
            let runner = self.runner();
            let approvals = no_approvals();
            let danger = danger();
            let protected = protected();
            let wrangler = wrangler_allow();
            let input = JudgeInput {
                argv: &argv,
                cwd: &self.launch,
                launch_dir,
                home: Some(Path::new("/Users/test")),
                tmpdir: Some(Path::new("/var/folders/xx/T")),
                fs: &fs,
                runner: &runner,
                approvals: &approvals,
                now: 1_800_000_000,
                danger_words: &danger,
                protected_patterns: &protected,
                wrangler_allow: &wrangler,
                danger_word_ack: false,
            };
            judge_argv(&input)
        }
    }

    impl Drop for RmFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn rm_inside_git_managed_launch_dir_is_allowed() {
        let fix = RmFixture::new();
        let launch = fix.launch.to_string_lossy().to_string();
        let file = fix
            .launch
            .join("nested/file.txt")
            .to_string_lossy()
            .to_string();
        let nested = fix.launch.join("nested").to_string_lossy().to_string();
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", &file]).verdict,
            Verdict::Allow
        );
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", "-rf", &nested])
                .verdict,
            Verdict::Allow
        );
        let _ = launch;
    }

    #[test]
    fn rm_launch_dir_itself_and_outside_paths_ask() {
        let fix = RmFixture::new();
        let launch = fix.launch.to_string_lossy().to_string();
        let outside_file = fix.outside.join("file.txt").to_string_lossy().to_string();
        let inside_file = fix
            .launch
            .join("nested/file.txt")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", &launch]).verdict,
            Verdict::Ask
        );
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", &outside_file])
                .verdict,
            Verdict::Ask
        );
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", &inside_file, &outside_file])
                .verdict,
            Verdict::Ask
        );
    }

    #[test]
    fn rm_in_unmanaged_launch_dir_asks() {
        let fix = RmFixture::new();
        let file = fix.unmanaged.join("file.txt").to_string_lossy().to_string();
        assert_eq!(
            fix.judge_with(Some(&fix.unmanaged), &["rm", &file]).verdict,
            Verdict::Ask
        );
    }

    #[test]
    fn rm_path_traversal_asks() {
        let fix = RmFixture::new();
        let traversal = format!("{}/nested/../file.txt", fix.launch.to_string_lossy());
        assert_eq!(
            fix.judge_with(Some(&fix.launch), &["rm", &traversal])
                .verdict,
            Verdict::Ask
        );
    }

    #[test]
    fn rm_tmp_and_artifacts_paths_are_allowed() {
        let tmp = std::env::temp_dir();
        let file = tmp.join(format!("herdr-guard-tmp-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let fix = RmFixture::new();
        // $TMPDIR-mapped path (the judge gets tmpdir=/var/folders/xx/T, so use
        // the real temp dir via a tmpdir-aware input instead).
        let argv = v(&["rm", &file.to_string_lossy()]);
        let fs = SystemFs;
        let runner = no_git();
        let approvals = no_approvals();
        let danger = danger();
        let protected = protected();
        let wrangler = wrangler_allow();
        let cwd = fix.launch.clone();
        let input = JudgeInput {
            argv: &argv,
            cwd: &cwd,
            launch_dir: Some(&fix.launch),
            home: Some(Path::new("/Users/test")),
            tmpdir: Some(tmp.as_path()),
            fs: &fs,
            runner: &runner,
            approvals: &approvals,
            now: 1_800_000_000,
            danger_words: &danger,
            protected_patterns: &protected,
            wrangler_allow: &wrangler,
            danger_word_ack: false,
        };
        assert_eq!(judge_argv(&input).verdict, Verdict::Allow);
        // /tmpfoo must not match the /tmp boundary.
        let argv = v(&["rm", "/tmpfoo/x"]);
        let input = JudgeInput {
            argv: &argv,
            ..input
        };
        assert_eq!(judge_argv(&input).verdict, Verdict::Ask);
        let _ = std::fs::remove_file(&file);
    }

    // -- danger words / misc ------------------------------------------------------

    #[test]
    fn danger_word_escalates_to_ask() {
        assert_verdict(&["vercel", "--prod"], Verdict::Allow); // no danger word
        assert_verdict(&["npm", "publish"], Verdict::Ask);
        assert_verdict(&["flyctl", "deploy"], Verdict::Ask);
        assert_verdict(&["wrangler", "deploy"], Verdict::Ask);
        // git/gh/herdr are exempt from the danger-word rule.
        assert_verdict(&["git", "deploy-notes"], Verdict::Allow);
    }

    #[test]
    fn disk_destructive_commands_ask() {
        assert_verdict(&["dd", "if=/dev/zero", "of=/tmp/x"], Verdict::Ask);
        assert_verdict(&["mkfs", "/dev/disk4"], Verdict::Ask);
        assert_verdict(
            &["diskutil", "eraseDisk", "JHFS+", "x", "disk4"],
            Verdict::Ask,
        );
    }

    #[test]
    fn mv_backup_asks_other_mv_allows() {
        assert_verdict(&["mv", "a", "b.backup"], Verdict::Ask);
        assert_verdict(&["mv", "a", "b"], Verdict::Allow);
    }

    #[test]
    fn wrangler_allow_pair_bypasses_prompt() {
        assert_verdict(
            &["wrangler", "r2", "put", "bucket/key", "file"],
            Verdict::Allow,
        );
    }

    #[test]
    fn judge_line_mirrors_accept_line_rules() {
        let danger = danger();
        let protected = protected();
        assert_eq!(
            judge_line("git wt list", &danger, &protected).map(|j| j.verdict),
            Some(Verdict::Block)
        );
        assert_eq!(
            judge_line("touch .allow-main", &danger, &protected).map(|j| j.verdict),
            Some(Verdict::Block)
        );
        assert_eq!(
            judge_line("npm publish", &danger, &protected).map(|j| j.verdict),
            Some(Verdict::Ask)
        );
        assert!(judge_line("git deploy --notes", &danger, &protected).is_none());
        assert!(judge_line("ls -la", &danger, &protected).is_none());
    }

    #[test]
    fn danger_word_ack_skips_prompts() {
        let argv = v(&["npm", "publish"]);
        let cwd = PathBuf::from("/repo");
        let fs = SystemFs;
        let runner = no_git();
        let approvals = no_approvals();
        let danger = danger();
        let protected = protected();
        let wrangler = wrangler_allow();
        let input = JudgeInput {
            argv: &argv,
            cwd: &cwd,
            launch_dir: None,
            home: None,
            tmpdir: None,
            fs: &fs,
            runner: &runner,
            approvals: &approvals,
            now: 1_800_000_000,
            danger_words: &danger,
            protected_patterns: &protected,
            wrangler_allow: &wrangler,
            danger_word_ack: true,
        };
        assert_eq!(judge_argv(&input).verdict, Verdict::Allow);
    }
}
