use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api;
use crate::api::schema::{
    AgentReadParams, AgentRenameParams, AgentSendParams, AgentStartParams, AgentStatus,
    AgentTarget, EmptyParams, Method, MsgHistoryParams, MsgInboxParams, MsgSendParams, OutputMatch,
    PaneAgentState, PaneCurrentParams, PaneListParams, PaneMoveDestination, PaneMoveParams,
    PaneNotifyParams, PaneReadParams, PaneRenameParams, PaneReportAgentParams, PaneSendInputParams,
    PaneSendKeysParams, PaneSendTextParams, PaneSplitParams, PaneTarget, PaneWaitForOutputParams,
    PingParams, ReadFormat, ReadSource, Request, SplitDirection, Subscription, TabCreateParams,
    TabListParams, TabRenameParams, TabTarget, WorkspaceCreateParams, WorkspaceRenameParams,
    WorkspaceTarget,
};

const CLI_SUBMIT_DELAY: Duration = Duration::from_millis(500);
const PANE_NOTIFY_SAMPLE_CHARS: usize = 20_000;

pub enum CommandOutcome {
    Handled(i32),
    NotCli,
}

pub fn maybe_run(args: &[String]) -> std::io::Result<CommandOutcome> {
    let Some(command) = args.get(1).map(|arg| arg.as_str()) else {
        return Ok(CommandOutcome::NotCli);
    };

    let exit_code = match command {
        "server" => {
            let Some(exit_code) = run_server_command(&args[2..])? else {
                return Ok(CommandOutcome::NotCli);
            };
            exit_code
        }
        "status" => run_status_command(&args[2..])?,
        "config" => run_config_command(&args[2..])?,
        "workspace" => run_workspace_command(&args[2..])?,
        "tab" => run_tab_command(&args[2..])?,
        "agent" => run_agent_command(&args[2..])?,
        "terminal" => run_terminal_command(&args[2..])?,
        "pane" => run_pane_command(&args[2..])?,
        "send" => msg_send(&args[2..])?,
        "run" => herdr_run(&args[2..])?,
        "log" => herdr_log(&args[2..])?,
        "inbox" => msg_inbox(&args[2..])?,
        "job" => run_job_command(&args[2..])?,
        "msg" => run_msg_command(&args[2..])?,
        "wait" => run_wait_command(&args[2..])?,
        "session" => run_session_command(&args[2..])?,
        "__pane-notify-run" => pane_notify_runner(&args[2..])?,
        "__background-run" => background_runner(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    Ok(CommandOutcome::Handled(exit_code))
}

fn run_server_command(args: &[String]) -> std::io::Result<Option<i32>> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        return Ok(None);
    };

    match subcommand {
        "stop" => server_stop(&args[1..]).map(Some),
        "reload-config" => server_reload_config(&args[1..]).map(Some),
        "help" | "--help" | "-h" => {
            print_server_help();
            Ok(Some(0))
        }
        _ => {
            print_server_help();
            Ok(Some(2))
        }
    }
}

fn run_status_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        None => print_full_status(),
        Some("server") => {
            if args.len() > 1 {
                eprintln!("usage: herdr status server");
                return Ok(2);
            }
            print_server_status()
        }
        Some("client") => {
            if args.len() > 1 {
                eprintln!("usage: herdr status client");
                return Ok(2);
            }
            print_client_status();
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            print_status_help();
            Ok(0)
        }
        Some(_) => {
            print_status_help();
            Ok(2)
        }
    }
}

fn run_config_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_config_help();
        return Ok(2);
    };

    match subcommand {
        "reset-keys" => config_reset_keys(&args[1..]),
        "help" | "--help" | "-h" => {
            print_config_help();
            Ok(0)
        }
        _ => {
            print_config_help();
            Ok(2)
        }
    }
}

fn config_reset_keys(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr config reset-keys");
        return Ok(2);
    }

    let path = crate::config::config_path();
    if !path.exists() {
        println!(
            "No config file found at {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let content = std::fs::read_to_string(&path)?;
    let parsed = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "config file at {} is invalid TOML: {err}. Fix it manually or move it aside to use defaults.",
                path.display()
            );
            return Ok(1);
        }
    };
    let Some(table) = parsed.as_table() else {
        eprintln!(
            "config file at {} is invalid TOML: top-level config must be a table.",
            path.display()
        );
        return Ok(1);
    };

    if !table.contains_key("keys") {
        println!(
            "No [keys] config found in {}. Built-in v2 keybindings already apply.",
            path.display()
        );
        return Ok(0);
    }

    let (updated, removed) = crate::config::remove_keybinding_config_sections(&content);
    if !removed {
        eprintln!(
            "could not safely remove keybinding config from {} without rewriting comments; edit the file manually or remove the top-level keys setting.",
            path.display()
        );
        return Ok(1);
    }
    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "removing keybinding config would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }

    let backup_path = key_config_backup_path(&path);
    std::fs::copy(&path, &backup_path)?;
    std::fs::write(&path, updated)?;

    println!("Created backup: {}", backup_path.display());
    println!(
        "Removed [keys], [keys.indexed], and [[keys.command]] from {}.",
        path.display()
    );
    println!("Built-in v2 keybindings will apply after Herdr restarts or reloads config.");
    println!("If a Herdr server is running, run `herdr server reload-config` to apply this now.");
    println!(
        "To restore: cp {} {}",
        backup_path.display(),
        path.display()
    );
    Ok(0)
}

fn key_config_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!("{file_name}.bak-keybind-v2-{timestamp}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRuntimeStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
    },
    NotRunning,
}

fn print_full_status() -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;

    println!("client:");
    println!("  version: {}", env!("CARGO_PKG_VERSION"));
    println!("  protocol: {}", crate::server::protocol::PROTOCOL_VERSION);
    println!();
    println!("server:");
    print_server_status_body(&server, "  ");
    println!();
    println!("update:");
    println!("  restart_needed: {}", restart_needed_label(&server));

    Ok(0)
}

fn print_server_status() -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;
    print_server_status_body(&server, "");
    Ok(0)
}

fn print_client_status() {
    println!("version: {}", env!("CARGO_PKG_VERSION"));
    println!("protocol: {}", crate::server::protocol::PROTOCOL_VERSION);
    println!("binary: {}", current_exe_label());
}

fn print_server_status_body(server: &ServerRuntimeStatus, indent: &str) {
    match server {
        ServerRuntimeStatus::Running { version, protocol } => {
            println!("{indent}status: running");
            println!("{indent}version: {}", option_label(version.as_deref()));
            println!("{indent}protocol: {}", protocol_label(*protocol));
            println!("{indent}compatible: {}", compatibility_label(*protocol));
            println!("{indent}socket: {}", api::socket_path().display());
        }
        ServerRuntimeStatus::NotRunning => {
            println!("{indent}status: not running");
            println!("{indent}socket: {}", api::socket_path().display());
        }
    }
}

fn read_server_runtime_status() -> std::io::Result<ServerRuntimeStatus> {
    match send_request(&Request {
        id: "cli:status:server".into(),
        method: Method::Ping(PingParams::default()),
    }) {
        Ok(response) => {
            if response.get("error").is_some() {
                return Err(std::io::Error::other(format!(
                    "server status request failed: {}",
                    response
                )));
            }

            let result = &response["result"];
            Ok(ServerRuntimeStatus::Running {
                version: result
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                protocol: result
                    .get("protocol")
                    .and_then(|value| value.as_u64())
                    .and_then(|value| u32::try_from(value).ok()),
            })
        }
        Err(err) if server_not_running_error(&err) => Ok(ServerRuntimeStatus::NotRunning),
        Err(err) => Err(err),
    }
}

fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn option_label(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn compatibility_label(protocol: Option<u32>) -> &'static str {
    match protocol {
        Some(protocol) if protocol == crate::server::protocol::PROTOCOL_VERSION => "yes",
        Some(_) => "no",
        None => "unknown",
    }
}

fn restart_needed_label(server: &ServerRuntimeStatus) -> &'static str {
    match server {
        ServerRuntimeStatus::Running { version, protocol } => {
            if *protocol != Some(crate::server::protocol::PROTOCOL_VERSION) {
                return "yes";
            }
            match version.as_deref() {
                Some(env!("CARGO_PKG_VERSION")) => "no",
                Some(_) => "yes",
                None => "unknown",
            }
        }
        ServerRuntimeStatus::NotRunning => "no",
    }
}

fn current_exe_label() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"))
}

fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_workspace_help();
        return Ok(2);
    };

    match subcommand {
        "list" => workspace_list(&args[1..]),
        "create" => workspace_create(&args[1..]),
        "get" => workspace_get(&args[1..]),
        "focus" => workspace_focus(&args[1..]),
        "rename" => workspace_rename(&args[1..]),
        "close" => workspace_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_workspace_help();
            Ok(0)
        }
        _ => {
            print_workspace_help();
            Ok(2)
        }
    }
}

fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn run_msg_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_msg_help();
        return Ok(2);
    };

    match subcommand {
        "send" => {
            eprintln!("notice: `herdr msg send` is an alias; use `herdr send`");
            msg_send(&args[1..])
        }
        "inbox" => {
            eprintln!("notice: `herdr msg inbox` is an alias; use `herdr inbox`");
            msg_inbox(&args[1..])
        }
        "history" => {
            eprintln!("notice: `herdr msg history` is an alias; use `herdr log`");
            msg_history(&args[1..])
        }
        "tail" => {
            eprintln!("notice: `herdr msg tail` is an alias; use `herdr log -f`");
            msg_tail(&args[1..])
        }
        "rooms" => {
            eprintln!("notice: `herdr msg rooms` is an alias; use `herdr log rooms`");
            msg_rooms(&args[1..])
        }
        "help" | "--help" | "-h" => {
            print_msg_help();
            Ok(0)
        }
        _ => {
            print_msg_help();
            Ok(2)
        }
    }
}

fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_agent_help();
        return Ok(2);
    };

    match subcommand {
        "list" => agent_list(&args[1..]),
        "get" => agent_get(&args[1..]),
        "read" => agent_read(&args[1..]),
        "send" => agent_send(&args[1..]),
        "rename" => agent_rename(&args[1..]),
        "focus" => agent_focus(&args[1..]),
        "wait" => agent_wait(&args[1..]),
        "attach" => agent_attach(&args[1..]),
        "start" => agent_start(&args[1..]),
        "restore" => agent_restore(&args[1..]),
        "help" | "--help" | "-h" => {
            print_agent_help();
            Ok(0)
        }
        _ => {
            print_agent_help();
            Ok(2)
        }
    }
}

fn run_terminal_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_terminal_help();
        return Ok(2);
    };

    match subcommand {
        "attach" => terminal_attach(&args[1..]),
        "help" | "--help" | "-h" => {
            print_terminal_help();
            Ok(0)
        }
        _ => {
            print_terminal_help();
            Ok(2)
        }
    }
}

fn run_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_pane_help();
        return Ok(2);
    };

    match subcommand {
        "list" => pane_list(&args[1..]),
        "current" => pane_current(&args[1..]),
        "get" => pane_get(&args[1..]),
        "focus" => pane_focus(&args[1..]),
        "read" => pane_read(&args[1..]),
        "rename" => pane_rename(&args[1..]),
        "split" => pane_split(&args[1..]),
        "move" => pane_move(&args[1..]),
        "close" => pane_close(&args[1..]),
        "send-text" => pane_send_text(&args[1..]),
        "send-keys" => pane_send_keys(&args[1..]),
        "report-agent" => pane_report_agent(&args[1..]),
        "run" => pane_run(&args[1..]),
        "run-notify" => pane_run_notify(&args[1..]),
        "job-log" => pane_job_log(&args[1..]),
        "help" | "--help" | "-h" => {
            print_pane_help();
            Ok(0)
        }
        _ => {
            print_pane_help();
            Ok(2)
        }
    }
}

