//! `herdr guard` — command guard CLI.
//!
//! Subcommands:
//!
//! - `herdr guard exec -- <argv...>`: entry point used by the PATH shims and
//!   shell functions. Judges the command when the pane marker exists, appends
//!   a JSONL record, then applies `[guard] mode`: `record` always runs the
//!   real command; `enforce` stops `block` verdicts and waits on a
//!   `command_guard` decision for `ask` verdicts before running or refusing.
//! - `herdr guard line -- <line>`: called by the zsh accept-line hook with the
//!   typed buffer. Records line-level judgements (protected patterns,
//!   `git wt list`, danger words) without touching execution.
//! - `herdr guard check -- <argv...>`: judge one command and print the verdict
//!   as JSON (for manual verification and tests).
//! - `herdr guard log [--lines N]`: print recent JSONL records.
//! - `herdr guard compare`: match herdr records against ai_guard log entries
//!   for the same command and report agreement.

use std::path::{Path, PathBuf};

use crate::api::schema::{
    Decision, DecisionCreateParams, DecisionKind, DecisionOrigin, DecisionStatus, Method, Request,
};
use crate::config::GuardModeConfig;
use crate::guard::enforce;
use crate::guard::judge::{self, CommandRunner, JudgeInput};
use crate::guard::marker::{self, AgentMarker};
use crate::guard::record::{self, DecisionRecord, Via};

const GUARD_USAGE: &str = "usage:
  herdr guard exec -- <cmd> [args...]
  herdr guard line -- <command line>
  herdr guard check -- <cmd> [args...] [--cwd DIR] [--launch-dir DIR]
  herdr guard log [--lines N] [--json]
  herdr guard compare [--guard-log PATH] [--ai-guard-log PATH] [--window SEC] [--since EPOCH]
  herdr guard status";

pub(super) fn run_guard_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        eprintln!("{GUARD_USAGE}");
        return Ok(2);
    };

    match subcommand {
        "exec" => guard_exec(&args[1..]),
        "line" => guard_line(&args[1..]),
        "check" => guard_check(&args[1..]),
        "log" => guard_log(&args[1..]),
        "compare" => guard_compare(&args[1..]),
        "status" => guard_status(&args[1..]),
        "help" | "--help" | "-h" => {
            eprintln!("{GUARD_USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{GUARD_USAGE}");
            Ok(2)
        }
    }
}

/// Split `args` on a bare `--`; returns (flags, argv).
fn split_cmd_args(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|arg| arg == "--") {
        Some(idx) => (&args[..idx], &args[idx + 1..]),
        None => (args, &[]),
    }
}

struct GuardContext {
    config: crate::config::GuardConfig,
    marker: Option<(PathBuf, AgentMarker)>,
    cwd: PathBuf,
    sanitized_path: String,
}

fn load_context() -> GuardContext {
    let loaded = crate::config::Config::load();
    let marker = std::env::var(crate::guard::MARKER_ENV_VAR)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .and_then(|path| marker::read(&path).map(|marker| (path, marker)));
    let sanitized_path = sanitize_path();
    GuardContext {
        config: loaded.config.guard,
        marker,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        sanitized_path,
    }
}

/// Current `PATH` with every guard shim directory removed so helper
/// commands (`git`, `gh`) and the wrapped command resolve to real binaries.
/// The pane's own directory is named by `HERDR_GUARD_SHIM_DIR`, but a process
/// can inherit the shim `PATH` prepend without that variable (an env scrub on
/// a spawned job, for example). Matching only the named directory then leaves
/// the pane shim first in `PATH`, the "real" command resolves back to the
/// shim, and `herdr guard exec` re-enters itself without end. Dropping every
/// directory under the shim root closes that loop regardless of the env.
fn sanitize_path() -> String {
    let shim = std::env::var(crate::guard::SHIM_DIR_ENV_VAR).ok();
    let root = crate::guard::shim_dir();
    sanitize_path_from(
        &std::env::var("PATH").unwrap_or_default(),
        shim.as_deref(),
        &root,
    )
}

