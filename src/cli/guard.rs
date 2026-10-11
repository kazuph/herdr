//! `herdr guard` — record-only command guard CLI.
//!
//! Subcommands:
//!
//! - `herdr guard exec -- <argv...>`: entry point used by the PATH shims and
//!   shell functions. Judges the command when the pane marker exists, appends
//!   a JSONL record, then always executes the real command and propagates its
//!   exit status.
//! - `herdr guard line -- <line>`: called by the zsh accept-line hook with the
//!   typed buffer. Records line-level judgements (protected patterns,
//!   `git wt list`, danger words) without touching execution.
//! - `herdr guard check -- <argv...>`: judge one command and print the verdict
//!   as JSON (for manual verification and tests).
//! - `herdr guard log [--lines N]`: print recent JSONL records.
//! - `herdr guard compare`: match herdr records against ai_guard log entries
//!   for the same command and report agreement.

use std::path::{Path, PathBuf};

use crate::guard::judge::{self, JudgeInput};
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

/// Current `PATH` with this pane's guard shim directory removed so helper
/// commands (`git`, `gh`) and the wrapped command resolve to real binaries.
/// The directory is per-pane and named by `HERDR_GUARD_SHIM_DIR`, which the
/// pane launch env always sets alongside the PATH prepend.
fn sanitize_path() -> String {
    let shim = std::env::var(crate::guard::SHIM_DIR_ENV_VAR)
        .unwrap_or_else(|_| crate::guard::shim_dir().to_string_lossy().to_string());
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|component| *component != shim)
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
    }
    // The guard never stops a command in this phase: always run the real one.
    exec_real_command(argv, &ctx.sanitized_path)
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
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "enabled": config.enabled,
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