fn run_job_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") if args.len() == 1 => {
            eprintln!("notice: `herdr job list` is an alias; use `herdr run list`");
            job_list()
        }
        Some("status") if args.len() == 2 => {
            eprintln!("notice: `herdr job status` is an alias; use `herdr log <job_id>`");
            job_status(&args[1])
        }
        Some("log") if args.len() >= 2 => {
            eprintln!("notice: `herdr job log` is an alias; use `herdr log <job_id>`");
            pane_job_log(&args[1..])
        }
        Some("cancel") if args.len() == 2 => {
            eprintln!("notice: `herdr job cancel` is an alias; use `herdr run cancel <job_id>`");
            job_cancel(&args[1])
        }
        Some("help" | "--help" | "-h") => {
            print_job_help();
            Ok(0)
        }
        _ => {
            print_job_help();
            Ok(2)
        }
    }
}

fn job_list() -> std::io::Result<i32> {
    let jobs = crate::job::JobStore::open_active()
        .and_then(|store| store.list())
        .map_err(std::io::Error::other)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({ "jobs": jobs }))?
    );
    Ok(0)
}

fn job_status(job_id: &str) -> std::io::Result<i32> {
    if !valid_pane_job_id(job_id) {
        eprintln!("invalid job id: {job_id}");
        return Ok(2);
    }
    let Some(job) = crate::job::JobStore::open_active()
        .and_then(|store| store.get(job_id))
        .map_err(std::io::Error::other)?
    else {
        eprintln!("job not found: {job_id}");
        return Ok(1);
    };
    println!("{}", serde_json::to_string(&job)?);
    Ok(0)
}

fn job_cancel(job_id: &str) -> std::io::Result<i32> {
    if !valid_pane_job_id(job_id) {
        eprintln!("invalid job id: {job_id}");
        return Ok(2);
    }
    let store = crate::job::JobStore::open_active().map_err(std::io::Error::other)?;
    let Some(job) = store.get(job_id).map_err(std::io::Error::other)? else {
        eprintln!("job not found: {job_id}");
        return Ok(1);
    };
    if !matches!(job.status.as_str(), "queued" | "running") {
        eprintln!("job {job_id} is not running (status={})", job.status);
        return Ok(1);
    }
    let Some(pid) = job.runner_pid else {
        eprintln!("job {job_id} has not published its runner pid yet; retry cancel");
        return Ok(1);
    };
    let process_group = unsafe { libc::getpgid(pid as i32) };
    if process_group != pid as i32 {
        eprintln!("job {job_id} runner process group is no longer provable; refusing to signal");
        return Ok(1);
    }
    if !store
        .mark_cancelling(job_id)
        .map_err(std::io::Error::other)?
    {
        eprintln!("job {job_id} reached a terminal state before cancellation acquired it");
        return Ok(1);
    }
    signal_process_group(pid, libc::SIGTERM)?;
    let mut escalated = false;
    if !wait_for_process_group_exit(pid, crate::session::STOP_WAIT_TIMEOUT) {
        escalated = true;
        signal_process_group(pid, libc::SIGKILL)?;
        if !wait_for_process_group_exit(pid, crate::session::STOP_WAIT_TIMEOUT) {
            eprintln!(
                "job {job_id} process group {pid} survived SIGKILL; status remains cancelling"
            );
            return Ok(1);
        }
    }
    let cancelled = store
        .mark_cancelled(job_id, unix_millis(SystemTime::now()))
        .map_err(std::io::Error::other)?;
    if cancelled && job.completion != "none" {
        enqueue_job_mailbox(
            &job,
            format!(
                "[herdr run] cancelled label={} job={} details: herdr log {}",
                one_line_field(&job.label),
                job.id,
                job.id
            ),
        )?;
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "job": job_id,
            "status": "cancelled",
            "signal": if escalated { "KILL" } else { "TERM" },
        }))?
    );
    Ok(0)
}

fn signal_process_group(pid: u32, signal: i32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err);
        }
    }
    Ok(())
}

fn wait_for_process_group_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(crate::session::STOP_WAIT_POLL);
    }
}

#[derive(Debug, Serialize)]
struct RunSpawnOutput {
    job: String,
    label: String,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
}

fn herdr_run(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") if args.len() == 1 => return job_list(),
        Some("cancel") if args.len() == 2 => return job_cancel(&args[1]),
        Some("help" | "--help" | "-h") => {
            print_run_help();
            return Ok(0);
        }
        _ => {}
    }
    let mut label = None;
    let mut cwd = None;
    let mut split = SplitDirection::Down;
    let mut caller = None;
    let mut pane_mode = false;
    let mut close_on_success = false;
    let mut completion = "summary".to_string();
    let mut completion_set = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                if value.trim().is_empty() {
                    eprintln!("--label must not be empty");
                    return Ok(2);
                }
                label = Some(value.clone());
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--split" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --split");
                    return Ok(2);
                };
                split = parse_split_direction(value)?;
                index += 2;
            }
            "--pane" => {
                pane_mode = true;
                index += 1;
            }
            "--caller" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --caller");
                    return Ok(2);
                };
                caller = Some(normalize_pane_id(value));
                index += 2;
            }
            "--close-on-success" => {
                close_on_success = true;
                index += 1;
            }
            "--completion" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --completion");
                    return Ok(2);
                };
                if !matches!(value.as_str(), "summary" | "full" | "none") {
                    eprintln!("invalid --completion: {value} (expected summary, full, or none)");
                    return Ok(2);
                }
                completion = value.clone();
                completion_set = true;
                index += 2;
            }
            "--help" | "-h" | "help" => {
                print_run_help();
                return Ok(0);
            }
            "--" => {
                index += 1;
                break;
            }
            other => {
                eprintln!("unknown option before --: {other}");
                print_run_help();
                return Ok(2);
            }
        }
    }

    if index >= args.len() {
        print_run_help();
        return Ok(2);
    }

    let command_args = &args[index..];
    let label = label.unwrap_or_else(|| default_run_label(command_args));
    if !pane_mode && (args.iter().any(|arg| arg == "--split") || close_on_success) {
        eprintln!("--split and --close-on-success require --pane");
        print_run_help();
        return Ok(2);
    }
    if pane_mode && completion_set {
        eprintln!(
            "--completion applies only to pane-less background jobs; remove it or omit --pane"
        );
        return Ok(2);
    }
    let caller = match resolve_run_caller(caller.as_deref()) {
        Ok(caller) => caller,
        Err(err) => {
            eprintln!("unable to resolve caller pane: {err}");
            eprintln!("pass --caller <pane> explicitly; do not infer from focused pane");
            return Ok(1);
        }
    };

    if pane_mode {
        return herdr_run_in_pane(command_args, label, cwd, split, caller, close_on_success);
    }

    herdr_run_background(command_args, label, cwd, caller, completion)
}

fn herdr_run_in_pane(
    command_args: &[String],
    label: String,
    cwd: Option<String>,
    split: SplitDirection,
    caller: String,
    close_on_success: bool,
) -> std::io::Result<i32> {
    let command = shell_command_from_args(command_args);

    let split_response = send_request(&Request {
        id: "cli:run:split".into(),
        method: Method::PaneSplit(PaneSplitParams {
            workspace_id: None,
            target_pane_id: caller.clone(),
            direction: split,
            cwd,
            focus: false,
        }),
    })?;
    if split_response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&split_response).unwrap());
        return Ok(1);
    }

    let pane = split_response["result"]["pane"]["global_id"]
        .as_str()
        .or_else(|| split_response["result"]["pane"]["pane_id"].as_str())
        .ok_or_else(|| std::io::Error::other("pane.split response did not include pane id"))?
        .to_string();

    let rename_response = send_request(&Request {
        id: "cli:run:rename".into(),
        method: Method::PaneRename(PaneRenameParams {
            pane_id: pane.clone(),
            label: Some(label.clone()),
        }),
    })?;
    if rename_response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&rename_response).unwrap());
        return Ok(1);
    }

    let job = new_pane_job_id();
    let exe = std::env::current_exe()?;
    let mut runner_parts = vec![
        shell_quote(&exe.display().to_string()),
        "__pane-notify-run".to_string(),
        "--parent".to_string(),
        shell_quote(&caller),
        "--target".to_string(),
        shell_quote(&pane),
        "--job-id".to_string(),
        shell_quote(&job),
        "--run-label".to_string(),
        shell_quote(&label),
    ];
    if close_on_success {
        runner_parts.push("--close-on-success".to_string());
    } else {
        runner_parts.push("--close-on-exit".to_string());
    }
    runner_parts.push("--".to_string());
    runner_parts.push(shell_quote(&command));
    let runner = runner_parts.join(" ");

    let submit_response = send_pane_input_text_enter(&pane, runner)?;
    if submit_response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&submit_response).unwrap());
        return Ok(1);
    }

    println!(
        "{}",
        serde_json::to_string(&RunSpawnOutput {
            job,
            label,
            mode: "pane".into(),
            pane: Some(pane),
        })?
    );
    Ok(0)
}

fn herdr_run_background(
    command_args: &[String],
    label: String,
    cwd: Option<String>,
    caller: String,
    completion: String,
) -> std::io::Result<i32> {
    let cwd = cwd
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    if !cwd.is_dir() {
        eprintln!("--cwd is not a directory: {}", cwd.display());
        return Ok(2);
    }
    let caller_identity = resolve_agent_target(&caller, "cli:run:agent")?;
    if caller_identity.get("error").is_some() {
        eprintln!("caller pane {caller} has no exact agent identity");
        return Ok(1);
    }
    let caller_agent = caller_identity["result"]["agent"]["name"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            caller_identity["result"]["agent"]["agent"]
                .as_str()
                .map(|_| caller.clone())
        })
        .ok_or_else(|| std::io::Error::other("caller pane has no reported agent identity"))?;

    let job = new_pane_job_id();
    let log_path = pane_job_log_path(&job)?;
    let record = crate::job::JobRecord {
        id: job.clone(),
        label: label.clone(),
        command: shell_command_from_args(command_args),
        cwd: cwd.display().to_string(),
        caller_pane: caller.clone(),
        caller_agent: caller_agent.clone(),
        completion: completion.clone(),
        status: "queued".into(),
        runner_pid: None,
        exit_code: None,
        started_unix_ms: None,
        finished_unix_ms: None,
        log_path: log_path.display().to_string(),
    };
    crate::job::JobStore::open_active()
        .and_then(|store| store.insert(&record))
        .map_err(std::io::Error::other)?;

    let exe = std::env::current_exe()?;
    let mut runner = Command::new(exe);
    runner
        .arg("__background-run")
        .arg("--job-id")
        .arg(&job)
        .arg("--completion")
        .arg(&completion)
        .arg("--")
        .args(command_args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        runner.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if let Err(err) = runner.spawn() {
        let _ = crate::job::JobStore::open_active()
            .and_then(|store| store.mark_start_failed(&job, 127, unix_millis(SystemTime::now())));
        return Err(err);
    }

    println!(
        "{}",
        serde_json::to_string(&RunSpawnOutput {
            job,
            label,
            mode: "background".into(),
            pane: None,
        })?
    );
    Ok(0)
}