fn sanitize_path_from(path: &str, shim: Option<&str>, root: &Path) -> String {
    path.split(':')
        .filter(|component| {
            if Some(*component) == shim {
                return false;
            }
            let component = Path::new(component);
            !(component == root || component.starts_with(root))
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn build_judge_input<'a>(
    ctx: &'a GuardContext,
    argv: &'a [String],
    approvals: &'a judge::FileApprovalStore,
    runner: &'a judge::SystemCommandRunner,
    fs: &'a judge::SystemFs,
    danger_words: &'a [String],
    protected_patterns: &'a [String],
    wrangler_allow: &'a [String],
    launch_dir: Option<&'a Path>,
    home: Option<&'a Path>,
    tmpdir: Option<&'a Path>,
) -> JudgeInput<'a> {
    JudgeInput {
        argv,
        cwd: &ctx.cwd,
        launch_dir,
        home,
        tmpdir,
        fs,
        runner,
        approvals,
        now: record::now_epoch(),
        danger_words,
        protected_patterns,
        wrangler_allow,
        danger_word_ack: std::env::var("AI_GUARD_DANGER_WORD_ACK")
            .map(|value| value == "1")
            .unwrap_or(false),
    }
}

fn env_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    (
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("TMPDIR").map(PathBuf::from),
    )
}

fn env_context_ids() -> (Option<String>, Option<String>, Option<String>) {
    (
        std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|v| !v.is_empty()),
        std::env::var("HERDR_WORKSPACE_ID")
            .ok()
            .filter(|v| !v.is_empty()),
        std::env::var("HERDR_TAB_ID").ok().filter(|v| !v.is_empty()),
    )
}

fn guard_exec(args: &[String]) -> std::io::Result<i32> {
    let (_, argv) = split_cmd_args(args);
    if argv.is_empty() {
        eprintln!("usage: herdr guard exec -- <cmd> [args...]");
        return Ok(2);
    }

    let ctx = load_context();
    if let Some((_, marker)) = &ctx.marker {
        let approvals = judge::FileApprovalStore {
            path: crate::guard::approval_file_path(&ctx.config),
        };
        let runner = judge::SystemCommandRunner {
            sanitized_path: ctx.sanitized_path.clone(),
        };
        let fs = judge::SystemFs;
        let danger_words = crate::guard::danger_words(&ctx.config);
        let protected_patterns = crate::guard::protected_patterns(&ctx.config);
        let wrangler_allow = crate::guard::wrangler_allow(&ctx.config);
        let launch_dir = marker.cwd.as_deref().map(Path::new);
        let (home, tmpdir) = env_paths();
        let input = build_judge_input(
            &ctx,
            argv,
            &approvals,
            &runner,
            &fs,
            &danger_words,
            &protected_patterns,
            &wrangler_allow,
            launch_dir,
            home.as_deref(),
            tmpdir.as_deref(),
        );
        let judgment = judge::judge_argv(&input);
        let (pane_id, workspace_id, tab_id) = env_context_ids();
        let via = match std::env::var("HERDR_GUARD_VIA").as_deref() {
            Ok("shell-func") => Via::ShellFunc,
            _ => Via::Shim,
        };
        let record = DecisionRecord::from_judgment(
            &judgment,
            argv,
            &ctx.cwd,
            launch_dir,
            pane_id,
            workspace_id,
            tab_id,
            Some(marker.agent.clone()),
            via,
            input.now,
        );
        record::append(&crate::guard::log_path(&ctx.config), &record);
        if ctx.config.enabled && ctx.config.mode == GuardModeConfig::Enforce {
            match judgment.verdict {
                judge::Verdict::Block => {
                    eprint!(
                        "{}",
                        enforce::block_message(judgment.rule, &judgment.reason, &argv.join(" "),)
                    );
                    return Ok(1);
                }
                judge::Verdict::Ask => {
                    return enforce_ask(&ctx, marker, argv, &judgment);
                }
                judge::Verdict::Allow => {}
            }
        }
    }
    exec_real_command(argv, &ctx.sanitized_path)
}

