use std::collections::HashMap;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api::schema::{
    EmptyParams, Method, MsgHistoryParams, MsgInboxParams, MsgSendParams, PaneCurrentParams,
    PaneRenameParams, PaneSendInputParams, PaneSplitParams, PaneTarget, Request, SplitDirection,
};

const JOB_LOG_TAIL_CHARS: usize = 20_000;
#[cfg(unix)]
const JOB_CANCEL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const JOB_CANCEL_WAIT_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Serialize)]
struct RunSpawnOutput {
    job: String,
    label: String,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
}

pub(super) fn run_msg_command(args: &[String]) -> std::io::Result<i32> {
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

pub(super) fn run_job_command(args: &[String]) -> std::io::Result<i32> {
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
            job_log(&args[1..])
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

pub(super) fn msg_send(args: &[String]) -> std::io::Result<i32> {
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
                print_send_help();
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
    let response = super::send_request(&Request {
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

pub(super) fn msg_inbox(args: &[String]) -> std::io::Result<i32> {
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
    let response = super::send_request(&Request {
        id: "cli:msg:inbox".into(),
        method: Method::MsgInbox(MsgInboxParams {
            room,
            to_agent: identity.agent,
        }),
    })?;
    print_msg_messages_response(&response, "inbox")
}

pub(super) fn herdr_log(args: &[String]) -> std::io::Result<i32> {
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
        Some("-f") => msg_tail(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_log_help();
            Ok(0)
        }
        Some(job_id) if !job_id.starts_with('-') && valid_job_id(job_id) => job_log(args),
        _ => log_timeline(args),
    }
}

pub(super) fn herdr_run(args: &[String]) -> std::io::Result<i32> {
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
                split = super::parse_split_direction(value)?;
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
                caller = Some(super::normalize_pane_id(value));
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
    if pane_mode && completion_set {
        eprintln!("--completion applies only to pane-less background jobs");
        return Ok(2);
    }
    if !pane_mode && close_on_success {
        eprintln!("--close-on-success requires --pane");
        return Ok(2);
    }

    let command_args = &args[index..];
    let label = label.unwrap_or_else(|| default_run_label(command_args));
    let caller = match resolve_run_caller(caller.as_deref()) {
        Ok(caller) => caller,
        Err(err) => {
            eprintln!("unable to resolve caller pane: {err}");
            eprintln!("pass --caller <pane> explicitly; do not infer from focused pane");
            return Ok(1);
        }
    };

    if pane_mode {
        run_in_pane(command_args, label, cwd, split, caller, close_on_success)
    } else {
        run_background(command_args, label, cwd, caller, completion)
    }
}

pub(super) fn background_runner(args: &[String]) -> std::io::Result<i32> {
    let Some(job_id_index) = args.iter().position(|arg| arg == "--job-id") else {
        eprintln!(
            "usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>"
        );
        return Ok(2);
    };
    let Some(job_id) = args.get(job_id_index + 1).filter(|id| valid_job_id(id)) else {
        eprintln!(
            "usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>"
        );
        return Ok(2);
    };
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        eprintln!(
            "usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>"
        );
        return Ok(2);
    };
    let command_args = &args[separator + 1..];
    let Some((program, program_args)) = command_args.split_first() else {
        eprintln!(
            "usage: herdr __background-run --job-id ID [--completion summary|full|none] -- <argv...>"
        );
        return Ok(2);
    };
    let completion_override = parse_completion_override(&args[..separator])?;
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
    let log = open_job_log(&log_path)?;
    write_job_header(
        &log,
        job_id,
        Some(std::process::id()),
        &job.command,
        &job.cwd,
        started,
    )?;

    let mut command = Command::new(program);
    command
        .args(program_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path_with_cwd_node_bin()? {
        command.env("PATH", path);
    }

    let code = match command.spawn() {
        Ok(mut child) => wait_with_logged_output(&mut child, log.clone(), std::io::sink())?,
        Err(err) => {
            if let Ok(mut log) = log.lock() {
                let _ = writeln!(log, "[runner] failed to spawn: {err}");
            }
            store
                .mark_finished(job_id, Some(127), unix_millis(SystemTime::now()))
                .map_err(std::io::Error::other)?;
            enqueue_job_completion(&job, Some(127), &log_path)?;
            return Ok(127);
        }
    };

    let finished = SystemTime::now();
    write_job_footer(&log, finished, code)?;
    if store
        .mark_finished(job_id, code, unix_millis(finished))
        .map_err(std::io::Error::other)?
    {
        enqueue_job_completion(&job, code, &log_path)?;
    }
    Ok(code.unwrap_or(1))
}

pub(super) fn pane_runner(args: &[String]) -> std::io::Result<i32> {
    let mut parent_agent = None;
    let mut target = None;
    let mut job_id = None;
    let mut label = None;
    let mut close_on_success = false;
    let mut close_on_exit = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--parent-agent" => {
                parent_agent = args.get(index + 1).cloned();
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
                label = args.get(index + 1).cloned();
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
    let Some(job_id) = job_id.filter(|id| valid_job_id(id)) else {
        eprintln!(
            "usage: herdr __pane-run --parent-agent NAME --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    };
    let Some(target) = target else {
        eprintln!(
            "usage: herdr __pane-run --parent-agent NAME --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    };
    if index >= args.len() {
        eprintln!(
            "usage: herdr __pane-run --parent-agent NAME --target PANE --job-id ID -- <command>"
        );
        return Ok(2);
    }
    let command = args[index..].join(" ");
    let label = label.unwrap_or_else(|| default_run_label(&args[index..]));
    let log_path = job_log_path(&job_id)?;
    let log = open_job_log(&log_path)?;
    let started = SystemTime::now();
    write_job_header(&log, &job_id, None, &command, "", started)?;

    let mut child = Command::new("/bin/sh");
    child.arg("-lc").arg(&command);
    if let Some(path) = path_with_cwd_node_bin()? {
        child.env("PATH", path);
    }
    let code = match child.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(mut child) => wait_with_logged_output(&mut child, log.clone(), std::io::stdout())?,
        Err(err) => {
            eprintln!("failed to spawn command: {err}");
            Some(127)
        }
    };
    write_job_footer(&log, SystemTime::now(), code)?;

    if let Some(parent_agent) = parent_agent {
        let body = pane_run_notification_line(&label, &target, &job_id, code, "");
        let _ = send_job_message(&parent_agent, "", body, None);
    }
    if close_on_exit || (close_on_success && code == Some(0)) {
        let _ = super::send_ok_request(Method::PaneClose(PaneTarget { pane_id: target }));
    }
    Ok(code.unwrap_or(1))
}

fn run_background(
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
    let caller_agent = resolve_agent_name(&caller, "cli:run:agent")?;
    let job = new_job_id();
    let log_path = job_log_path(&job)?;
    let record = crate::job::JobRecord {
        id: job.clone(),
        label: label.clone(),
        command: shell_command_from_args(command_args),
        cwd: cwd.display().to_string(),
        caller_pane: caller,
        caller_agent,
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

    let mut runner = Command::new(std::env::current_exe()?);
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
    #[cfg(unix)]
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

fn run_in_pane(
    command_args: &[String],
    label: String,
    cwd: Option<String>,
    split: SplitDirection,
    caller: String,
    close_on_success: bool,
) -> std::io::Result<i32> {
    let caller_agent = resolve_agent_name(&caller, "cli:run:pane-agent")?;
    let split_response = super::send_request(&Request {
        id: "cli:run:split".into(),
        method: Method::PaneSplit(PaneSplitParams {
            workspace_id: None,
            target_pane_id: Some(caller),
            direction: split,
            ratio: None,
            cwd,
            focus: false,
            env: HashMap::new(),
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
    let _ = super::send_ok_request(Method::PaneRename(PaneRenameParams {
        pane_id: pane.clone(),
        label: Some(label.clone()),
    }));

    let job = new_job_id();
    let mut runner_parts = vec![
        shell_quote(&std::env::current_exe()?.display().to_string()),
        "__pane-run".to_string(),
        "--parent-agent".to_string(),
        shell_quote(&caller_agent),
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
    runner_parts.push(shell_quote(&shell_command_from_args(command_args)));
    let submit = super::send_request(&Request {
        id: "cli:run:pane-submit".into(),
        method: Method::PaneSendInput(PaneSendInputParams {
            pane_id: pane.clone(),
            text: runner_parts.join(" "),
            keys: vec!["Enter".into()],
        }),
    })?;
    if submit.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&submit).unwrap());
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
    if !valid_job_id(job_id) {
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
    if !valid_job_id(job_id) {
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
    #[cfg(not(unix))]
    {
        let _ = pid;
        eprintln!("job cancellation is not implemented on this platform");
        return Ok(1);
    }
    #[cfg(unix)]
    let process_group = unsafe { libc::getpgid(pid as i32) };
    #[cfg(unix)]
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
    #[cfg(unix)]
    signal_process_group(pid, libc::SIGTERM)?;
    #[cfg(unix)]
    let mut escalated = false;
    #[cfg(not(unix))]
    let escalated = false;
    #[cfg(unix)]
    if !wait_for_process_group_exit(pid, JOB_CANCEL_WAIT_TIMEOUT) {
        escalated = true;
        signal_process_group(pid, libc::SIGKILL)?;
        if !wait_for_process_group_exit(pid, JOB_CANCEL_WAIT_TIMEOUT) {
            eprintln!(
                "job {job_id} process group {pid} survived SIGKILL; status remains cancelling"
            );
            return Ok(1);
        }
    }
    let cancelled = store
        .mark_cancelled(job_id, unix_millis(SystemTime::now()))
        .map_err(std::io::Error::other)?;
    if cancelled {
        enqueue_job_completion(&job, None, std::path::Path::new(&job.log_path))?;
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

#[cfg(unix)]
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

#[cfg(unix)]
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
        std::thread::sleep(JOB_CANCEL_WAIT_POLL);
    }
}

fn job_log(args: &[String]) -> std::io::Result<i32> {
    let Some(job_id) = args.first() else {
        eprintln!("usage: herdr log <job_id> [--tail N|tail=N]");
        return Ok(2);
    };
    if !valid_job_id(job_id) {
        eprintln!("usage: herdr log <job_id> [--tail N|tail=N]");
        return Ok(2);
    }
    let tail = parse_job_log_tail(&args[1..])?;
    let log_path = crate::job::JobStore::open_active()
        .and_then(|store| store.get(job_id))
        .ok()
        .flatten()
        .map(|job| std::path::PathBuf::from(job.log_path))
        .unwrap_or(job_log_path(job_id)?);
    let text = std::fs::read_to_string(&log_path).map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!("failed to read {}: {err}", log_path.display()),
        )
    })?;
    if let Some(lines) = tail {
        print!("{}", tail_text(&text, lines));
    } else {
        print!("{text}");
    }
    Ok(0)
}

fn msg_history(args: &[String]) -> std::io::Result<i32> {
    let (room, project, limit) = parse_msg_history_args(args)?;
    let response = super::send_request(&Request {
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
    let (room, project, limit) = parse_msg_history_args(args)?;
    let mut seen_max = 0_i64;
    loop {
        let response = super::send_request(&Request {
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
        for message in response["result"]["messages"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let id = message["id"].as_i64().unwrap_or(0);
            if id > seen_max {
                seen_max = id;
                print_msg_message(message);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn msg_rooms(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr log rooms");
        return Ok(2);
    }
    let response = super::send_request(&Request {
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

fn log_timeline(args: &[String]) -> std::io::Result<i32> {
    let (room, project, limit) = parse_msg_history_args(args)?;
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
    for (id, kind, room, from, to, body, label, status, exit_code, created_at) in rows {
        if kind == "command" {
            println!(
                "#{id} [{room}] {created_at} command {from} -> {to}: {} status={} exit={}",
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
        .and_then(|pane_id| pane_info(pane_id).ok())
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
    Ok(MsgIdentity {
        agent: resolve_agent_name(&pane_id, "cli:msg:identity")?,
        project,
    })
}

fn resolve_run_caller(explicit: Option<&str>) -> std::io::Result<String> {
    if let Some(pane_id) = explicit {
        let pane_id = super::normalize_pane_id(pane_id);
        let response = pane_info(&pane_id)?;
        if let Some(error) = response.get("error") {
            return Err(std::io::Error::other(error.to_string()));
        }
        return Ok(response["result"]["pane"]["global_id"]
            .as_str()
            .unwrap_or(&pane_id)
            .to_string());
    }
    resolve_current_pane_id()
}

fn resolve_current_pane_id() -> std::io::Result<String> {
    let caller_pane_id = std::env::var(crate::integration::HERDR_PANE_ID_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| super::normalize_pane_id(value.trim()));
    let response = super::send_request(&Request {
        id: "cli:pane:current".into(),
        method: Method::PaneCurrent(PaneCurrentParams {
            caller_pane_id,
            caller_process_id: Some(std::process::id()),
        }),
    })?;
    if let Some(error) = response.get("error") {
        return Err(std::io::Error::other(error.to_string()));
    }
    response["result"]["pane"]["global_id"]
        .as_str()
        .or_else(|| response["result"]["pane"]["pane_id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("pane.current response did not include pane id"))
}

fn pane_info(pane_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&Request {
        id: "cli:g9:pane".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_string(),
        }),
    })
}

fn resolve_agent_name(target: &str, request_id: &str) -> std::io::Result<String> {
    let response = super::send_request(&Request {
        id: request_id.into(),
        method: Method::AgentGet(crate::api::schema::AgentTarget {
            target: target.to_string(),
        }),
    })?;
    if let Some(error) = response.get("error") {
        return Err(std::io::Error::other(error.to_string()));
    }
    response["result"]["agent"]["name"]
        .as_str()
        .or_else(|| response["result"]["agent"]["agent"].as_str())
        .or_else(|| response["result"]["agent"]["global_pane_id"].as_str())
        .or_else(|| response["result"]["agent"]["pane_id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("target has no reported agent identity"))
}

fn send_job_message(
    to_agent: &str,
    project: &str,
    body: String,
    reply_to: Option<i64>,
) -> std::io::Result<()> {
    let response = super::send_request(&Request {
        id: "cli:g9:job-message".into(),
        method: Method::MsgSend(MsgSendParams {
            room: crate::msg::JOBS_ROOM.into(),
            project: project.into(),
            from_agent: "herdr-run".into(),
            to: to_agent.into(),
            body,
            reply_to,
        }),
    });
    if matches!(response, Ok(ref value) if value.get("error").is_none()) {
        return Ok(());
    }
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
        format!("{summary}\n\n{}", std::fs::read_to_string(log_path)?)
    } else {
        summary
    };
    let reply_to = crate::dispatch::DispatchStore::open_active()
        .ok()
        .and_then(|store| store.command_dispatch_id(&job.id).ok().flatten());
    if send_job_message(&job.caller_agent, &job.cwd, body.clone(), reply_to).is_ok() {
        return Ok(());
    }
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

fn parse_completion_override(args: &[String]) -> std::io::Result<Option<String>> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--completion" {
            let Some(value) = args.get(index + 1) else {
                return Err(std::io::Error::other("missing value for --completion"));
            };
            return Ok(Some(value.clone()));
        }
        index += 1;
    }
    Ok(None)
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
                    return Err(std::io::Error::other("missing value for --room"));
                };
                room = value.clone();
                index += 2;
            }
            "--project" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(std::io::Error::other("missing value for --project"));
                };
                project = Some(value.clone());
                index += 2;
            }
            "--limit" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(std::io::Error::other("missing value for --limit"));
                };
                limit = super::parse_u32_flag("--limit", value)?;
                index += 2;
            }
            "help" | "--help" | "-h" => {
                print_log_help();
                return Err(std::io::Error::other("help"));
            }
            other => {
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

fn open_job_log(path: &std::path::Path) -> std::io::Result<Arc<Mutex<std::fs::File>>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Arc::new(Mutex::new(std::fs::File::create(path)?)))
}

fn write_job_header(
    log: &Arc<Mutex<std::fs::File>>,
    job_id: &str,
    runner_pid: Option<u32>,
    command: &str,
    cwd: &str,
    started: SystemTime,
) -> std::io::Result<()> {
    let mut log = log
        .lock()
        .map_err(|_| std::io::Error::other("job log lock poisoned"))?;
    writeln!(log, "job_id: {job_id}")?;
    if let Some(pid) = runner_pid {
        writeln!(log, "runner_pid: {pid}")?;
    }
    writeln!(log, "command: {command}")?;
    if !cwd.is_empty() {
        writeln!(log, "cwd: {cwd}")?;
    }
    writeln!(log, "started_unix_ms: {}", unix_millis(started))?;
    writeln!(log)?;
    Ok(())
}

fn write_job_footer(
    log: &Arc<Mutex<std::fs::File>>,
    finished: SystemTime,
    code: Option<i32>,
) -> std::io::Result<()> {
    let mut log = log
        .lock()
        .map_err(|_| std::io::Error::other("job log lock poisoned"))?;
    writeln!(log)?;
    writeln!(log, "finished_unix_ms: {}", unix_millis(finished))?;
    writeln!(log, "exit_code: {}", exit_code_label(code))?;
    Ok(())
}

fn wait_with_logged_output<W: Write + Send + 'static>(
    child: &mut std::process::Child,
    log: Arc<Mutex<std::fs::File>>,
    output: W,
) -> std::io::Result<Option<i32>> {
    let tail = Arc::new(Mutex::new(String::new()));
    let stdout = child
        .stdout
        .take()
        .map(|stdout| stream_job_output(stdout, output, log.clone(), tail.clone(), "stdout"));
    let stderr = child
        .stderr
        .take()
        .map(|stderr| stream_job_output(stderr, std::io::stderr(), log.clone(), tail, "stderr"));
    let status = child.wait()?;
    if let Some(thread) = stdout {
        let _ = thread.join();
    }
    if let Some(thread) = stderr {
        let _ = thread.join();
    }
    Ok(status.code())
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
    if count > JOB_LOG_TAIL_CHARS {
        *tail = tail.chars().skip(count - JOB_LOG_TAIL_CHARS).collect();
    }
}

fn parse_job_log_tail(args: &[String]) -> std::io::Result<Option<usize>> {
    match args {
        [] => Ok(None),
        [arg] if arg.starts_with("tail=") => parse_tail_value(&arg["tail=".len()..]).map(Some),
        [flag, value] if flag == "--tail" => parse_tail_value(value).map(Some),
        _ => {
            eprintln!("usage: herdr log <job_id> [--tail N|tail=N]");
            Ok(None)
        }
    }
}

fn parse_tail_value(value: &str) -> std::io::Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| std::io::Error::other("invalid tail value"))
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

fn pane_run_notification_line(
    label: &str,
    pane_id: &str,
    job_id: &str,
    code: Option<i32>,
    _sample: &str,
) -> String {
    format!(
        "[herdr run] exit={} label={} pane={} details: herdr log {}",
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
    current_path: Option<&std::ffi::OsStr>,
) -> std::io::Result<Option<std::ffi::OsString>> {
    let node_bin = cwd.join("node_modules").join(".bin");
    if !node_bin.is_dir() {
        return Ok(None);
    }
    let mut paths = current_path
        .map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default();
    if paths.iter().any(|path| path == &node_bin) {
        return Ok(None);
    }
    paths.insert(0, node_bin);
    std::env::join_paths(paths)
        .map(Some)
        .map_err(|err| std::io::Error::other(format!("failed to construct PATH: {err}")))
}

fn shell_command_from_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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

fn one_line_field(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("_")
}

fn exit_code_label(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

fn new_job_id() -> String {
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

fn valid_job_id(job_id: &str) -> bool {
    !job_id.is_empty()
        && job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn job_log_path(job_id: &str) -> std::io::Result<std::path::PathBuf> {
    Ok(crate::session::data_dir()
        .join("job-logs")
        .join(format!("{job_id}.log")))
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
    print_data_footer();
}

fn print_job_help() {
    eprintln!("herdr job commands:");
    eprintln!("  herdr job list");
    eprintln!("  herdr job status <job_id>");
    eprintln!("  herdr job log <job_id> [--tail N|tail=N]");
    eprintln!("  herdr job cancel <job_id>");
}

fn print_run_help() {
    eprintln!("usage: herdr run [--label TEXT] [--cwd PATH] [--caller <pane>] [--completion summary|full|none] [--pane [--split right|down] [--close-on-success]] -- <command...>");
    eprintln!("  default: starts a pane-less background job and returns its job id immediately");
    eprintln!("  --pane starts the command in a visible same-space pane");
    eprintln!("  inspect background jobs with `herdr run list`, `herdr log <job_id>`, and `herdr run cancel <job_id>`");
    eprintln!("  caller resolution fails closed; pass --caller <pane> when needed");
    print_data_footer();
}

fn print_data_footer() {
    eprintln!(
        "data: {} (WAL mode; safe to query while running)",
        crate::dispatch::DispatchStore::active_path().display()
    );
    eprintln!("lowest-level API: sqlite3 \"$(herdr log --db)\"");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_notify_tail_sample_keeps_bounded_chars() {
        let tail = Arc::new(Mutex::new(String::new()));
        append_tail_sample(&tail, &"a".repeat(JOB_LOG_TAIL_CHARS + 100));

        let sample = tail.lock().unwrap().clone();
        assert_eq!(sample.chars().count(), JOB_LOG_TAIL_CHARS);
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
    fn run_notification_line_is_single_line_and_points_to_job_log() {
        let line =
            pane_run_notification_line("cargo test", "p_2", "job-123", Some(0), "hello\nworld\n");

        assert_eq!(
            line,
            "[herdr run] exit=0 label=cargo_test pane=p_2 details: herdr log job-123"
        );
        assert!(!line.contains('\n'));
        assert!(!line.contains("tail="));
    }

    #[test]
    fn pane_job_log_tail_accepts_legacy_tail_equals_arg() {
        let args = vec!["tail=200".to_string()];
        assert_eq!(parse_job_log_tail(&args).unwrap(), Some(200));
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
            "herdr-node-bin-test-{}-{}",
            std::process::id(),
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

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn path_with_node_bin_returns_none_when_missing_or_already_present() {
        let base = std::env::temp_dir().join(format!(
            "herdr-node-bin-test-{}-{}",
            std::process::id(),
            unix_millis(SystemTime::now())
        ));
        assert!(path_with_node_bin_from(&base, None).unwrap().is_none());

        let node_bin = base.join("node_modules").join(".bin");
        std::fs::create_dir_all(&node_bin).unwrap();
        let existing = std::env::join_paths([node_bin.clone()]).unwrap();
        assert!(path_with_node_bin_from(&base, Some(&existing))
            .unwrap()
            .is_none());

        std::fs::remove_dir_all(&base).unwrap();
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
        assert!(valid_job_id("job-123_abc"));
        assert!(!valid_job_id("../secret"));
        assert!(!valid_job_id(""));
    }
}