fn resolve_run_caller(explicit: Option<&str>) -> std::io::Result<String> {
    if let Some(pane_id) = explicit {
        let pane_id = normalize_pane_id(pane_id);
        let response = send_request(&Request {
            id: "cli:run:caller".into(),
            method: Method::PaneGet(PaneTarget {
                pane_id: pane_id.clone(),
            }),
        })?;
        if let Some(error) = response.get("error") {
            return Err(std::io::Error::other(
                serde_json::to_string(error).unwrap_or_else(|_| error.to_string()),
            ));
        }
        return Ok(response["result"]["pane"]["global_id"]
            .as_str()
            .unwrap_or(&pane_id)
            .to_string());
    }

    resolve_current_pane_id()
}

fn default_run_label(command_args: &[String]) -> String {
    command_args
        .first()
        .and_then(|command| {
            std::path::Path::new(command)
                .file_name()
                .and_then(|name| name.to_str())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("run")
        .to_string()
}

fn shell_command_from_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_wait_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_wait_help();
        return Ok(2);
    };

    match subcommand {
        "output" => wait_output(&args[1..]),
        "agent-status" => wait_agent_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_wait_help();
            Ok(0)
        }
        _ => {
            print_wait_help();
            Ok(2)
        }
    }
}

fn run_session_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_session_help();
        return Ok(2);
    };

    match subcommand {
        "list" => session_list(&args[1..]),
        "attach" => session_attach_help(&args[1..]),
        "stop" => session_stop(&args[1..]),
        "delete" => session_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_session_help();
            Ok(0)
        }
        _ => {
            print_session_help();
            Ok(2)
        }
    }
}

fn server_stop(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr server stop");
        return Ok(2);
    }

    send_ok_request(Method::ServerStop(EmptyParams::default()))
}

fn server_reload_config(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr server reload-config");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:server:reload-config".into(),
        method: Method::ServerReloadConfig(EmptyParams::default()),
    })?)
}

fn session_attach_help(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("usage: herdr session attach <name>");
        return Ok(0);
    }
    eprintln!("usage: herdr session attach <name>");
    Ok(2)
}

fn session_list(args: &[String]) -> std::io::Result<i32> {
    let json = match parse_session_json_only(args, "usage: herdr session list [--json]") {
        Ok(json) => json,
        Err(code) => return Ok(code),
    };

    let sessions = crate::session::list_sessions()?;
    if json {
        _print_json(&serde_json::json!({
            "sessions": sessions,
        }));
    } else {
        print_session_table(&sessions);
    }
    Ok(0)
}

fn session_stop(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session stop <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let target = match crate::session::parse_target_name(&name) {
        Ok(target) => target,
        Err(message) => {
            print_session_error("invalid_session_name", &message);
            return Ok(1);
        }
    };
    match crate::session::stop_session(target.as_deref()) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "stopped": true,
                    "session": session,
                }));
            } else {
                println!("stopped session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_stop_failed", &message);
            Ok(1)
        }
    }
}

fn session_delete(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session delete <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    match crate::session::delete_session(&name) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "deleted": true,
                    "session": session,
                }));
            } else {
                println!("deleted session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_delete_failed", &message);
            Ok(1)
        }
    }
}

fn workspace_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr workspace list");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:list".into(),
        method: Method::WorkspaceList(EmptyParams::default()),
    })?)
}

fn workspace_create(args: &[String]) -> std::io::Result<i32> {
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:create".into(),
        method: Method::WorkspaceCreate(WorkspaceCreateParams { cwd, focus, label }),
    })?)
}

fn workspace_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:get".into(),
        method: Method::WorkspaceGet(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:focus".into(),
        method: Method::WorkspaceFocus(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr workspace rename <workspace_id> <label>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:rename".into(),
        method: Method::WorkspaceRename(WorkspaceRenameParams {
            workspace_id: normalize_workspace_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn workspace_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace close <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace close <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:close".into(),
        method: Method::WorkspaceClose(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:tab:list".into(),
        method: Method::TabList(TabListParams { workspace_id }),
    })?)
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:tab:create".into(),
        method: Method::TabCreate(TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
        }),
    })?)
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:get".into(),
        method: Method::TabGet(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:focus".into(),
        method: Method::TabFocus(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:rename".into(),
        method: Method::TabRename(TabRenameParams {
            tab_id: normalize_tab_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:close".into(),
        method: Method::TabClose(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn agent_start(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: herdr agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
        return Ok(2);
    };

    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        eprintln!("usage: herdr agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
        return Ok(2);
    };
    if separator == args.len() - 1 {
        eprintln!("agent start requires argv after --");
        return Ok(2);
    }

    let mut cwd = None;
    let mut workspace_id = None;
    let mut tab_id = None;
    let mut split = None;
    let mut focus = false;

    let mut index = 1;
    while index < separator {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--workspace" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            "--tab" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --tab");
                    return Ok(2);
                };
                tab_id = Some(normalize_tab_id(value));
                index += 2;
            }
            "--split" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --split");
                    return Ok(2);
                };
                split = Some(parse_split_direction(value)?);
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:agent:start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: name.clone(),
            cwd,
            workspace_id,
            tab_id,
            split,
            focus,
            argv: args[separator + 1..].to_vec(),
        }),
    })?)
}

fn agent_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr agent list");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_restore(args: &[String]) -> std::io::Result<i32> {
    let mut dry_run = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            _ => {
                eprintln!("usage: herdr agent restore [--dry-run]");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:agent:restore".into(),
        method: Method::AgentRestore(crate::api::schema::AgentRestoreParams { dry_run }),
    })?)
}

fn agent_get(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_attach(args: &[String]) -> std::io::Result<i32> {
    let (target, takeover) =
        match parse_attach_target(args, "usage: herdr agent attach <target> [--takeover]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
        return Ok(2);
    };

    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_wait_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    let response = resolve_agent_target(target, "cli:agent:wait:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    if response["result"]["agent"]["agent_status"]
        .as_str()
        .is_some_and(|current| agent_wait_status_satisfied(agent_status, current))
    {
        println!("{}", serde_json::to_string(&response).unwrap());
        return Ok(0);
    }

    let Some(pane_id) = response["result"]["agent"]["pane_id"].as_str() else {
        eprintln!("agent wait failed: response did not include pane_id");
        return Ok(1);
    };

    let subscriptions = if agent_status == AgentStatus::Idle {
        vec![
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Idle),
            },
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Done),
            },
        ]
    } else {
        vec![Subscription::PaneAgentStatusChanged {
            pane_id: pane_id.to_owned(),
            agent_status: Some(agent_status),
        }]
    };

    wait_for_agent_change(
        Request {
            id: "cli:agent:wait".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions,
            }),
        },
        timeout_ms,
        "timed out waiting for agent status change",
    )
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    send_request(&Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    })
}

fn terminal_attach(args: &[String]) -> std::io::Result<i32> {
    let (terminal_id, takeover) = match parse_attach_target(
        args,
        "usage: herdr terminal attach <terminal_id> [--takeover]",
    ) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    crate::client::run_terminal_attach(terminal_id, takeover)?;
    Ok(0)
}

fn parse_attach_target(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let Some(target) = args.first() else {
        eprintln!("{usage}");
        return Err(2);
    };
    let mut takeover = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--takeover" => takeover = true,
            "help" | "--help" | "-h" => {
                eprintln!("{usage}");
                return Err(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Err(2);
            }
        }
    }
    Ok((target.clone(), takeover))
}

fn msg_send(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        print_send_help();
        return Ok(0);
    }
    if args.len() < 2 {
        eprintln!("usage: herdr send <to> <text> [--room R] [--reply-to ID] [--from NAME]");
        return Ok(2);
    }

    let to = args[0].clone();
    let mut room = crate::msg::DEFAULT_ROOM.to_string();
    let mut from = None;
    let mut reply_to = None;
    let mut text = Vec::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--room" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --room");
                    return Ok(2);
                };
                room = value.clone();
                index += 2;
            }
            "--from" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --from");
                    return Ok(2);
                };
                from = Some(value.clone());
                index += 2;
            }
            "--reply-to" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --reply-to");
                    return Ok(2);
                };
                reply_to = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| std::io::Error::other("--reply-to must be a dispatch id"))?,
                );
                index += 2;
            }
            "help" | "--help" | "-h" => {
                print_msg_help();
                return Ok(0);
            }
            value => {
                text.push(value.to_string());
                index += 1;
            }
        }
    }
    if text.is_empty() {
        eprintln!("usage: herdr send <to> <text> [--room R] [--reply-to ID] [--from NAME]");
        return Ok(2);
    }

    let identity = current_msg_identity(from)?;
    let response = send_request(&Request {
        id: "cli:msg:send".into(),
        method: Method::MsgSend(MsgSendParams {
            room,
            project: identity.project,
            from_agent: identity.agent,
            to,
            body: text.join(" "),
            reply_to,
        }),
    })?;
    print_msg_send_response(&response)
}

fn msg_inbox(args: &[String]) -> std::io::Result<i32> {
    let mut room = crate::msg::DEFAULT_ROOM.to_string();
    let mut to_agent = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--room" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --room");
                    return Ok(2);
                };
                room = value.clone();
                index += 2;
            }
            "--to" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --to");
                    return Ok(2);
                };
                to_agent = Some(value.clone());
                index += 2;
            }
            "help" | "--help" | "-h" => {
                print_msg_help();
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let identity = current_msg_identity(to_agent)?;
    let response = send_request(&Request {
        id: "cli:msg:inbox".into(),
        method: Method::MsgInbox(MsgInboxParams {
            room,
            to_agent: identity.agent,
        }),
    })?;
    print_msg_messages_response(&response, "inbox")
}

fn msg_history(args: &[String]) -> std::io::Result<i32> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        print_msg_help();
        return Ok(0);
    }
    let (room, project, limit) = parse_msg_history_args(args)?;
    let response = send_request(&Request {
        id: "cli:msg:history".into(),
        method: Method::MsgHistory(MsgHistoryParams {
            room,
            project,
            limit,
        }),
    })?;
    print_msg_messages_response(&response, "history")
}