/// Enforce path for an `ask` verdict: validate the ai_guard request reason,
/// create a `command_guard` decision on this pane's server, wait for the
/// answer, then run or refuse the command exactly like ai_guard does.
///
/// Exit behaviour mirrors ai_guard: approval (or day approval) runs the
/// command and propagates its status; rejection, expiry, cancellation, a
/// missing/invalid request reason, or an unreachable server refuse the
/// command with exit code 1 and the ai_guard-style stderr wording.
fn enforce_ask(
    ctx: &GuardContext,
    marker: &AgentMarker,
    argv: &[String],
    judgment: &judge::Judgment,
) -> std::io::Result<i32> {
    let cmd_line = argv.join(" ");

    // ai_guard reads the request reason from `AI_GUARD_REQUEST_REASON`, or
    // from the variable named by `AI_GUARD_REQUEST_ENV_NAME`; it must exist
    // and contain Japanese text before any confirmation is created.
    let reason_env = enforce::request_reason_env_name();
    let raw_reason = std::env::var(&reason_env).unwrap_or_default();
    let Some(request_reason) = enforce::validated_request_reason(&raw_reason) else {
        eprint!(
            "{}",
            enforce::missing_reason_message(&cmd_line, &reason_env)
        );
        return Ok(1);
    };
    if !enforce::contains_japanese(&request_reason) {
        eprint!(
            "{}",
            enforce::non_japanese_reason_message(&cmd_line, &reason_env)
        );
        return Ok(1);
    }

    let (pane_id, _, _) = env_context_ids();
    let runner = judge::SystemCommandRunner {
        sanitized_path: ctx.sanitized_path.clone(),
    };
    let branch = runner
        .output(
            &["git", "symbolic-ref", "--quiet", "--short", "HEAD"],
            &ctx.cwd,
        )
        .filter(|value| !value.is_empty());
    let (home, _) = env_paths();
    let title = enforce::decision_title(&argv[0], &ctx.cwd, home.as_deref());
    let body = enforce::decision_body(
        &request_reason,
        &cmd_line,
        &ctx.cwd,
        branch.as_deref(),
        &marker.agent,
    );
    let response = match super::send_request(&Request {
        id: "cli:guard:decision:create".into(),
        method: Method::DecisionCreate(DecisionCreateParams {
            kind: DecisionKind::CommandGuard,
            title,
            body: Some(body),
            options: enforce::decision_options(),
            allow_text: true,
            multi_select: false,
            timeout_ms: Some(ctx.config.ask_timeout_seconds.saturating_mul(1000)),
            origin: Some(DecisionOrigin {
                pane_id: pane_id
                    .map(|value| super::normalize_pane_id(&value))
                    .or_else(|| marker.public_pane_id.clone()),
                agent: Some(marker.agent.clone()),
                cwd: Some(ctx.cwd.to_string_lossy().to_string()),
                branch: branch.clone(),
                command: Some(cmd_line.clone()),
                reason: Some(request_reason),
                ..DecisionOrigin::default()
            }),
        }),
    }) {
        Ok(response) => response,
        Err(err) if super::protocol_mismatch_was_reported(&err) => return Err(err),
        Err(err) => {
            eprint!(
                "{}",
                enforce::deny_message(&cmd_line, &format!("判断待ちを作成できませんでした: {err}"))
            );
            return Ok(1);
        }
    };
    if response.get("error").is_some() {
        eprint!(
            "{}",
            enforce::deny_message(
                &cmd_line,
                &format!("判断待ちを作成できませんでした: {}", response["error"])
            )
        );
        return Ok(1);
    }
    let Some(decision_id) = response["result"]["decision"]["decision_id"]
        .as_str()
        .map(str::to_string)
    else {
        eprint!(
            "{}",
            enforce::deny_message(
                &cmd_line,
                "判断待ちの応答に decision_id が含まれていませんでした"
            )
        );
        return Ok(1);
    };

    let response = match super::decision::wait_for_decision(&decision_id) {
        Ok(response) => response,
        Err(err) if super::protocol_mismatch_was_reported(&err) => return Err(err),
        Err(err) => {
            eprint!(
                "{}",
                enforce::deny_message(
                    &cmd_line,
                    &format!("判断待ちの回答を受け取れませんでした: {err}")
                )
            );
            return Ok(1);
        }
    };
    if response.get("error").is_some() {
        eprint!(
            "{}",
            enforce::deny_message(
                &cmd_line,
                &format!("判断待ちがエラーになりました: {}", response["error"])
            )
        );
        return Ok(1);
    }
    let Ok(decision) = serde_json::from_value::<Decision>(response["result"]["decision"].clone())
    else {
        eprint!(
            "{}",
            enforce::deny_message(&cmd_line, "判断待ちの応答を解釈できませんでした")
        );
        return Ok(1);
    };

    match decision.status {
        DecisionStatus::Answered => match decision
            .answer
            .as_ref()
            .and_then(|answer| answer.option_id.as_deref())
        {
            Some(enforce::OPTION_APPROVE) | Some(enforce::OPTION_APPROVE_DAY) => {
                grant_approval(ctx, judgment, &decision);
                eprint!(
                    "{}",
                    enforce::approved_message(&cmd_line, &approval_note(&decision))
                );
                exec_real_command(argv, &ctx.sanitized_path)
            }
            _ => {
                let detail = decision
                    .answer
                    .as_ref()
                    .and_then(|answer| answer.text.clone())
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| "却下されました".to_string());
                eprint!("{}", enforce::deny_message(&cmd_line, &detail));
                Ok(1)
            }
        },
        DecisionStatus::Expired => {
            eprint!(
                "{}",
                enforce::deny_message(
                    &cmd_line,
                    &format!(
                        "承認の判断待ちが {} 秒の期限切れで自動却下されました",
                        ctx.config.ask_timeout_seconds
                    ),
                )
            );
            Ok(1)
        }
        DecisionStatus::Cancelled => {
            eprint!(
                "{}",
                enforce::deny_message(&cmd_line, "承認の判断待ちが取り消されました")
            );
            Ok(1)
        }
        DecisionStatus::Pending => {
            eprint!(
                "{}",
                enforce::deny_message(&cmd_line, "承認の判断待ちが解決せずに終わりました")
            );
            Ok(1)
        }
    }
}

/// Label for the `✅ 承認` line: the ai_guard choice label plus the optional
/// free-text the responder typed.
fn approval_note(decision: &Decision) -> String {
    let Some(answer) = decision.answer.as_ref() else {
        return "承認".to_string();
    };
    let label = match answer.option_id.as_deref() {
        Some(enforce::OPTION_APPROVE_DAY) => "1日許可",
        _ => "承認",
    };
    match answer
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => format!("{label}: {text}"),
        None => label.to_string(),
    }
}

/// Persist the granted approval in the ai_guard-compatible file so an
/// equivalent command does not prompt again:
///
/// - `承認` writes the per-command temp key for
///   `AI_GUARD_TEMP_APPROVAL_SECONDS` (default 180s).
/// - `1日許可` writes `day:<cwd>` until the end of the local day.
///
/// Commands whose judgement carries no temp key (wildcard sends,
/// disk-destructive commands) get nothing cached, matching ai_guard.
fn grant_approval(ctx: &GuardContext, judgment: &judge::Judgment, decision: &Decision) {
    let Some(temp_key) = judgment.approval_key.clone() else {
        return;
    };
    let now = record::now_epoch();
    let path = crate::guard::approval_file_path(&ctx.config);
    let is_day = decision
        .answer
        .as_ref()
        .is_some_and(|answer| answer.option_id.as_deref() == Some(enforce::OPTION_APPROVE_DAY));
    let (key, expiry) = if is_day {
        (
            judge::day_approval_key_for(&ctx.cwd),
            now.saturating_add(judge::seconds_until_end_of_local_day(now)),
        )
    } else {
        (
            temp_key,
            now.saturating_add(enforce::temp_approval_seconds()),
        )
    };
    let _ = judge::write_approval_entry(&path, &key, expiry, now);
}

/// Run the wrapped command with the shim dir stripped from PATH, propagating
/// its exit status. `herdr` resolves to the herdr binary that injected the
/// guard (or this binary) to avoid recursion through the shim.
fn exec_real_command(argv: &[String], sanitized_path: &str) -> std::io::Result<i32> {
    let program = if argv[0] == "herdr" {
        std::env::var(crate::guard::HERDR_EXE_ENV_VAR)
            .map(PathBuf::from)
            .or_else(|_| std::env::current_exe())
            .unwrap_or_else(|_| PathBuf::from("herdr"))
    } else {
        PathBuf::from(&argv[0])
    };
    match std::process::Command::new(&program)
        .args(&argv[1..])
        .env("PATH", sanitized_path)
        .status()
    {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("herdr guard: failed to execute {program:?}: {error}");
            Ok(127)
        }
    }
}