fn msg_tail(args: &[String]) -> std::io::Result<i32> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        print_msg_help();
        return Ok(0);
    }
    let (room, project, limit) = parse_msg_history_args(args)?;
    let mut seen_max = 0_i64;
    loop {
        let response = send_request(&Request {
            id: "cli:msg:tail".into(),
            method: Method::MsgHistory(MsgHistoryParams {
                room: room.clone(),
                project: project.clone(),
                limit,
            }),
        })?;
        if let Some(error) = response.get("error") {
            eprintln!("{error}");
            return Ok(1);
        }
        let messages = response["result"]["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for message in messages {
            let id = message["id"].as_i64().unwrap_or(0);
            if id <= seen_max {
                continue;
            }
            seen_max = seen_max.max(id);
            print_msg_message(&message);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn msg_rooms(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr msg rooms");
        return Ok(2);
    }
    let response = send_request(&Request {
        id: "cli:msg:rooms".into(),
        method: Method::MsgRooms(EmptyParams::default()),
    })?;
    if let Some(error) = response.get("error") {
        eprintln!("{error}");
        return Ok(1);
    }
    for room in response["result"]["rooms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
    {
        println!("{room}");
    }
    Ok(0)
}

fn herdr_log(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("--db") if args.len() == 1 => {
            println!(
                "{}",
                crate::dispatch::DispatchStore::active_path().display()
            );
            Ok(0)
        }
        Some("--schema") if args.len() == 1 => {
            println!("{}", crate::dispatch::DispatchStore::schema());
            Ok(0)
        }
        Some("rooms") => msg_rooms(&args[1..]),
        Some("stats") => log_stats(&args[1..]),
        Some("-f") => msg_tail(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_log_help();
            Ok(0)
        }
        Some(job_id) if !job_id.starts_with('-') && valid_pane_job_id(job_id) => {
            job_status(job_id)?;
            pane_job_log(args)
        }
        _ => log_timeline(args),
    }
}

fn log_timeline(args: &[String]) -> std::io::Result<i32> {
    let (room, project, limit) = parse_msg_history_args(args)?;
    let _store = crate::dispatch::DispatchStore::open_active().map_err(std::io::Error::other)?;
    let conn = rusqlite::Connection::open(crate::dispatch::DispatchStore::active_path())
        .map_err(std::io::Error::other)?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT d.id, d.kind, d.room, d.project, fa.name, ta.name, d.body,
                   d.label, d.status, d.exit_code, d.created_at
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.room = ?1 AND (?2 IS NULL OR d.project = ?2)
            ORDER BY d.id DESC
            LIMIT ?3
            "#,
        )
        .map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map(
            rusqlite::params![room, project, i64::from(limit.max(1))],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<i32>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .map_err(std::io::Error::other)?;
    let mut rows = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(std::io::Error::other)?;
    rows.reverse();
    if rows.is_empty() {
        println!("log: no dispatches");
        return Ok(0);
    }
    for (id, kind, room, _project, from, to, body, label, status, exit_code, created_at) in rows {
        if kind == "command" {
            println!(
                "#{id} [{room}] {created_at} command {} -> {}: {} status={} exit={}",
                from,
                to,
                label.unwrap_or(body),
                status,
                exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
        } else {
            println!("#{id} [{room}] {created_at} {from} -> {to}: {body}");
        }
    }
    Ok(0)
}

fn log_stats(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    let mut room: Option<String> = None;
    let mut since: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--room" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --room");
                    return Ok(2);
                };
                room = Some(value.clone());
                index += 2;
            }
            "--since" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --since");
                    return Ok(2);
                };
                since = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let _store = crate::dispatch::DispatchStore::open_active().map_err(std::io::Error::other)?;
    let conn = rusqlite::Connection::open(crate::dispatch::DispatchStore::active_path())
        .map_err(std::io::Error::other)?;
    let room_filter = room.as_deref().unwrap_or("%");
    let since_cutoff = log_stats_since_cutoff(&conn, since.as_deref())?;
    let since_filter = since_cutoff.as_deref();
    let reply_pairs = log_stats_reply_pairs(&conn, room_filter, since_filter)?;
    let replied: usize = reply_pairs.iter().map(|row| row.count).sum();
    let unanswered_rows = log_stats_unanswered(&conn, room_filter, since_filter)?;
    let command_rows = log_stats_commands(&conn, since_filter)?;
    let commands: i64 = command_rows.iter().map(|row| row.runs).sum();
    let model_rows = log_stats_model_matrix(&conn, since_filter)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "reply_latency": {"replied": replied, "pairs": reply_pairs},
                "unanswered": {"count": unanswered_rows.len(), "items": unanswered_rows},
                "command_stats": {"commands": commands, "labels": command_rows},
                "model_matrix": {"pairs": model_rows},
            }))?
        );
    } else {
        println!("■ 返答時間（返答済み {replied}件）");
        println!("  pair                          p50     p95     max");
        for row in &reply_pairs {
            println!(
                "  {:<29} {:>7} {:>7} {:>7}",
                truncate_for_table(&row.pair, 29),
                format_seconds(row.p50_seconds),
                format_seconds(row.p95_seconds),
                format_seconds(row.max_seconds)
            );
        }
        println!("■ 返答抜け（未返答 {}件）", unanswered_rows.len());
        for row in &unanswered_rows {
            println!(
                "  #{:<4} {} -> {}   {:?}   経過 {}",
                row.id,
                row.asked_by,
                row.asked_to,
                truncate_for_table(&row.body, 30),
                format_seconds(row.waiting_seconds)
            );
        }
        println!("■ コマンド実行（kind=command {commands}件）");
        println!("  label                         件数   成功率   平均      最大");
        for row in &command_rows {
            println!(
                "  {:<29} {:>4} {:>6} {:>8} {:>8}",
                truncate_for_table(&row.label, 29),
                row.runs,
                format_percent(row.success_rate),
                row.avg_seconds
                    .map(format_seconds)
                    .unwrap_or_else(|| "-".into()),
                row.max_seconds
                    .map(format_seconds)
                    .unwrap_or_else(|| "-".into())
            );
        }
        println!("■ モデル別（依頼→返答マトリクス）");
        for row in &model_rows {
            println!(
                "  {} -> {}   {}",
                row.asked_model, row.answered_model, row.dispatches
            );
        }
    }
    Ok(0)
}

#[derive(serde::Serialize)]
struct ReplyLatencyStats {
    pair: String,
    count: usize,
    p50_seconds: i64,
    p95_seconds: i64,
    max_seconds: i64,
}

#[derive(serde::Serialize)]
struct UnansweredStats {
    id: i64,
    asked_by: String,
    asked_to: String,
    body: String,
    waiting_seconds: i64,
}

#[derive(serde::Serialize)]
struct CommandStats {
    label: String,
    runs: i64,
    success_rate: Option<f64>,
    avg_seconds: Option<i64>,
    max_seconds: Option<i64>,
}

#[derive(serde::Serialize)]
struct ModelMatrixStats {
    asked_model: String,
    answered_model: String,
    dispatches: i64,
}

fn log_stats_since_cutoff(
    conn: &rusqlite::Connection,
    since: Option<&str>,
) -> std::io::Result<Option<String>> {
    let Some(since) = since.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let Some((amount, unit)) = parse_relative_since(since) else {
        return Ok(Some(since.to_string()));
    };
    let modifier = format!("-{amount} {unit}");
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1)",
        rusqlite::params![modifier],
        |row| row.get(0),
    )
    .map(Some)
    .map_err(std::io::Error::other)
}

fn parse_relative_since(value: &str) -> Option<(u64, &'static str)> {
    let (digits, suffix) = value.split_at(value.len().checked_sub(1)?);
    let amount = digits.parse::<u64>().ok()?;
    let unit = match suffix {
        "s" => "seconds",
        "m" => "minutes",
        "h" => "hours",
        "d" => "days",
        _ => return None,
    };
    Some((amount, unit))
}

fn log_stats_reply_pairs(
    conn: &rusqlite::Connection,
    room_filter: &str,
    since_filter: Option<&str>,
) -> std::io::Result<Vec<ReplyLatencyStats>> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT fa.name, COALESCE(fa.model, 'NULL'), ta.name, COALESCE(ta.model, 'NULL'),
                   CAST((julianday(d.replied_at) - julianday(d.created_at)) * 86400 AS INTEGER)
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.kind='message'
              AND d.replied_at IS NOT NULL
              AND d.room LIKE ?1
              AND (?2 IS NULL OR d.created_at >= ?2)
            ORDER BY d.id ASC
            "#,
        )
        .map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map(rusqlite::params![room_filter, since_filter], |row| {
            let asked_by: String = row.get(0)?;
            let asked_model: String = row.get(1)?;
            let answered_by: String = row.get(2)?;
            let answered_model: String = row.get(3)?;
            let seconds: i64 = row.get(4)?;
            Ok((
                format!("{asked_model}({asked_by}) -> {answered_model}({answered_by})"),
                seconds,
            ))
        })
        .map_err(std::io::Error::other)?;
    let mut grouped = std::collections::BTreeMap::<String, Vec<i64>>::new();
    for row in rows {
        let (pair, seconds) = row.map_err(std::io::Error::other)?;
        grouped.entry(pair).or_default().push(seconds);
    }
    Ok(grouped
        .into_iter()
        .map(|(pair, mut seconds)| {
            seconds.sort_unstable();
            ReplyLatencyStats {
                pair,
                count: seconds.len(),
                p50_seconds: percentile_seconds(&seconds, 50),
                p95_seconds: percentile_seconds(&seconds, 95),
                max_seconds: seconds.last().copied().unwrap_or(0),
            }
        })
        .collect())
}

fn log_stats_unanswered(
    conn: &rusqlite::Connection,
    room_filter: &str,
    since_filter: Option<&str>,
) -> std::io::Result<Vec<UnansweredStats>> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, asked_by, asked_to, body, waiting_seconds
            FROM v_unanswered
            WHERE room LIKE ?1 AND (?2 IS NULL OR created_at >= ?2)
            ORDER BY id ASC
            LIMIT 10
            "#,
        )
        .map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map(rusqlite::params![room_filter, since_filter], |row| {
            Ok(UnansweredStats {
                id: row.get(0)?,
                asked_by: row.get(1)?,
                asked_to: row.get(2)?,
                body: row.get(3)?,
                waiting_seconds: row.get(4)?,
            })
        })
        .map_err(std::io::Error::other)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(std::io::Error::other)
}

fn log_stats_commands(
    conn: &rusqlite::Connection,
    since_filter: Option<&str>,
) -> std::io::Result<Vec<CommandStats>> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT COALESCE(label, body), COUNT(*),
                   AVG(CASE WHEN exit_code = 0 THEN 1.0 ELSE 0.0 END),
                   CAST(AVG((julianday(finished_at) - julianday(started_at)) * 86400) AS INTEGER),
                   CAST(MAX((julianday(finished_at) - julianday(started_at)) * 86400) AS INTEGER)
            FROM dispatches
            WHERE kind='command'
              AND finished_at IS NOT NULL
              AND (?1 IS NULL OR created_at >= ?1)
            GROUP BY COALESCE(label, body)
            ORDER BY COUNT(*) DESC, COALESCE(label, body) ASC
            LIMIT 10
            "#,
        )
        .map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map(rusqlite::params![since_filter], |row| {
            Ok(CommandStats {
                label: row.get(0)?,
                runs: row.get(1)?,
                success_rate: row.get(2)?,
                avg_seconds: row.get(3)?,
                max_seconds: row.get(4)?,
            })
        })
        .map_err(std::io::Error::other)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(std::io::Error::other)
}

fn log_stats_model_matrix(
    conn: &rusqlite::Connection,
    since_filter: Option<&str>,
) -> std::io::Result<Vec<ModelMatrixStats>> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT COALESCE(fa.model, 'NULL'), COALESCE(ta.model, 'NULL'), COUNT(*)
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE (?1 IS NULL OR d.created_at >= ?1)
            GROUP BY fa.model, ta.model
            ORDER BY COUNT(*) DESC, fa.model, ta.model
            LIMIT 20
            "#,
        )
        .map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map(rusqlite::params![since_filter], |row| {
            Ok(ModelMatrixStats {
                asked_model: row.get(0)?,
                answered_model: row.get(1)?,
                dispatches: row.get(2)?,
            })
        })
        .map_err(std::io::Error::other)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(std::io::Error::other)
}

fn percentile_seconds(sorted: &[i64], percentile: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() * percentile).div_ceil(100)).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn format_seconds(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn format_percent(value: Option<f64>) -> String {
    value
        .map(|rate| format!("{:.0}%", rate * 100.0))
        .unwrap_or_else(|| "-".into())
}

fn truncate_for_table(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!(
            "{}…",
            truncated
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        truncated
    }
}

struct MsgIdentity {
    agent: String,
    project: String,
}

fn current_msg_identity(explicit_agent: Option<String>) -> std::io::Result<MsgIdentity> {
    let pane_id = match resolve_current_pane_id() {
        Ok(pane_id) => Some(pane_id),
        Err(err) => {
            if explicit_agent.is_none() {
                return Err(err);
            }
            None
        }
    };

    let project = pane_id
        .as_deref()
        .and_then(|pane_id| pane_info_for_msg(pane_id).ok())
        .and_then(|pane| pane["result"]["pane"]["cwd"].as_str().map(str::to_string))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|path| path.display().to_string())
        })
        .unwrap_or_default();

    if let Some(agent) = explicit_agent {
        let agent = agent.trim().to_string();
        if agent.is_empty() {
            return Err(std::io::Error::other("--from/--to must not be empty"));
        }
        return Ok(MsgIdentity { agent, project });
    }

    let Some(pane_id) = pane_id else {
        return Err(std::io::Error::other("current pane could not be resolved"));
    };
    let agent = resolve_agent_target(&pane_id, "cli:msg:identity")?;
    let name = agent["result"]["agent"]["name"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            agent["result"]["agent"]["agent"]
                .as_str()
                .map(|_| pane_id.clone())
        })
        .ok_or_else(|| {
            std::io::Error::other(
                "current pane has no reported agent identity; pass --from/--to explicitly",
            )
        })?;
    Ok(MsgIdentity {
        agent: name,
        project,
    })
}