fn guard_line(args: &[String]) -> std::io::Result<i32> {
    let (_, rest) = split_cmd_args(args);
    let line = rest.join(" ");
    if line.trim().is_empty() {
        return Ok(0);
    }

    let ctx = load_context();
    let Some((_, marker)) = &ctx.marker else {
        return Ok(0);
    };
    let danger_words = crate::guard::danger_words(&ctx.config);
    let protected_patterns = crate::guard::protected_patterns(&ctx.config);
    let Some(judgment) = judge::judge_line(&line, &danger_words, &protected_patterns) else {
        return Ok(0);
    };
    let (pane_id, workspace_id, tab_id) = env_context_ids();
    let argv = tokenize_line(&line);
    let record = DecisionRecord::from_judgment(
        &judgment,
        &argv,
        &ctx.cwd,
        marker.cwd.as_deref().map(Path::new),
        pane_id,
        workspace_id,
        tab_id,
        Some(marker.agent.clone()),
        Via::Line,
        record::now_epoch(),
    );
    record::append(&crate::guard::log_path(&ctx.config), &record);
    Ok(0)
}

/// Re-export the judge's line tokenizer for `guard line` records.
fn tokenize_line(line: &str) -> Vec<String> {
    judge::tokenize_shell_line(line)
}

fn guard_check(args: &[String]) -> std::io::Result<i32> {
    let (flags, argv) = split_cmd_args(args);
    if argv.is_empty() {
        eprintln!("usage: herdr guard check -- <cmd> [args...] [--cwd DIR] [--launch-dir DIR]");
        return Ok(2);
    }
    let mut cwd: Option<PathBuf> = None;
    let mut launch_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < flags.len() {
        match flags[i].as_str() {
            "--cwd" => {
                let Some(value) = flags.get(i + 1) else {
                    return Ok(2);
                };
                cwd = Some(PathBuf::from(value));
                i += 2;
            }
            "--launch-dir" => {
                let Some(value) = flags.get(i + 1) else {
                    return Ok(2);
                };
                launch_dir = Some(PathBuf::from(value));
                i += 2;
            }
            _ => return Ok(2),
        }
    }

    let mut ctx = load_context();
    if let Some(cwd) = cwd {
        ctx.cwd = cwd;
    }
    print_check(&ctx, argv, launch_dir.as_deref())
}

fn print_check(
    ctx: &GuardContext,
    argv: &[String],
    launch_dir: Option<&Path>,
) -> std::io::Result<i32> {
    let approvals = judge::FileApprovalStore {
        path: crate::guard::approval_file_path(&ctx.config),
    };
    let runner = judge::SystemCommandRunner {
        sanitized_path: ctx.sanitized_path.clone(),
    };
    let fs = judge::SystemFs;
    let danger_words = crate::guard::danger_words(&ctx.config);
    let protected_patterns = crate::guard::protected_patterns(&ctx.config);
    let wrangler_allow = crate::guard::wrangler_allow(&ctx.config);
    let (home, tmpdir) = env_paths();
    let input = build_judge_input(
        ctx,
        argv,
        &approvals,
        &runner,
        &fs,
        &danger_words,
        &protected_patterns,
        &wrangler_allow,
        launch_dir,
        home.as_deref(),
        tmpdir.as_deref(),
    );
    let judgment = judge::judge_argv(&input);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "verdict": judgment.verdict,
            "rule": judgment.rule,
            "reason": judgment.reason,
            "approval_key": judgment.approval_key,
            "suppressed_by": judgment.suppressed_by,
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

fn guard_log(args: &[String]) -> std::io::Result<i32> {
    let mut lines = 20usize;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lines" | "-n" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(2);
                };
                lines = value.parse().unwrap_or(20);
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            _ => return Ok(2),
        }
    }
    let loaded = crate::config::Config::load();
    let path = crate::guard::log_path(&loaded.config.guard);
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(lines);
    for line in &all[start..] {
        if json {
            println!("{line}");
            continue;
        }
        match serde_json::from_str::<record::StoredRecord>(line) {
            Ok(rec) => {
                let verdict = match rec.verdict {
                    judge::Verdict::Allow => "allow",
                    judge::Verdict::Ask => "ask  ",
                    judge::Verdict::Block => "block",
                };
                let mut context = String::new();
                if let Some(pane) = &rec.pane_id {
                    context.push_str(&format!(" pane={pane}"));
                }
                if let Some(agent) = &rec.agent {
                    context.push_str(&format!(" agent={agent}"));
                }
                if let Some(via) = &rec.via {
                    context.push_str(&format!(" via={}", via.as_str()));
                }
                println!(
                    "{}\t{}\t{}\t{}{}",
                    rec.ts_epoch, verdict, rec.rule, rec.cmd_line, context
                );
            }
            Err(_) => println!("{line}"),
        }
    }
    Ok(0)
}