fn pane_info_for_msg(pane_id: &str) -> std::io::Result<serde_json::Value> {
    send_request(&Request {
        id: "cli:msg:pane".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_string(),
        }),
    })
}

fn parse_msg_history_args(args: &[String]) -> std::io::Result<(String, Option<String>, u32)> {
    let mut room = crate::msg::DEFAULT_ROOM.to_string();
    let mut project = None;
    let mut limit = 50_u32;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--room" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --room");
                    return Err(std::io::Error::other("missing value for --room"));
                };
                room = value.clone();
                index += 2;
            }
            "--project" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --project");
                    return Err(std::io::Error::other("missing value for --project"));
                };
                project = Some(value.clone());
                index += 2;
            }
            "--limit" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --limit");
                    return Err(std::io::Error::other("missing value for --limit"));
                };
                limit = parse_u32_flag("--limit", value)?;
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Err(std::io::Error::other(format!("unknown option: {other}")));
            }
        }
    }
    Ok((room, project, limit))
}

fn print_msg_send_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if let Some(error) = response.get("error") {
        eprintln!("{error}");
        return Ok(1);
    }
    let messages = response["result"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("sent {} message(s)", messages.len());
    for message in messages {
        print_msg_message(&message);
    }
    let nudged = response["result"]["nudged"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !nudged.is_empty() {
        println!("nudged: {}", nudged.join(", "));
    }
    Ok(0)
}

fn print_msg_messages_response(response: &serde_json::Value, label: &str) -> std::io::Result<i32> {
    if let Some(error) = response.get("error") {
        eprintln!("{error}");
        return Ok(1);
    }
    let messages = response["result"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        println!("{label}: no messages");
        return Ok(0);
    }
    for message in messages {
        print_msg_message(&message);
    }
    Ok(0)
}

fn print_msg_message(message: &serde_json::Value) {
    let id = message["id"].as_i64().unwrap_or(0);
    let room = message["room"].as_str().unwrap_or("");
    let created_at = message["created_at"].as_str().unwrap_or("");
    let from = message["from_agent"].as_str().unwrap_or("");
    let to = message["to_agent"].as_str().unwrap_or("");
    let body = message["body"].as_str().unwrap_or("");
    println!("#{id} [{room}] {created_at} {from} -> {to}: {body}");
}

fn agent_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    }
    let name = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    print_response(&send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(AgentRenameParams {
            target: target.clone(),
            name,
        }),
    })?)
}

fn agent_send(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr agent send <target> <text>");
        return Ok(2);
    }

    eprintln!("hint: AI間の連絡は `herdr send`（記録・順序保証付き）");
    print_response(&send_request(&Request {
        id: "cli:agent:send".into(),
        method: Method::AgentSend(AgentSendParams {
            target: args[0].clone(),
            text: args[1..].join(" "),
        }),
    })?)
}

fn agent_read(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent read <target> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = parse_read_format(value)?;
                strip_ansi = !matches!(format, ReadFormat::Ansi);
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(AgentReadParams {
            target: target.clone(),
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?)
}

fn pane_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:pane:list".into(),
        method: Method::PaneList(PaneListParams { workspace_id }),
    })?)
}

fn pane_current(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr pane current");
        return Ok(2);
    }

    match resolve_current_pane_id() {
        Ok(pane_id) => {
            println!("{pane_id}");
            Ok(0)
        }
        Err(err) => {
            eprintln!("{err}");
            eprintln!(
                "unable to identify the calling pane automatically; do not use the focused pane as a fallback"
            );
            eprintln!(
                "next: inspect `herdr pane list`, then verify a candidate with `herdr pane get <pane_id>` and `herdr pane read <pane_id> --source recent --lines 40`"
            );
            Ok(1)
        }
    }
}

fn resolve_current_pane_id() -> std::io::Result<String> {
    let (request, fallback) = match std::env::var(crate::integration::HERDR_PANE_ID_ENV_VAR) {
        Ok(value) if !value.trim().is_empty() => {
            let pane_id = normalize_pane_id(value.trim());
            (
                Request {
                    id: "cli:pane:current".into(),
                    method: Method::PaneGet(PaneTarget {
                        pane_id: pane_id.clone(),
                    }),
                },
                pane_id,
            )
        }
        _ => (
            Request {
                id: "cli:pane:current".into(),
                method: Method::PaneCurrent(PaneCurrentParams {
                    process_id: std::process::id(),
                }),
            },
            String::new(),
        ),
    };

    let response = send_request(&request)?;

    if let Some(error) = response.get("error") {
        if current_pane_unsupported_error(error) {
            return Err(std::io::Error::other(
                "running herdr server does not support `pane.current`; restart the server so the installed herdr binary takes effect",
            ));
        }
        return Err(std::io::Error::other(
            serde_json::to_string(error).unwrap_or_else(|_| error.to_string()),
        ));
    }

    let pane_id = current_pane_id_from_response(&response, &fallback);
    if pane_id.is_empty() {
        return Err(std::io::Error::other(
            "current pane response did not include pane id",
        ));
    }
    Ok(pane_id)
}

fn current_pane_id_from_response(response: &serde_json::Value, fallback: &str) -> String {
    response["result"]["pane"]["global_id"]
        .as_str()
        .unwrap_or(fallback)
        .to_string()
}

fn current_pane_unsupported_error(error: &serde_json::Value) -> bool {
    let text = serde_json::to_string(error).unwrap_or_else(|_| error.to_string());
    text.contains("unknown variant")
        && text.contains("pane.current")
        && text.contains("expected one of")
}

fn pane_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane get <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr pane get <pane_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:pane:get".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane focus <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr pane focus <pane_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:pane:focus".into(),
        method: Method::PaneFocus(PaneTarget {
            pane_id: normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane rename <pane_id> <label>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: herdr pane rename <pane_id> <label>|--clear");
        return Ok(2);
    }
    let label = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    print_response(&send_request(&Request {
        id: "cli:pane:rename".into(),
        method: Method::PaneRename(PaneRenameParams {
            pane_id: normalize_pane_id(raw_pane_id),
            label,
        }),
    })?)
}

fn pane_read(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = parse_read_format(value)?;
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                index += 1;
            }
            "--raw" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let response = send_request(&Request {
        id: "cli:pane:read".into(),
        method: Method::PaneRead(PaneReadParams {
            pane_id,
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?;

    if let Some(error) = response.get("error") {
        eprintln!("{}", serde_json::to_string(error).unwrap());
        return Ok(1);
    }

    if let Some(text) = response["result"]["read"]["text"].as_str() {
        print!("{text}");
    }
    Ok(0)
}

fn pane_split(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!(
            "usage: herdr pane split <pane_id> --direction right|down [--cwd PATH] [--focus] [--no-focus]"
        );
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut direction = None;
    let mut cwd = None;
    let mut focus = false;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --direction");
                    return Ok(2);
                };
                direction = Some(parse_split_direction(value)?);
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(direction) = direction else {
        eprintln!("missing required --direction");
        return Ok(2);
    };

    print_response(&send_request(&Request {
        id: "cli:pane:split".into(),
        method: Method::PaneSplit(PaneSplitParams {
            workspace_id: None,
            target_pane_id: pane_id,
            direction,
            cwd,
            focus,
        }),
    })?)
}

fn pane_move(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane move <pane_id> (--new-tab [--label TEXT] | --tab <tab_id> --split right|down) [--focus|--no-focus]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut new_tab = false;
    let mut tab_id = None;
    let mut split = None;
    let mut label = None;
    let mut focus = false;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--new-tab" => {
                new_tab = true;
                index += 1;
            }
            "--tab" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --tab");
                    return Ok(2);
                };
                tab_id = Some(normalize_tab_id(value));
                index += 2;
            }
            "--split" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --split");
                    return Ok(2);
                };
                split = Some(parse_split_direction(value)?);
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let destination = match (new_tab, tab_id, split) {
        (true, None, None) => PaneMoveDestination::NewTab { label },
        (false, Some(tab_id), Some(split)) if label.is_none() => {
            PaneMoveDestination::Tab { tab_id, split }
        }
        _ => {
            eprintln!("usage: herdr pane move <pane_id> (--new-tab [--label TEXT] | --tab <tab_id> --split right|down) [--focus|--no-focus]");
            return Ok(2);
        }
    };

    print_response(&send_request(&Request {
        id: "cli:pane:move".into(),
        method: Method::PaneMove(PaneMoveParams {
            pane_id,
            destination,
            focus,
        }),
    })?)
}

fn pane_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane close <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr pane close <pane_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:pane:close".into(),
        method: Method::PaneClose(PaneTarget {
            pane_id: normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_send_text(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-text <pane_id> <text>");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    send_ok_request(Method::PaneSendText(PaneSendTextParams { pane_id, text }))
}

fn pane_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-keys <pane_id> <key> [key ...]");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let keys = args[1..].to_vec();
    send_ok_request(Method::PaneSendKeys(PaneSendKeysParams { pane_id, keys }))
}

fn pane_run(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane run <pane_id> <command>");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    send_pane_text_then_enter(pane_id, text)
}

fn pane_run_notify(args: &[String]) -> std::io::Result<i32> {
    let _ = args;
    eprintln!("error: `herdr pane run-notify` has been removed; use `herdr run -- <command...>`");
    Ok(2)
}

fn pane_job_log(args: &[String]) -> std::io::Result<i32> {
    let Some(job_id) = args.first() else {
        eprintln!("usage: herdr pane job-log <job_id> [--tail N|tail=N]");
        return Ok(2);
    };
    if !valid_pane_job_id(job_id) {
        eprintln!("usage: herdr pane job-log <job_id> [--tail N|tail=N]");
        return Ok(2);
    }
    let tail_lines = match parse_job_log_tail(&args[1..]) {
        Ok(tail_lines) => tail_lines,
        Err(()) => {
            eprintln!("usage: herdr pane job-log <job_id> [--tail N|tail=N]");
            return Ok(2);
        }
    };

    let log_path = pane_job_log_path(job_id)?;
    let text = std::fs::read_to_string(&log_path).map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!("failed to read {}: {err}", log_path.display()),
        )
    })?;
    if let Some(tail_lines) = tail_lines {
        print!("{}", tail_text(&text, tail_lines));
    } else {
        print!("{text}");
    }
    Ok(0)
}

fn parse_job_log_tail(args: &[String]) -> Result<Option<usize>, ()> {
    match args {
        [] => Ok(None),
        [arg] if arg.starts_with("tail=") => parse_tail_value(&arg["tail=".len()..]).map(Some),
        [flag, value] if flag == "--tail" => parse_tail_value(value).map(Some),
        _ => Err(()),
    }
}

fn parse_tail_value(value: &str) -> Result<usize, ()> {
    value.parse::<usize>().map_err(|_| ())
}