fn guard_compare(args: &[String]) -> std::io::Result<i32> {
    let loaded = crate::config::Config::load();
    let config = &loaded.config.guard;
    let mut guard_log = crate::guard::log_path(config);
    let mut ai_logs: Vec<PathBuf> = Vec::new();
    let mut window: Option<i64> = None;
    let mut since: Option<i64> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--guard-log" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(2);
                };
                guard_log = PathBuf::from(value);
                i += 2;
            }
            "--ai-guard-log" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(2);
                };
                ai_logs.push(crate::guard::expand_tilde(value));
                i += 2;
            }
            "--window" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(2);
                };
                window = value.parse().ok();
                i += 2;
            }
            "--since" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(2);
                };
                since = value.parse().ok();
                i += 2;
            }
            _ => return Ok(2),
        }
    }

    if ai_logs.is_empty() {
        // Both ai_guard logs carry decisions: the confirmation log holds
        // ALLOW/REJECT/DAY_ALLOW/PROMPT_SHOWN, the security log holds
        // BLOCK*/wildcard/protected entries.
        ai_logs.push(crate::guard::ai_guard_log_path(config));
        let security_log = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".ai_guard_security.log");
        if security_log.exists() {
            ai_logs.push(security_log);
        }
    }

    Ok(crate::guard::compare::run(
        &guard_log, &ai_logs, window, since,
    ))
}

fn guard_status(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr guard status");
        return Ok(2);
    }
    let loaded = crate::config::Config::load();
    let config = &loaded.config.guard;
    let marker = std::env::var(crate::guard::MARKER_ENV_VAR)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let marker_state = marker
        .as_ref()
        .and_then(|path| marker::read(path).map(|m| (path.clone(), m)));
    let log = crate::guard::log_path(config);
    if config.enabled {
        for warning in crate::guard::coverage::warnings() {
            eprintln!("{warning}");
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "enabled": config.enabled,
            "mode": config.mode,
            "ask_timeout_seconds": config.ask_timeout_seconds,
            "log_path": log,
            "log_records": std::fs::read_to_string(&log)
                .map(|c| c.lines().count())
                .unwrap_or(0),
            "marker": marker_state.as_ref().map(|(path, m)| serde_json::json!({
                "path": path,
                "agent": m.agent,
                "cwd": m.cwd,
            })),
            "shim_dir": crate::guard::shim_dir(),
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

#[cfg(test)]
mod sanitize_path_tests {
    use super::sanitize_path_from;
    use std::path::Path;

    #[test]
    fn drops_named_and_unnamed_pane_shim_dirs() {
        let root = Path::new("/state/guard/bin");
        let path = "/state/guard/bin/p_7:/state/guard/bin/p_9:/usr/bin:/bin";
        // HERDR_GUARD_SHIM_DIR names p_9 only; p_7 (inherited prepend without
        // its variable) must still go, or the real command resolves to a shim.
        assert_eq!(
            sanitize_path_from(path, Some("/state/guard/bin/p_9"), root),
            "/usr/bin:/bin"
        );
    }

    #[test]
    fn drops_shim_dirs_when_the_shim_variable_was_scrubbed() {
        let root = Path::new("/state/guard/bin");
        let path = "/state/guard/bin/p_2467:/opt/homebrew/bin:/usr/bin";
        assert_eq!(
            sanitize_path_from(path, None, root),
            "/opt/homebrew/bin:/usr/bin"
        );
    }

    #[test]
    fn keeps_unrelated_dirs_that_share_a_prefix() {
        let root = Path::new("/state/guard/bin");
        assert_eq!(
            sanitize_path_from("/state/guard/binaries:/usr/bin", None, root),
            "/state/guard/binaries:/usr/bin"
        );
    }
}