fn tail_text(text: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let mut lines = text.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    let mut result = lines.join("\n");
    if text.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result
}

fn pane_report_agent(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--title TEXT] [--session-id ID] [--model NAME]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut state = None;
    let mut message = None;
    let mut custom_status = None;
    let mut seq = None;
    let mut title = None;
    let mut session_id = None;
    let mut model = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--state" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state");
                    return Ok(2);
                };
                state = Some(parse_pane_agent_state(value)?);
                index += 2;
            }
            "--message" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --message");
                    return Ok(2);
                };
                message = Some(value.clone());
                index += 2;
            }
            "--custom-status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --custom-status");
                    return Ok(2);
                };
                custom_status = Some(value.clone());
                index += 2;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --title");
                    return Ok(2);
                };
                title = Some(value.clone());
                index += 2;
            }
            "--session-id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --session-id");
                    return Ok(2);
                };
                session_id = Some(value.clone());
                index += 2;
            }
            "--model" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --model");
                    return Ok(2);
                };
                model = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let Some(agent) = agent else {
        eprintln!("missing required --agent");
        return Ok(2);
    };
    let Some(state) = state else {
        eprintln!("missing required --state");
        return Ok(2);
    };

    send_ok_request(Method::PaneReportAgent(PaneReportAgentParams {
        pane_id,
        source,
        agent,
        state,
        message,
        custom_status,
        seq,
        title,
        session_id,
        model,
    }))
}

fn wait_output(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--regex]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut timeout_ms = None;
    let mut strip_ansi = true;
    let mut regex = false;
    let mut match_value = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--match" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --match");
                    return Ok(2);
                };
                match_value = Some(value.clone());
                index += 2;
            }
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "--regex" => {
                regex = true;
                index += 1;
            }
            "--raw" => {
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(match_value) = match_value else {
        eprintln!("missing required --match");
        return Ok(2);
    };

    let matcher = if regex {
        OutputMatch::Regex { value: match_value }
    } else {
        OutputMatch::Substring { value: match_value }
    };

    let response = send_request(&Request {
        id: "cli:wait:output".into(),
        method: Method::PaneWaitForOutput(PaneWaitForOutputParams {
            pane_id,
            source,
            lines,
            r#match: matcher,
            timeout_ms,
            strip_ansi,
        }),
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(0)
}

fn wait_agent_status(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    let current = send_request(&Request {
        id: "cli:wait:agent-status:current".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.clone(),
        }),
    })?;
    if current.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&current).unwrap());
        return Ok(1);
    }
    let desired_status_value = serde_json::to_value(agent_status).map_err(std::io::Error::other)?;
    if current["result"]["pane"]["agent_status"] == desired_status_value {
        print_current_agent_status_event(&current)?;
        return Ok(0);
    }

    wait_for_agent_change(
        Request {
            id: "cli:wait:agent-status".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions: vec![Subscription::PaneAgentStatusChanged {
                    pane_id,
                    agent_status: Some(agent_status),
                }],
            }),
        },
        timeout_ms,
        "timed out waiting for agent status change",
    )
}

fn print_current_agent_status_event(response: &serde_json::Value) -> std::io::Result<()> {
    let pane = &response["result"]["pane"];
    let mut data = serde_json::Map::new();
    data.insert("pane_id".into(), pane["pane_id"].clone());
    data.insert("workspace_id".into(), pane["workspace_id"].clone());
    data.insert("agent_status".into(), pane["agent_status"].clone());
    if !pane["agent"].is_null() {
        data.insert("agent".into(), pane["agent"].clone());
    }
    if !pane["custom_status"].is_null() {
        data.insert("custom_status".into(), pane["custom_status"].clone());
    }
    let event = serde_json::json!({
        "event": "pane.agent_status_changed",
        "data": data,
    });
    println!(
        "{}",
        serde_json::to_string(&event).map_err(std::io::Error::other)?
    );
    Ok(())
}

fn wait_for_agent_change(
    request: Request,
    timeout_ms: Option<u64>,
    timeout_message: &str,
) -> std::io::Result<i32> {
    let mut stream = UnixStream::connect(api::socket_path())?;
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    if let Some(timeout_ms) = timeout_ms {
        stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)))?;
    }

    let mut reader = BufReader::new(stream);
    let mut ack = String::new();
    reader.read_line(&mut ack)?;
    if ack.trim().is_empty() {
        eprintln!("empty subscription ack");
        return Ok(1);
    }
    let ack_value: serde_json::Value = serde_json::from_str(&ack)?;
    if ack_value.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&ack_value).unwrap());
        return Ok(1);
    }

    let mut event = String::new();
    match reader.read_line(&mut event) {
        Ok(0) => {
            eprintln!("subscription closed before event arrived");
            Ok(1)
        }
        Ok(_) => {
            let event_value: serde_json::Value = serde_json::from_str(&event)?;
            println!("{}", serde_json::to_string(&event_value).unwrap());
            Ok(0)
        }
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            eprintln!("{timeout_message}");
            Ok(1)
        }
        Err(err) => Err(err),
    }
}

fn print_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(response).unwrap());
    Ok(0)
}

fn send_ok_request(method: Method) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:request".into(),
        method,
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    Ok(0)
}

fn send_request(request: &Request) -> std::io::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(api::socket_path())?;
    stream.write_all(serde_json::to_string(request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

fn send_pane_text_then_enter(pane_id: String, text: String) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:pane:send-text".into(),
        method: Method::PaneSendText(PaneSendTextParams {
            pane_id: pane_id.clone(),
            text,
        }),
    })?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    std::thread::sleep(CLI_SUBMIT_DELAY);

    let response = send_request(&Request {
        id: "cli:pane:send-enter".into(),
        method: Method::PaneSendKeys(PaneSendKeysParams {
            pane_id,
            keys: vec!["Enter".into()],
        }),
    })?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    Ok(0)
}

fn send_pane_input_text_enter(pane_id: &str, text: String) -> std::io::Result<serde_json::Value> {
    send_request(&Request {
        id: "cli:pane:send-input".into(),
        method: Method::PaneSendInput(PaneSendInputParams {
            pane_id: pane_id.to_string(),
            text,
            keys: vec!["Enter".into()],
        }),
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn pane_notify_runner(args: &[String]) -> std::io::Result<i32> {
    let mut parent = None;
    let mut target = None;
    let mut job_id = None;
    let mut run_label = None;
    let mut close_on_exit = false;
    let mut close_on_success = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--parent" => {
                parent = args.get(index + 1).cloned();
                index += 2;
            }
            "--target" => {
                target = args.get(index + 1).cloned();
                index += 2;
            }
            "--job-id" => {
                job_id = args.get(index + 1).cloned();
                index += 2;
            }
            "--run-label" => {
                run_label = args.get(index + 1).cloned();
                index += 2;
            }
            "--close-on-success" => {
                close_on_success = true;
                index += 1;
            }
            "--close-on-exit" => {
                close_on_exit = true;
                index += 1;
            }
            "--" => {
                index += 1;
                break;
            }
            _ => break,
        }
    }

    let Some(parent) = parent else {
        eprintln!(
            "usage: herdr __pane-notify-run --parent PANE --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    };
    let Some(target) = target else {
        eprintln!(
            "usage: herdr __pane-notify-run --parent PANE --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    };
    let Some(job_id) = job_id.filter(|id| valid_pane_job_id(id)) else {
        eprintln!(
            "usage: herdr __pane-notify-run --parent PANE --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    };
    if index >= args.len() {
        eprintln!(
            "usage: herdr __pane-notify-run --parent PANE --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    }
    let command = args[index..].join(" ");

    let started = SystemTime::now();
    let log_path = pane_job_log_path(&job_id)?;
    if let Some(parent_dir) = log_path.parent() {
        std::fs::create_dir_all(parent_dir)?;
    }
    let log = Arc::new(Mutex::new(std::fs::File::create(&log_path)?));
    {
        let mut log = log.lock().unwrap();
        writeln!(log, "job_id: {job_id}")?;
        writeln!(log, "target_pane: {target}")?;
        writeln!(log, "parent_pane: {parent}")?;
        writeln!(log, "command: {command}")?;
        writeln!(log, "started_unix_ms: {}", unix_millis(started))?;
        writeln!(log)?;
    }

    let tail = Arc::new(Mutex::new(String::new()));
    let mut child_command = Command::new("/bin/sh");
    child_command.arg("-lc").arg(&command);
    if let Some(path) = path_with_cwd_node_bin()? {
        child_command.env("PATH", path);
    }
    let mut child = child_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = stdout.map(|stdout| {
        stream_job_output(
            stdout,
            std::io::stdout(),
            log.clone(),
            tail.clone(),
            "stdout",
        )
    });
    let stderr_thread = stderr.map(|stderr| {
        stream_job_output(
            stderr,
            std::io::stderr(),
            log.clone(),
            tail.clone(),
            "stderr",
        )
    });

    let status = child.wait()?;
    if let Some(thread) = stdout_thread {
        let _ = thread.join();
    }
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }

    let finished = SystemTime::now();
    let code = status.code();
    {
        let mut log = log.lock().unwrap();
        writeln!(log)?;
        writeln!(log, "finished_unix_ms: {}", unix_millis(finished))?;
        writeln!(log, "exit_code: {}", exit_code_label(code))?;
    }

    let sample = tail.lock().unwrap().clone();
    let (title, context) = pane_notify_toast(&job_id, &target, &command, code, &log_path, &sample);
    let _ = send_ok_request(Method::PaneNotify(PaneNotifyParams {
        pane_id: parent.clone(),
        title,
        context,
    }));
    if let Some(label) = run_label {
        let notification = pane_run_notification_line(&label, &target, &job_id, code, &sample);
        let _ = send_pane_input_text_enter(&parent, notification);
    }
    if close_on_exit || (close_on_success && code == Some(0)) {
        let _ = send_ok_request(Method::PaneClose(PaneTarget { pane_id: target }));
    }

    Ok(code.unwrap_or(1))
}

fn background_runner(args: &[String]) -> std::io::Result<i32> {
    let Some(job_id_index) = args.iter().position(|arg| arg == "--job-id") else {
        eprintln!("usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>");
        return Ok(2);
    };
    let Some(job_id) = args
        .get(job_id_index + 1)
        .filter(|id| valid_pane_job_id(id))
    else {
        eprintln!("usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>");
        return Ok(2);
    };
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        eprintln!("usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>");
        return Ok(2);
    };
    let mut completion_override = None;
    let mut parse_index = 0;
    while parse_index < separator {
        if args[parse_index] == "--completion" {
            let Some(value) = args
                .get(parse_index + 1)
                .filter(|_| parse_index + 1 < separator)
            else {
                eprintln!("missing value for --completion");
                return Ok(2);
            };
            completion_override = Some(value.clone());
            parse_index += 2;
        } else {
            parse_index += 1;
        }
    }
    let command_args = &args[separator + 1..];
    let Some((program, program_args)) = command_args.split_first() else {
        eprintln!("usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>");
        return Ok(2);
    };

    let store = crate::job::JobStore::open_active().map_err(std::io::Error::other)?;
    let Some(mut job) = store.get(job_id).map_err(std::io::Error::other)? else {
        eprintln!("job not found: {job_id}");
        return Ok(1);
    };
    if let Some(completion) = completion_override {
        job.completion = completion;
    }
    let started = SystemTime::now();
    store
        .mark_running(job_id, std::process::id(), unix_millis(started))
        .map_err(std::io::Error::other)?;
    let log_path = std::path::PathBuf::from(&job.log_path);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = Arc::new(Mutex::new(std::fs::File::create(&log_path)?));
    {
        let mut log = log
            .lock()
            .map_err(|_| std::io::Error::other("job log lock poisoned"))?;
        writeln!(log, "job_id: {job_id}")?;
        writeln!(log, "runner_pid: {}", std::process::id())?;
        writeln!(log, "caller_pane: {}", job.caller_pane)?;
        writeln!(log, "command: {}", job.command)?;
        writeln!(log, "cwd: {}", job.cwd)?;
        writeln!(log, "started_unix_ms: {}", unix_millis(started))?;
        writeln!(log)?;
    }

    let tail = Arc::new(Mutex::new(String::new()));
    let mut command = Command::new(program);
    command
        .args(program_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path_with_cwd_node_bin()? {
        command.env("PATH", path);
    }
    let status = match command.spawn() {
        Ok(mut child) => {
            let stdout_thread = child.stdout.take().map(|stdout| {
                stream_job_output(stdout, std::io::sink(), log.clone(), tail.clone(), "stdout")
            });
            let stderr_thread = child.stderr.take().map(|stderr| {
                stream_job_output(stderr, std::io::sink(), log.clone(), tail.clone(), "stderr")
            });
            let status = child.wait()?;
            if let Some(thread) = stdout_thread {
                let _ = thread.join();
            }
            if let Some(thread) = stderr_thread {
                let _ = thread.join();
            }
            status
        }
        Err(err) => {
            if let Ok(mut log) = log.lock() {
                let _ = writeln!(log, "[runner] failed to spawn: {err}");
            }
            let completed = store
                .mark_finished(job_id, Some(127), unix_millis(SystemTime::now()))
                .map_err(std::io::Error::other)?;
            if completed {
                enqueue_job_completion(&job, Some(127), &log_path)?;
            }
            return Ok(127);
        }
    };
    let finished = SystemTime::now();
    let code = status.code();
    {
        let mut log = log
            .lock()
            .map_err(|_| std::io::Error::other("job log lock poisoned"))?;
        writeln!(log)?;
        writeln!(log, "finished_unix_ms: {}", unix_millis(finished))?;
        writeln!(log, "exit_code: {}", exit_code_label(code))?;
    }
    let completed = store
        .mark_finished(job_id, code, unix_millis(finished))
        .map_err(std::io::Error::other)?;
    if completed {
        enqueue_job_completion(&job, code, &log_path)?;
    }
    Ok(code.unwrap_or(1))
}

fn enqueue_job_mailbox(job: &crate::job::JobRecord, body: String) -> std::io::Result<()> {
    let reply_to = crate::dispatch::DispatchStore::open_active()
        .ok()
        .and_then(|store| store.command_dispatch_id(&job.id).ok().flatten());
    let response = send_request(&Request {
        id: format!("job:{}:completion", job.id),
        method: Method::MsgSend(MsgSendParams {
            room: crate::msg::JOBS_ROOM.into(),
            project: job.cwd.clone(),
            from_agent: "herdr-run".into(),
            to: job.caller_agent.clone(),
            body: body.clone(),
            reply_to,
        }),
    });
    if matches!(response, Ok(ref value) if value.get("error").is_none()) {
        return Ok(());
    }

    // The server may be restarting. Writing the same mailbox database keeps
    // completion durable; startup recovery nudges the exact caller when idle.
    let mut store = crate::msg::MsgStore::open_active().map_err(std::io::Error::other)?;
    store
        .insert_message_with_reply(
            crate::msg::JOBS_ROOM,
            &job.cwd,
            "herdr-run",
            &job.caller_agent,
            &body,
            reply_to,
        )
        .map_err(std::io::Error::other)?;
    Ok(())
}

fn enqueue_job_completion(
    job: &crate::job::JobRecord,
    code: Option<i32>,
    log_path: &std::path::Path,
) -> std::io::Result<()> {
    if job.completion == "none" {
        return Ok(());
    }
    let summary = format!(
        "[herdr run] exit={} label={} job={} details: herdr log {}",
        exit_code_label(code),
        one_line_field(&job.label),
        job.id,
        job.id
    );
    let body = if job.completion == "full" {
        let output = std::fs::read_to_string(log_path)?;
        format!("{summary}\n\n{output}")
    } else {
        summary
    };
    enqueue_job_mailbox(job, body)
}

fn pane_run_notification_line(
    label: &str,
    pane_id: &str,
    job_id: &str,
    code: Option<i32>,
    _sample: &str,
) -> String {
    format!(
        "[herdr run] exit={} label={} pane={} 詳細: herdr pane job-log {}",
        exit_code_label(code),
        one_line_field(label),
        pane_id,
        job_id
    )
}

fn path_with_cwd_node_bin() -> std::io::Result<Option<std::ffi::OsString>> {
    let cwd = std::env::current_dir()?;
    path_with_node_bin_from(&cwd, std::env::var_os("PATH").as_deref())
}

fn path_with_node_bin_from(
    cwd: &std::path::Path,
    existing_path: Option<&std::ffi::OsStr>,
) -> std::io::Result<Option<std::ffi::OsString>> {
    let node_bin = cwd.join("node_modules").join(".bin");
    if !node_bin.is_dir() {
        return Ok(None);
    }

    let mut paths = existing_path
        .map(std::env::split_paths)
        .map(Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    if paths.iter().any(|path| path == &node_bin) {
        return Ok(None);
    }
    paths.insert(0, node_bin);
    std::env::join_paths(paths)
        .map(Some)
        .map_err(|err| std::io::Error::other(format!("failed to construct PATH: {err}")))
}

fn one_line_field(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("_")
}

fn stream_job_output<R, W>(
    mut reader: R,
    mut output: W,
    log: Arc<Mutex<std::fs::File>>,
    tail: Arc<Mutex<String>>,
    stream_name: &'static str,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };
            let chunk = &buffer[..n];
            let _ = output.write_all(chunk);
            let _ = output.flush();
            if let Ok(mut log) = log.lock() {
                let _ = write!(log, "[{stream_name}] ");
                let _ = log.write_all(chunk);
                if !chunk.ends_with(b"\n") {
                    let _ = writeln!(log);
                }
            }
            append_tail_sample(&tail, &String::from_utf8_lossy(chunk));
        }
    })
}

fn append_tail_sample(tail: &Arc<Mutex<String>>, chunk: &str) {
    let mut tail = tail.lock().unwrap();
    tail.push_str(chunk);
    let count = tail.chars().count();
    if count > PANE_NOTIFY_SAMPLE_CHARS {
        *tail = tail
            .chars()
            .skip(count - PANE_NOTIFY_SAMPLE_CHARS)
            .collect();
    }
}

fn pane_notify_toast(
    job_id: &str,
    target: &str,
    command: &str,
    code: Option<i32>,
    log_path: &std::path::Path,
    sample: &str,
) -> (String, String) {
    let title = format!("pane job exited: {}", exit_code_label(code));
    let mut context = format!(
        "{target} · {job_id} · {}",
        truncate_for_message(command, 80)
    );
    let sample = sample.trim();
    if !sample.is_empty() {
        context.push_str(" · tail: ");
        context.push_str(&truncate_for_message(sample, 120));
    }
    context.push_str(" · log: ");
    context.push_str(&truncate_for_message(&log_path.display().to_string(), 120));
    (title, context)
}

fn truncate_for_message(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    format!("{}…", value.chars().take(keep).collect::<String>())
}

fn exit_code_label(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

fn new_pane_job_id() -> String {
    format!(
        "job-{}-{}",
        unix_millis(SystemTime::now()),
        std::process::id()
    )
}

fn unix_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn valid_pane_job_id(job_id: &str) -> bool {
    !job_id.is_empty()
        && job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn pane_job_log_path(job_id: &str) -> std::io::Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".local/state"))
        })
        .ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    Ok(base
        .join("herdr")
        .join("job-logs")
        .join(format!("{job_id}.log")))
}

fn normalize_workspace_id(value: &str) -> String {
    value.to_string()
}

fn normalize_tab_id(value: &str) -> String {
    value.to_string()
}

fn normalize_pane_id(value: &str) -> String {
    value.to_string()
}

fn parse_split_direction(value: &str) -> std::io::Result<SplitDirection> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err(std::io::Error::other(format!(
            "invalid split direction: {value}"
        ))),
    }
}

fn parse_read_source(value: &str) -> std::io::Result<ReadSource> {
    match value {
        "visible" => Ok(ReadSource::Visible),
        "recent" => Ok(ReadSource::Recent),
        "recent-unwrapped" | "recent_unwrapped" => Ok(ReadSource::RecentUnwrapped),
        _ => Err(std::io::Error::other(format!(
            "invalid read source: {value}"
        ))),
    }
}

fn parse_read_format(value: &str) -> std::io::Result<ReadFormat> {
    match value {
        "text" => Ok(ReadFormat::Text),
        "ansi" => Ok(ReadFormat::Ansi),
        _ => Err(std::io::Error::other(format!(
            "invalid read format: {value}"
        ))),
    }
}

fn agent_wait_status_satisfied(desired: AgentStatus, current: &str) -> bool {
    match desired {
        AgentStatus::Idle => matches!(current, "idle" | "done"),
        AgentStatus::Working => current == "working",
        AgentStatus::Blocked => current == "blocked",
        AgentStatus::Unknown => current == "unknown",
        AgentStatus::Done => false,
    }
}

fn parse_agent_wait_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "unknown" => Ok(AgentStatus::Unknown),
        "done" => Err(std::io::Error::other(
            "done is a UI attention state; use idle for CLI agent completion waits",
        )),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

fn parse_agent_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "done" => Ok(AgentStatus::Done),
        "unknown" => Ok(AgentStatus::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, done, or unknown)"
        ))),
    }
}

fn parse_pane_agent_state(value: &str) -> std::io::Result<PaneAgentState> {
    match value {
        "idle" => Ok(PaneAgentState::Idle),
        "working" => Ok(PaneAgentState::Working),
        "blocked" => Ok(PaneAgentState::Blocked),
        "unknown" => Ok(PaneAgentState::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid pane agent state: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

fn parse_u32_flag(flag: &str, value: &str) -> std::io::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

fn parse_u64_flag(flag: &str, value: &str) -> std::io::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

fn parse_session_json_only(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn parse_session_name_and_json(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let mut name = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("{usage}");
            return Err(2);
        }
    }

    let Some(name) = name else {
        eprintln!("{usage}");
        return Err(2);
    };
    Ok((name, json))
}

fn print_session_table(sessions: &[crate::session::SessionInfo]) {
    println!("{:<20} {:<8} {:<48} socket", "name", "status", "directory");
    for session in sessions {
        println!(
            "{:<20} {:<8} {:<48} {}",
            session.name,
            if session.running {
                "running"
            } else {
                "stopped"
            },
            session.session_dir,
            session.socket_path
        );
    }
}

fn print_session_error(code: &str, message: &str) {
    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .unwrap()
    );
}

fn print_server_help() {
    eprintln!("herdr server commands:");
    eprintln!("  herdr server                run as headless server");
    eprintln!("  herdr server stop           stop the running server via the API socket");
    eprintln!("  herdr server reload-config  reload config.toml in the running server");
}

fn print_status_help() {
    eprintln!("herdr status commands:");
    eprintln!("  herdr status         show local client and running server status");
    eprintln!("  herdr status server  show running server status");
    eprintln!("  herdr status client  show local client binary status");
}

fn print_config_help() {
    eprintln!("herdr config commands:");
    eprintln!("  herdr config reset-keys  back up config.toml and remove custom keybindings");
}

fn print_workspace_help() {
    eprintln!("herdr workspace commands:");
    eprintln!("  herdr workspace list");
    eprintln!("  herdr workspace create [--cwd PATH] [--label TEXT] [--focus] [--no-focus]");
    eprintln!("  herdr workspace get <workspace_id>");
    eprintln!("  herdr workspace focus <workspace_id>");
    eprintln!("  herdr workspace rename <workspace_id> <label>");
    eprintln!("  herdr workspace close <workspace_id>");
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab close <tab_id>");
}

fn print_agent_help() {
    eprintln!("herdr agent commands:");
    eprintln!("  herdr agent list");
    eprintln!("  herdr agent get <target>");
    eprintln!("  herdr agent read <target> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  herdr agent send <target> <text>");
    eprintln!("  herdr agent rename <target> <name>|--clear");
    eprintln!("  herdr agent focus <target>");
    eprintln!("  herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
    eprintln!("  herdr agent attach <target> [--takeover]");
    eprintln!("  herdr agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
    eprintln!("  herdr agent restore [--dry-run]   relaunch agents recorded in the restored session ([agent_restore] config)");
    eprintln!("  targets accept terminal ids, unique agent names, detected/reported agent labels, and legacy pane ids");
    eprintln!("  agent send writes text and submits it with Enter");
}

fn print_msg_help() {
    eprintln!("herdr msg commands:");
    eprintln!("  herdr msg send <to> <text> [--room R] [--reply-to ID] [--from NAME]");
    eprintln!("  herdr msg inbox [--room R] [--to NAME]");
    eprintln!("  herdr msg history [--room R] [--project P] [--limit N]");
    eprintln!("  herdr msg tail [--room R] [--project P] [--limit N]");
    eprintln!("  herdr msg rooms");
    eprintln!("  send targets accept agent names, pane targets, or '*' for room broadcast");
    print_data_footer();
}

fn print_send_help() {
    eprintln!("herdr send commands:");
    eprintln!("  herdr send <to> <text> [--room R] [--reply-to ID] [--from NAME]");
    eprintln!("  targets accept agent names, pane targets, or '*' for room broadcast");
    print_data_footer();
}

fn print_log_help() {
    eprintln!("herdr log commands:");
    eprintln!("  herdr log [--room R] [--project P] [--limit N]");
    eprintln!("  herdr log -f [--room R] [--project P] [--limit N]");
    eprintln!("  herdr log <job_id>");
    eprintln!("  herdr log rooms");
    eprintln!("  herdr log --db");
    eprintln!("  herdr log --schema");
    eprintln!("  herdr log stats [--room R] [--since D] [--json]");
    print_data_footer();
}

fn print_data_footer() {
    eprintln!(
        "data: ~/.config/herdr/herdr.db (per-session: ~/.config/herdr/sessions/<name>/herdr.db)"
    );
    eprintln!("      WAL mode; safe to query while running.");
    eprintln!("lowest-level API: sqlite3 \"$(herdr log --db)\" — schema: herdr log --schema");
}

fn print_job_help() {
    eprintln!("herdr job commands:");
    eprintln!("  herdr job list");
    eprintln!("  herdr job status <job_id>");
    eprintln!("  herdr job log <job_id> [--tail N|tail=N]");
    eprintln!("  herdr job cancel <job_id>");
}

fn print_terminal_help() {
    eprintln!("herdr terminal commands:");
    eprintln!("  herdr terminal attach <terminal_id> [--takeover]");
    eprintln!("  detach from direct attach with ctrl+b q; send literal ctrl+b with ctrl+b ctrl+b");
}

fn print_pane_help() {
    eprintln!("herdr pane commands:");
    eprintln!("  herdr pane list [--workspace <workspace_id>]");
    eprintln!("  herdr pane current");
    eprintln!("  herdr pane get <pane_id>");
    eprintln!("  herdr pane focus <pane_id>");
    eprintln!("  herdr pane rename <pane_id> <label>|--clear");
    eprintln!("  herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!(
        "  herdr pane split <pane_id> --direction right|down [--cwd PATH] [--focus] [--no-focus]"
    );
    eprintln!("  herdr pane move <pane_id> (--new-tab [--label TEXT] | --tab <tab_id> --split right|down) [--focus] [--no-focus]");
    eprintln!("  herdr pane close <pane_id>");
    eprintln!("  herdr pane send-text <pane_id> <text>");
    eprintln!("  herdr pane send-keys <pane_id> <key> [key ...]");
    eprintln!("  herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--title TEXT] [--session-id ID] [--model NAME]");
    eprintln!("  herdr pane run <pane_id> <command>");
    eprintln!("  herdr pane job-log <job_id> [--tail N|tail=N]");
    eprintln!("  pane current uses HERDR_PANE_ID first, then the calling process session, then its parent process tree");
    eprintln!("  if pane current cannot identify you, inspect pane list/get/read and fail closed when the candidate is ambiguous");
    eprintln!("  pane send-text writes literal text without Enter; pane run submits command text with Enter");
    eprintln!("  pane run-notify was removed; use herdr run for durable command execution");
}

fn print_run_help() {
    eprintln!("usage: herdr run [--label TEXT] [--cwd PATH] [--caller <pane>] [--completion summary|full|none] [--pane [--split right|down] [--close-on-success]] -- <command...>");
    eprintln!("  default: starts a pane-less, non-interactive background job and returns its job id immediately");
    eprintln!("  --pane: starts the command in a visible same-space pane; use this for interactive/TTY commands");
    eprintln!(
        "  --pane closes after any exit by default; --close-on-success keeps a failed pane open"
    );
    eprintln!("  --split and --close-on-success are valid only with --pane");
    eprintln!("  background completion is durable mailbox delivery to the exact caller: summary (default), full, or none");
    eprintln!("  inspect background jobs with `herdr run list`, `herdr log <job_id>`, and `herdr run cancel <job_id>`");
    eprintln!("  commands inherit cwd and environment; cwd/node_modules/.bin is prepended to PATH when present");
    eprintln!("  caller resolution fails closed; pass --caller <pane> when the calling pane cannot be identified");
    print_data_footer();
}

fn print_wait_help() {
    eprintln!("herdr wait commands:");
    eprintln!("  herdr wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--regex] [--raw]");
    eprintln!(
        "  herdr wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]"
    );
}

fn print_session_help() {
    eprintln!("herdr session commands:");
    eprintln!("  herdr session list [--json]");
    eprintln!("  herdr session attach <name>");
    eprintln!("  herdr session stop <name> [--json]");
    eprintln!("  herdr session delete <name> [--json]");
    eprintln!("  use 'default' as <name> to target the default session for stop");
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_pane_id_prefers_validated_global_id() {
        let response = serde_json::json!({
            "result": {
                "pane": {
                    "global_id": "p_42"
                }
            }
        });

        assert_eq!(current_pane_id_from_response(&response, "p_1"), "p_42");
    }

    #[test]
    fn current_pane_id_falls_back_to_env_id_for_older_response_shapes() {
        let response = serde_json::json!({
            "result": {
                "pane": {}
            }
        });

        assert_eq!(current_pane_id_from_response(&response, "p_1"), "p_1");
    }

    #[test]
    fn current_pane_unsupported_error_detects_old_server_schema() {
        let error = serde_json::json!({
            "code": "invalid_request",
            "message": "invalid request: unknown variant `pane.current`, expected one of `ping`, `pane.list`"
        });

        assert!(current_pane_unsupported_error(&error));
    }

    #[test]
    fn restart_needed_when_running_server_protocol_differs() {
        let status = ServerRuntimeStatus::Running {
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            protocol: Some(crate::server::protocol::PROTOCOL_VERSION - 1),
        };

        assert_eq!(
            compatibility_label(Some(crate::server::protocol::PROTOCOL_VERSION - 1)),
            "no"
        );
        assert_eq!(restart_needed_label(&status), "yes");
    }

    #[test]
    fn pane_notify_tail_sample_keeps_bounded_chars() {
        let tail = Arc::new(Mutex::new(String::new()));
        append_tail_sample(&tail, &"a".repeat(PANE_NOTIFY_SAMPLE_CHARS + 100));

        let sample = tail.lock().unwrap().clone();
        assert_eq!(sample.chars().count(), PANE_NOTIFY_SAMPLE_CHARS);
    }

    #[test]
    fn pane_notify_toast_summarizes_exit_without_shell_payload() {
        let (title, context) = pane_notify_toast(
            "job-123",
            "p_2",
            "printf '%s' hello",
            Some(0),
            std::path::Path::new("/tmp/herdr-job.log"),
            "hello\n",
        );

        assert_eq!(title, "pane job exited: 0");
        assert!(context.contains("p_2"));
        assert!(context.contains("job-123"));
        assert!(context.contains("tail: hello"));
        assert!(context.contains("log: /tmp/herdr-job.log"));
        assert!(!context.contains("[herdr] pane job exited"));
    }

    #[test]
    fn run_notification_line_is_single_line_and_points_to_job_log() {
        let line =
            pane_run_notification_line("cargo test", "p_2", "job-123", Some(0), "hello\nworld\n");

        assert_eq!(
            line,
            "[herdr run] exit=0 label=cargo_test pane=p_2 詳細: herdr pane job-log job-123"
        );
        assert!(!line.contains('\n'));
        assert!(!line.contains("tail="));
    }

    #[test]
    fn run_shell_command_quotes_each_argument() {
        let command = shell_command_from_args(&[
            "sh".to_string(),
            "-c".to_string(),
            "sleep 3; echo 'done'".to_string(),
        ]);

        assert_eq!(command, "'sh' '-c' 'sleep 3; echo '\\''done'\\'''");
    }

    #[test]
    fn pane_job_log_tail_accepts_legacy_tail_equals_arg() {
        let args = vec!["tail=200".to_string()];
        assert_eq!(parse_job_log_tail(&args), Ok(Some(200)));
    }

    #[test]
    fn tail_text_returns_last_n_lines() {
        assert_eq!(tail_text("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail_text("a\nb\nc", 2), "b\nc");
        assert_eq!(tail_text("a\nb\nc", 0), "");
    }

    #[test]
    fn path_with_node_bin_prepends_existing_cwd_node_modules_bin() {
        let base = std::env::temp_dir().join(format!(
            "herdr-node-bin-test-{}",
            unix_millis(SystemTime::now())
        ));
        let node_bin = base.join("node_modules").join(".bin");
        std::fs::create_dir_all(&node_bin).unwrap();

        let existing = std::env::join_paths([std::path::PathBuf::from("/usr/bin")]).unwrap();
        let path = path_with_node_bin_from(&base, Some(&existing))
            .unwrap()
            .expect("node_modules/.bin should be prepended");
        let paths = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(paths.first(), Some(&node_bin));
        assert!(paths.contains(&std::path::PathBuf::from("/usr/bin")));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn path_with_node_bin_returns_none_when_missing_or_already_present() {
        let base = std::env::temp_dir().join(format!(
            "herdr-node-bin-test-{}",
            unix_millis(SystemTime::now())
        ));
        assert!(path_with_node_bin_from(&base, None).unwrap().is_none());

        let node_bin = base.join("node_modules").join(".bin");
        std::fs::create_dir_all(&node_bin).unwrap();
        let existing = std::env::join_paths([node_bin.clone()]).unwrap();
        assert!(path_with_node_bin_from(&base, Some(&existing))
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn run_default_label_uses_command_basename() {
        assert_eq!(
            default_run_label(&["/usr/bin/cargo".to_string(), "test".to_string()]),
            "cargo"
        );
    }

    #[test]
    fn pane_job_ids_reject_paths() {
        assert!(valid_pane_job_id("job-123_abc"));
        assert!(!valid_pane_job_id("../secret"));
        assert!(!valid_pane_job_id(""));
    }
}
