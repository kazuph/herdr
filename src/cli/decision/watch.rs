//! `herdr decision watch` — a long-running JSON-lines stream of pending and
//! resolved decisions.
//!
//! Without `--all-machines` the stream covers the local Herdr server: it emits
//! `machine_up`, re-emits every pending decision as `created`, then forwards
//! `decision.created`/`decision.resolved` as `created`/`resolved`. When the
//! server connection drops it emits `machine_down`, reconnects, and starts the
//! snapshot again so consumers only need `(machine.id, decision_id)` dedupe.
//!
//! With `--all-machines` one supervised SSH connection per enabled saved
//! machine runs `herdr --session <session> decision watch` remotely; every
//! forwarded line gets its `machine` object rewritten to the saved profile's
//! id and label. A failing machine only affects its own stream. A Tailscale
//! SSH `check` banner (`To authenticate, visit: <url>`) becomes a
//! `machine_down` whose reason carries the URL, the ssh child is killed, and
//! the machine retries after a fixed ten-minute wait — the watch never opens
//! a browser, unlike the interactive saved-machine connect path.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::api::client::{parse_response_value, ApiClient, ApiClientError, EventStream};
use crate::api::schema::{
    Decision, DecisionListParams, DecisionStatus, EventData, EventEnvelope, EventsSubscribeParams,
    Method, Request, ResponseResult, Subscription,
};
use crate::machine::{self, MachineProfile};

const LOCAL_MACHINE_ID: &str = "local";
const LOCAL_MACHINE_LABEL: &str = "Local";

/// Reconnect spacing for the local server socket: starts short and doubles
/// while connect attempts keep failing.
const LOCAL_RETRY_INITIAL: Duration = Duration::from_secs(1);
const LOCAL_RETRY_MAX: Duration = Duration::from_secs(10);
/// Retry spacing for saved-machine ssh streams. The interval doubles while a
/// machine keeps failing quickly and resets once a stream forwards lines.
const REMOTE_RETRY_INITIAL: Duration = Duration::from_secs(2);
const REMOTE_RETRY_MAX: Duration = Duration::from_secs(60);
/// Tailscale SSH `check` approvals need a human in a browser; minting a new
/// check URL faster than the approval can complete would only churn, so the
/// machine waits the contract's ten minutes before the next ssh attempt.
const TAILSCALE_AUTH_RETRY: Duration = Duration::from_secs(10 * 60);
/// How long the watch gives a child to exit after its stdout closed before
/// killing it.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(1);
/// Poll cadence for the per-connection stop watcher.
const STOP_WATCHER_POLL: Duration = Duration::from_millis(20);

pub(super) const DECISION_WATCH_USAGE: &str = "usage: herdr decision watch [--all-machines]";

/// Shared line sink. Production locks stdout, writes one line, and flushes;
/// tests record into a buffer. An error means the consumer went away.
type WatchSink = Arc<Mutex<dyn FnMut(&Value) -> io::Result<()> + Send>>;

/// Retry timing knobs. Production uses the contract values; tests shrink the
/// same code path instead of skipping it.
#[derive(Debug, Clone, Copy)]
struct WatchTimings {
    local_retry_initial: Duration,
    local_retry_max: Duration,
    remote_retry_initial: Duration,
    remote_retry_max: Duration,
    tailscale_auth_retry: Duration,
}

const PRODUCTION_TIMINGS: WatchTimings = WatchTimings {
    local_retry_initial: LOCAL_RETRY_INITIAL,
    local_retry_max: LOCAL_RETRY_MAX,
    remote_retry_initial: REMOTE_RETRY_INITIAL,
    remote_retry_max: REMOTE_RETRY_MAX,
    tailscale_auth_retry: TAILSCALE_AUTH_RETRY,
};

/// Doubling retry interval capped at `max`; `reset` after a healthy attempt.
struct Backoff {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl Backoff {
    fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            current: initial,
        }
    }

    fn next(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }
}

pub(super) fn run_watch(args: &[String]) -> io::Result<i32> {
    let mut all_machines = false;
    for arg in args {
        match arg.as_str() {
            "--all-machines" => all_machines = true,
            "help" | "--help" | "-h" => {
                eprintln!("{DECISION_WATCH_USAGE}");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("{DECISION_WATCH_USAGE}");
                return Ok(2);
            }
        }
    }

    start_stdout_close_monitor();
    let stop = Arc::new(AtomicBool::new(false));
    let sink = stdout_sink();

    if all_machines {
        let profiles: Vec<MachineProfile> = machine::load()
            .profiles
            .into_iter()
            .filter(|profile| profile.enabled)
            .collect();
        for profile in profiles {
            let sink = Arc::clone(&sink);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                pump_machine(&profile, &sink, &stop, PRODUCTION_TIMINGS);
            });
        }
    }

    let mut source = ApiDecisionWatchSource;
    pump_local(&mut source, &sink, &stop, PRODUCTION_TIMINGS);
    Ok(0)
}

fn stdout_sink() -> WatchSink {
    let stdout = std::io::stdout();
    Arc::new(Mutex::new(move |line: &Value| {
        let mut out = stdout.lock();
        writeln!(out, "{line}")?;
        out.flush()?;
        Ok(())
    }))
}

fn emit(sink: &WatchSink, line: &Value) -> io::Result<()> {
    (sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))(line)
}

/// Returns false when the sink failed: the whole watch stops on a broken
/// consumer pipe instead of writing into the void.
fn emit_or_stop(sink: &WatchSink, stop: &AtomicBool, line: &Value) -> bool {
    match emit(sink, line) {
        Ok(()) => true,
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            false
        }
    }
}

/// Exit quietly as soon as the pipe reading our stdout is gone, even while a
/// connection is idle and nothing is being written. Regular files and ttys
/// never report these flags, so interactive runs are unaffected.
#[cfg(unix)]
fn start_stdout_close_monitor() {
    thread::spawn(|| loop {
        let mut polled = libc::pollfd {
            fd: 1,
            events: 0,
            revents: 0,
        };
        let count = unsafe { libc::poll(&mut polled, 1, 200) };
        if count > 0 && polled.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            std::process::exit(0);
        }
    });
}

#[cfg(not(unix))]
fn start_stdout_close_monitor() {}

/// Sleep in short slices so a stop request (closed stdout) ends the wait
/// promptly instead of after the full retry interval.
fn sleep_interruptible(duration: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + duration;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn machine_object(id: &str, label: &str) -> Value {
    json!({ "id": id, "label": label })
}

fn local_machine_object() -> Value {
    machine_object(LOCAL_MACHINE_ID, LOCAL_MACHINE_LABEL)
}

fn machine_up_line(machine: &Value) -> Value {
    json!({ "event": "machine_up", "machine": machine })
}

fn machine_down_line(machine: &Value, reason: &str) -> Value {
    json!({ "event": "machine_down", "machine": machine, "reason": reason })
}

fn decision_line(event: &str, machine: &Value, decision: &Decision) -> Value {
    json!({ "event": event, "machine": machine, "decision": decision })
}

/// Contract events carried by `decision watch` lines. Anything else on a
/// remote stream is not ours to forward.
const WATCH_EVENTS: &[&str] = &["created", "resolved", "machine_up", "machine_down"];

fn is_watch_line(value: &Value) -> bool {
    value
        .get("event")
        .and_then(Value::as_str)
        .is_some_and(|event| WATCH_EVENTS.contains(&event))
}

/// Re-tag a remote watch line with the saved machine's identity. Lines that
/// are not contract events (ssh banners on stdout, remote usage errors) are
/// dropped rather than forwarded mangled.
fn rewrite_remote_line(raw: &str, profile: &MachineProfile) -> Option<Value> {
    let mut value: Value = serde_json::from_str(raw.trim()).ok()?;
    if !is_watch_line(&value) {
        return None;
    }
    *value.get_mut("machine")? = machine_object(&profile.id, &profile.label);
    Some(value)
}

/// Mirrors the banner parser in `remote::saved::process`: the saved-machine
/// connect path is owned by other work, so the watch keeps its own copy of
/// the (tiny) extraction and never invokes that path's browser opening.
fn tailscale_auth_url(stderr_so_far: &[u8]) -> Option<String> {
    const MARKER: &str = "To authenticate, visit:";
    let text = String::from_utf8_lossy(stderr_so_far);
    let rest = &text[text.find(MARKER)? + MARKER.len()..];
    let end = rest.find('\n')?;
    let url = rest[..end].trim();
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_owned())
}

// ---------------------------------------------------------------------------
// Local server pump
// ---------------------------------------------------------------------------

trait LocalWatchConnection {
    fn pending(&mut self) -> io::Result<Vec<Decision>>;
    fn next_event(&mut self) -> io::Result<Option<EventEnvelope>>;
}

trait LocalWatchSource {
    fn connect(&mut self) -> io::Result<Box<dyn LocalWatchConnection>>;
}

struct ApiDecisionWatchSource;

impl LocalWatchSource for ApiDecisionWatchSource {
    fn connect(&mut self) -> io::Result<Box<dyn LocalWatchConnection>> {
        let client = ApiClient::local();
        let (ack, stream) = client
            .subscribe_value(
                &Request {
                    id: "cli:decision:watch".to_string(),
                    method: Method::EventsSubscribe(EventsSubscribeParams {
                        subscriptions: vec![
                            Subscription::DecisionCreated {},
                            Subscription::DecisionResolved {},
                        ],
                    }),
                },
                None,
            )
            .map_err(api_error)?;
        parse_response_value(ack).map_err(api_error)?;
        Ok(Box::new(ApiWatchConnection { client, stream }))
    }
}

struct ApiWatchConnection {
    client: ApiClient,
    stream: EventStream,
}

impl LocalWatchConnection for ApiWatchConnection {
    fn pending(&mut self) -> io::Result<Vec<Decision>> {
        let response = self
            .client
            .request(Request {
                id: "cli:decision:watch:list".to_string(),
                method: Method::DecisionList(DecisionListParams {
                    status: Some(DecisionStatus::Pending),
                }),
            })
            .map_err(api_error)?;
        match response.result {
            ResponseResult::DecisionList { decisions } => Ok(decisions),
            other => Err(std::io::Error::other(format!(
                "unexpected decision.list result: {other:?}"
            ))),
        }
    }

    fn next_event(&mut self) -> io::Result<Option<EventEnvelope>> {
        let value = self.stream.next_value().map_err(api_error)?;
        value
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
            })
            .transpose()
    }
}

fn api_error(err: ApiClientError) -> io::Error {
    match err {
        ApiClientError::Io(err) => err,
        other => std::io::Error::other(other.to_string()),
    }
}

fn event_line(envelope: &EventEnvelope, machine: &Value) -> Option<Value> {
    let (event, decision) = match &envelope.data {
        EventData::DecisionCreated { decision } => ("created", decision),
        EventData::DecisionResolved { decision } => ("resolved", decision),
        _ => return None,
    };
    Some(decision_line(event, machine, decision))
}

fn pump_local(
    source: &mut dyn LocalWatchSource,
    sink: &WatchSink,
    stop: &AtomicBool,
    timings: WatchTimings,
) {
    let machine = local_machine_object();
    let mut backoff = Backoff::new(timings.local_retry_initial, timings.local_retry_max);
    while !stop.load(Ordering::Relaxed) {
        let mut connection = match source.connect() {
            Ok(connection) => {
                backoff.reset();
                connection
            }
            Err(err) => {
                if !emit_or_stop(
                    sink,
                    stop,
                    &machine_down_line(&machine, &format!("server connection failed: {err}")),
                ) {
                    return;
                }
                sleep_interruptible(backoff.next(), stop);
                continue;
            }
        };

        if !emit_or_stop(sink, stop, &machine_up_line(&machine)) {
            return;
        }

        match connection.pending() {
            Ok(decisions) => {
                let mut failed = false;
                for decision in &decisions {
                    if !emit_or_stop(sink, stop, &decision_line("created", &machine, decision)) {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    return;
                }
            }
            Err(err) => {
                if !emit_or_stop(
                    sink,
                    stop,
                    &machine_down_line(&machine, &format!("decision list failed: {err}")),
                ) {
                    return;
                }
                sleep_interruptible(backoff.next(), stop);
                continue;
            }
        }

        let reason = loop {
            if stop.load(Ordering::Relaxed) {
                break None;
            }
            match connection.next_event() {
                Ok(Some(envelope)) => {
                    if let Some(line) = event_line(&envelope, &machine) {
                        if !emit_or_stop(sink, stop, &line) {
                            return;
                        }
                    }
                }
                Ok(None) => break Some("server connection closed".to_string()),
                Err(err) => break Some(format!("server connection failed: {err}")),
            }
        };
        let Some(reason) = reason else {
            return;
        };
        if !emit_or_stop(sink, stop, &machine_down_line(&machine, &reason)) {
            return;
        }
        sleep_interruptible(backoff.next(), stop);
    }
}

// ---------------------------------------------------------------------------
// Saved-machine pump (--all-machines)
// ---------------------------------------------------------------------------

enum AttemptOutcome {
    Stopped,
    TailscaleAuth(String),
    Failed { reason: String, forwarded: bool },
}

/// One ssh `herdr decision watch` process plus the observation state needed
/// to classify how it ended.
struct SshWatchProcess {
    child: Arc<Mutex<Child>>,
    stdout: BufReader<std::process::ChildStdout>,
    auth_url: Arc<Mutex<Option<String>>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_join: Option<JoinHandle<()>>,
}

impl SshWatchProcess {
    fn next_line(&mut self) -> io::Result<Option<String>> {
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line)),
            Err(err) => Err(err),
        }
    }

    fn auth_url(&self) -> Option<String> {
        self.auth_url.lock().ok().and_then(|url| url.clone())
    }

    /// Kill and reap the child if it is still running, then join the stderr
    /// mirror so the captured tail is complete.
    fn terminate(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(join) = self.stderr_join.take() {
            let _ = join.join();
        }
    }

    /// Why the stream ended, for `machine_down.reason`. Waits briefly for a
    /// natural exit after stdout EOF before treating the child as stuck.
    fn failure_reason(&mut self, target: &str) -> String {
        let deadline = Instant::now() + CHILD_EXIT_GRACE;
        let status = loop {
            let outcome = self
                .child
                .lock()
                .map(|mut child| child.try_wait())
                .unwrap_or_else(|_| Err(std::io::Error::other("ssh child lock was poisoned")));
            match outcome {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        self.terminate();
                        break self
                            .child
                            .lock()
                            .ok()
                            .and_then(|mut child| child.try_wait().ok().flatten());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break None,
            }
        };
        if let Some(join) = self.stderr_join.take() {
            let _ = join.join();
        }
        let tail = self
            .stderr_tail
            .lock()
            .map(|tail| String::from_utf8_lossy(&tail).trim().to_string())
            .unwrap_or_default();
        let tail: String = tail.chars().take(240).collect();
        match (status, tail.is_empty()) {
            (Some(status), true) => format!("ssh to {target} exited with {status}"),
            (Some(status), false) => format!("ssh to {target} exited with {status}: {tail}"),
            (None, true) => format!("ssh to {target} closed its stream without exiting"),
            (None, false) => {
                format!("ssh to {target} closed its stream without exiting: {tail}")
            }
        }
    }
}

fn ssh_program() -> OsString {
    #[cfg(test)]
    if let Some(program) = test_ssh_program() {
        return program;
    }
    OsString::from("ssh")
}

#[cfg(test)]
fn test_ssh_program() -> Option<OsString> {
    TEST_SSH_PROGRAM
        .lock()
        .ok()
        .and_then(|program| program.clone())
}

#[cfg(test)]
static TEST_SSH_PROGRAM: Mutex<Option<OsString>> = Mutex::new(None);

/// Spawn `ssh <target> 'herdr' '--session' '<session>' 'decision' 'watch'`
/// through the same routed-argv builder `herdr --machine` uses, so the remote
/// binary override and ConnectTimeout stay consistent. stdin is closed so
/// ssh's BatchMode never waits on it. A mirror thread captures stderr,
/// notices a Tailscale `check` banner, and kills the child so the pump can
/// classify the attempt; a second watcher kills the child when the watch is
/// stopping so a blocking `read_line` unwinds.
fn spawn_remote(profile: &MachineProfile, stop: &Arc<AtomicBool>) -> io::Result<SshWatchProcess> {
    let argv = machine::build_routed_argv(profile, &["decision".to_string(), "watch".to_string()]);
    let mut command = Command::new(ssh_program());
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("ssh stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("ssh stderr was not piped"))?;

    let child = Arc::new(Mutex::new(child));
    let auth_url = Arc::new(Mutex::new(None::<String>));
    let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));

    let stderr_join = {
        let child = Arc::clone(&child);
        let auth_url = Arc::clone(&auth_url);
        let stderr_tail = Arc::clone(&stderr_tail);
        thread::spawn(move || {
            let mut reader = stderr;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        let mut tail = match stderr_tail.lock() {
                            Ok(tail) => tail,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        tail.extend_from_slice(&buf[..read]);
                        if auth_url.lock().map(|url| url.is_some()).unwrap_or(false) {
                            continue;
                        }
                        if let Some(url) = tailscale_auth_url(&tail) {
                            if let Ok(mut slot) = auth_url.lock() {
                                *slot = Some(url);
                            }
                            drop(tail);
                            // Do not wait for an approval the watch cannot
                            // drive: kill the ssh so the pump retries later.
                            if let Ok(mut child) = child.lock() {
                                let _ = child.kill();
                            }
                            return;
                        }
                    }
                }
            }
        })
    };

    {
        let child = Arc::clone(&child);
        let stop = Arc::clone(stop);
        thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
                return;
            }
            let exited = child
                .lock()
                .map(|mut child| {
                    child
                        .try_wait()
                        .map(|status| status.is_some())
                        .unwrap_or(true)
                })
                .unwrap_or(true);
            if exited {
                return;
            }
            thread::sleep(STOP_WATCHER_POLL);
        });
    }

    Ok(SshWatchProcess {
        child,
        stdout: BufReader::new(stdout),
        auth_url,
        stderr_tail,
        stderr_join: Some(stderr_join),
    })
}

fn pump_machine_once(
    profile: &MachineProfile,
    sink: &WatchSink,
    stop: &Arc<AtomicBool>,
) -> AttemptOutcome {
    let mut process = match spawn_remote(profile, stop) {
        Ok(process) => process,
        Err(err) => {
            return AttemptOutcome::Failed {
                reason: format!("failed to start ssh to {}: {err}", profile.target),
                forwarded: false,
            };
        }
    };

    let mut forwarded = false;
    let outcome = loop {
        if stop.load(Ordering::Relaxed) {
            break AttemptOutcome::Stopped;
        }
        match process.next_line() {
            Ok(Some(line)) => {
                if let Some(value) = rewrite_remote_line(&line, profile) {
                    if !emit_or_stop(sink, stop, &value) {
                        break AttemptOutcome::Stopped;
                    }
                    forwarded = true;
                }
            }
            Ok(None) => {
                if let Some(url) = process.auth_url() {
                    break AttemptOutcome::TailscaleAuth(url);
                }
                break AttemptOutcome::Failed {
                    reason: process.failure_reason(&profile.target),
                    forwarded,
                };
            }
            Err(err) => {
                break AttemptOutcome::Failed {
                    reason: format!("failed to read ssh output from {}: {err}", profile.target),
                    forwarded,
                };
            }
        }
    };
    process.terminate();
    outcome
}

fn pump_machine(
    profile: &MachineProfile,
    sink: &WatchSink,
    stop: &Arc<AtomicBool>,
    timings: WatchTimings,
) {
    let machine = machine_object(&profile.id, &profile.label);
    let mut backoff = Backoff::new(timings.remote_retry_initial, timings.remote_retry_max);
    while !stop.load(Ordering::Relaxed) {
        let delay = match pump_machine_once(profile, sink, stop) {
            AttemptOutcome::Stopped => break,
            AttemptOutcome::TailscaleAuth(url) => {
                if !emit_or_stop(
                    sink,
                    stop,
                    &machine_down_line(
                        &machine,
                        &format!("Tailscale SSH approval required: {url}"),
                    ),
                ) {
                    break;
                }
                timings.tailscale_auth_retry
            }
            AttemptOutcome::Failed { reason, forwarded } => {
                if !emit_or_stop(sink, stop, &machine_down_line(&machine, &reason)) {
                    break;
                }
                // A stream that forwarded lines was healthy until now; a
                // fast-failing one keeps doubling.
                if forwarded {
                    backoff.reset();
                }
                backoff.next()
            }
        };
        sleep_interruptible(delay, stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use crate::api::schema::{DecisionKind, DecisionOption, DecisionOptionRole, EventKind};

    fn test_timings() -> WatchTimings {
        WatchTimings {
            local_retry_initial: Duration::from_millis(1),
            local_retry_max: Duration::from_millis(5),
            remote_retry_initial: Duration::from_millis(1),
            remote_retry_max: Duration::from_millis(5),
            tailscale_auth_retry: Duration::from_millis(30),
        }
    }

    fn decision(id: &str, status: DecisionStatus) -> Decision {
        Decision {
            decision_id: id.to_string(),
            kind: DecisionKind::Ask,
            title: format!("title {id}"),
            body: None,
            options: vec![DecisionOption {
                id: "yes".to_string(),
                label: "Yes".to_string(),
                role: DecisionOptionRole::Approve,
            }],
            allow_text: false,
            origin: None,
            created_unix_ms: 1_700_000_000_000,
            expires_unix_ms: None,
            status,
            answer: None,
        }
    }

    fn recording_sink(
        lines: Arc<Mutex<Vec<Value>>>,
        limit: usize,
        stop: Arc<AtomicBool>,
    ) -> WatchSink {
        Arc::new(Mutex::new(move |line: &Value| {
            {
                lines
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(line.clone());
            }
            if lines.lock().map(|lines| lines.len()).unwrap_or(0) >= limit {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        }))
    }

    fn event_names(lines: &[Value]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line["event"].as_str().unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn watch_line_shapes_match_the_contract() {
        let machine = local_machine_object();
        assert_eq!(machine, json!({"id": "local", "label": "Local"}));

        let up = machine_up_line(&machine);
        assert_eq!(
            up,
            json!({"event": "machine_up", "machine": {"id": "local", "label": "Local"}})
        );

        let down = machine_down_line(&machine, "gone");
        assert_eq!(
            down,
            json!({"event": "machine_down", "machine": {"id": "local", "label": "Local"}, "reason": "gone"})
        );

        let pending = decision("dec-1", DecisionStatus::Pending);
        let created = decision_line("created", &machine, &pending);
        assert_eq!(created["event"], "created");
        assert_eq!(created["machine"], machine);
        assert_eq!(created["decision"]["decision_id"], "dec-1");
        assert_eq!(created["decision"]["status"], "pending");

        let resolved = decision_line(
            "resolved",
            &machine,
            &decision("dec-1", DecisionStatus::Answered),
        );
        assert_eq!(resolved["event"], "resolved");
        assert_eq!(resolved["decision"]["status"], "answered");
    }

    #[test]
    fn rewrite_remote_line_replaces_machine_and_drops_foreign_lines() {
        let profile = MachineProfile {
            id: "mabc".to_string(),
            label: "mini".to_string(),
            target: "mini".to_string(),
            session: "s".to_string(),
            enabled: true,
        };
        let remote_created = r#"{"event":"created","machine":{"id":"local","label":"Local"},"decision":{"decision_id":"dec-9","kind":"ask","title":"t","options":[],"allow_text":false,"created_unix_ms":1,"status":"pending"}}"#;
        let rewritten = rewrite_remote_line(remote_created, &profile).expect("rewritten");
        assert_eq!(rewritten["event"], "created");
        assert_eq!(rewritten["machine"], json!({"id": "mabc", "label": "mini"}));
        assert_eq!(rewritten["decision"]["decision_id"], "dec-9");

        for event in ["resolved", "machine_up", "machine_down"] {
            let raw = format!(
                r#"{{"event":"{event}","machine":{{"id":"local","label":"Local"}},"reason":"r"}}"#
            );
            let rewritten = rewrite_remote_line(&raw, &profile).expect("rewritten");
            assert_eq!(rewritten["machine"]["id"], "mabc");
            assert_eq!(rewritten["machine"]["label"], "mini");
        }

        assert!(rewrite_remote_line("not json", &profile).is_none());
        assert!(
            rewrite_remote_line(r#"{"event":"pane.created","machine":{}}"#, &profile).is_none()
        );
        assert!(rewrite_remote_line(r#"{"event":"created"}"#, &profile).is_none());
    }

    #[test]
    fn tailscale_auth_url_parses_check_banner() {
        assert_eq!(
            tailscale_auth_url(
                b"# To authenticate, visit: https://login.tailscale.com/a/abc123\n".as_slice()
            ),
            Some("https://login.tailscale.com/a/abc123".to_string())
        );
        assert_eq!(
            tailscale_auth_url(
                b"success.\n# To authenticate, visit: https://x.test/\nok\n".as_slice()
            ),
            Some("https://x.test/".to_string())
        );
        assert_eq!(tailscale_auth_url(b"no banner\n".as_slice()), None);
        // A banner without the trailing newline is not complete yet.
        assert_eq!(
            tailscale_auth_url(b"To authenticate, visit: https://x.test/".as_slice()),
            None
        );
        assert_eq!(
            tailscale_auth_url(b"To authenticate, visit: javascript:alert(1)\n".as_slice()),
            None
        );
    }

    #[test]
    fn tailscale_auth_retry_is_the_contract_ten_minutes() {
        assert_eq!(TAILSCALE_AUTH_RETRY, Duration::from_secs(600));
    }

    struct FakeConnection {
        pending: io::Result<Vec<Decision>>,
        events: VecDeque<io::Result<Option<EventEnvelope>>>,
    }

    impl LocalWatchConnection for FakeConnection {
        fn pending(&mut self) -> io::Result<Vec<Decision>> {
            match &self.pending {
                Ok(decisions) => Ok(decisions.clone()),
                Err(err) => Err(io::Error::other(err.to_string())),
            }
        }

        fn next_event(&mut self) -> io::Result<Option<EventEnvelope>> {
            self.events.pop_front().unwrap_or(Ok(None))
        }
    }

    struct FakeSource {
        steps: Mutex<VecDeque<io::Result<FakeConnection>>>,
    }

    impl FakeSource {
        fn exhausted() -> io::Error {
            io::Error::other("fake source exhausted")
        }
    }

    impl LocalWatchSource for FakeSource {
        fn connect(&mut self) -> io::Result<Box<dyn LocalWatchConnection>> {
            let step = self
                .steps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or_else(|| Err(Self::exhausted()));
            match step {
                Ok(connection) => Ok(Box::new(connection)),
                Err(err) => Err(err),
            }
        }
    }

    fn conn(
        pending: Vec<Decision>,
        events: Vec<io::Result<Option<EventEnvelope>>>,
    ) -> io::Result<FakeConnection> {
        Ok(FakeConnection {
            pending: Ok(pending),
            events: events.into(),
        })
    }

    fn created_envelope(id: &str) -> io::Result<Option<EventEnvelope>> {
        Ok(Some(EventEnvelope {
            event: EventKind::DecisionCreated,
            data: EventData::DecisionCreated {
                decision: decision(id, DecisionStatus::Pending),
            },
        }))
    }

    fn resolved_envelope(id: &str) -> io::Result<Option<EventEnvelope>> {
        Ok(Some(EventEnvelope {
            event: EventKind::DecisionResolved,
            data: EventData::DecisionResolved {
                decision: decision(id, DecisionStatus::Answered),
            },
        }))
    }

    #[test]
    fn local_pump_snapshots_streams_and_recovers_after_disconnect() {
        let first = conn(
            vec![
                decision("dec-1", DecisionStatus::Pending),
                decision("dec-2", DecisionStatus::Pending),
            ],
            vec![
                created_envelope("dec-3"),
                resolved_envelope("dec-1"),
                Ok(None), // connection dropped
            ],
        );
        let unreachable = Err(io::Error::other("connect refused"));
        let second = conn(
            vec![decision("dec-4", DecisionStatus::Pending)],
            vec![Ok(None)],
        );
        let source = FakeSource {
            steps: Mutex::new(VecDeque::from([first, unreachable, second])),
        };

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = recording_sink(Arc::clone(&lines), 10, Arc::clone(&stop));
        let mut source = source;
        pump_local(&mut source, &sink, &stop, test_timings());

        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            event_names(&lines),
            vec![
                "machine_up",
                "created",
                "created",
                "created",
                "resolved",
                "machine_down",
                "machine_down",
                "machine_up",
                "created",
                "machine_down",
            ]
        );
        assert_eq!(lines[1]["decision"]["decision_id"], "dec-1");
        assert_eq!(lines[2]["decision"]["decision_id"], "dec-2");
        assert_eq!(lines[3]["decision"]["decision_id"], "dec-3");
        assert_eq!(lines[4]["decision"]["status"], "answered");
        assert!(lines[5]["reason"].as_str().unwrap().contains("closed"));
        assert!(lines[6]["reason"]
            .as_str()
            .unwrap()
            .contains("connect refused"));
        assert_eq!(lines[8]["decision"]["decision_id"], "dec-4");
        for line in lines.iter() {
            assert_eq!(line["machine"], json!({"id": "local", "label": "Local"}));
        }
    }

    #[test]
    fn local_pump_emits_down_when_pending_list_fails() {
        let flaky = Ok(FakeConnection {
            pending: Err(io::Error::other("list boom")),
            events: VecDeque::new(),
        });
        let source = FakeSource {
            steps: Mutex::new(VecDeque::from([flaky])),
        };
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = recording_sink(Arc::clone(&lines), 3, Arc::clone(&stop));
        let mut source = source;
        pump_local(&mut source, &sink, &stop, test_timings());
        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            event_names(&lines),
            vec!["machine_up", "machine_down", "machine_down"]
        );
        assert!(lines[1]["reason"].as_str().unwrap().contains("list boom"));
    }

    // -- fake ssh -----------------------------------------------------------

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-watch-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// A per-target scriptable `ssh`. Behaviour files live in
    /// `FAKE_WATCH_SSH_DIR`:
    ///   <target>.banner   → print `To authenticate, visit: <contents>` on
    ///                       stderr, then wait like tailscale ssh `check`
    ///   <target>.stderr   → write contents to stderr
    ///   <target>.exit     → exit with the contained status code
    ///   <target>.stdout   → write contents to stdout
    ///   <target>.linger   → sleep the contained seconds, then exit
    /// Every invocation appends the target to `attempts.log`.
    fn install_fake_ssh(dir: &std::path::Path) {
        let script = r#"#!/usr/bin/env bash
dir="$FAKE_WATCH_SSH_DIR"
target=""
prev=""
for arg in "$@"; do
    if [ "$prev" = "--" ]; then target="$arg"; break; fi
    prev="$arg"
done
echo "$target" >> "$dir/attempts.log"
if [ -f "$dir/$target.banner" ]; then
    printf '# To authenticate, visit: %s\n' "$(cat "$dir/$target.banner")" >&2
    exec sleep 60
fi
if [ -f "$dir/$target.stderr" ]; then cat "$dir/$target.stderr" >&2; fi
if [ -f "$dir/$target.exit" ]; then exit "$(cat "$dir/$target.exit")"; fi
if [ -f "$dir/$target.stdout" ]; then cat "$dir/$target.stdout"; fi
if [ -f "$dir/$target.linger" ]; then exec sleep "$(cat "$dir/$target.linger")"; fi
exit 0
"#;
        let path = dir.join("ssh");
        std::fs::write(&path, script).expect("fake ssh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake ssh");
        }
        *TEST_SSH_PROGRAM
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path.into_os_string());
        std::env::set_var("FAKE_WATCH_SSH_DIR", dir);
    }

    fn profile(id: &str, label: &str, target: &str) -> MachineProfile {
        MachineProfile {
            id: id.to_string(),
            label: label.to_string(),
            target: target.to_string(),
            session: "work".to_string(),
            enabled: true,
        }
    }

    fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    fn attempts(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("attempts.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn remote_contract_lines(decision_id: &str) -> String {
        let up = r#"{"event":"machine_up","machine":{"id":"local","label":"Local"}}"#;
        let created = format!(
            r#"{{"event":"created","machine":{{"id":"local","label":"Local"}},"decision":{{"decision_id":"{decision_id}","kind":"ask","title":"t","options":[],"allow_text":false,"created_unix_ms":1,"status":"pending"}}}}"#
        );
        let resolved = format!(
            r#"{{"event":"resolved","machine":{{"id":"local","label":"Local"}},"decision":{{"decision_id":"{decision_id}","kind":"ask","title":"t","options":[],"allow_text":false,"created_unix_ms":1,"status":"answered","answer":{{"answered_unix_ms":2}}}}}}"#
        );
        format!("{up}\n{created}\n{resolved}\n")
    }

    #[test]
    fn remote_pump_forwards_rewritten_lines_then_machine_down() {
        let dir = temp_dir("rewrites");
        install_fake_ssh(&dir);
        std::fs::write(dir.join("good.stdout"), remote_contract_lines("dec-a"))
            .expect("stdout fixture");

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = recording_sink(Arc::clone(&lines), 4, Arc::clone(&stop));
        pump_machine(
            &profile("mgood", "Good Machine", "good"),
            &sink,
            &stop,
            test_timings(),
        );

        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            event_names(&lines),
            vec!["machine_up", "created", "resolved", "machine_down"]
        );
        for line in lines.iter() {
            assert_eq!(
                line["machine"],
                json!({"id": "mgood", "label": "Good Machine"})
            );
        }
        assert_eq!(lines[1]["decision"]["decision_id"], "dec-a");
        assert_eq!(lines[2]["decision"]["status"], "answered");
        assert!(lines[3]["reason"].is_string());
    }

    #[test]
    fn one_machine_failure_does_not_stop_other_streams() {
        let dir = temp_dir("isolation");
        install_fake_ssh(&dir);
        std::fs::write(
            dir.join("dead.stderr"),
            "ssh: connect to host dead: refused\n",
        )
        .expect("stderr fixture");
        std::fs::write(dir.join("dead.exit"), "255").expect("exit fixture");
        std::fs::write(dir.join("live.stdout"), remote_contract_lines("dec-live"))
            .expect("stdout fixture");
        std::fs::write(dir.join("live.linger"), "1").expect("linger fixture");

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        // No line-count stop here: the dead machine's machine_down spam must
        // not consume the budget before the live machine's lines arrive.
        let sink = recording_sink(Arc::clone(&lines), usize::MAX, Arc::clone(&stop));

        let dead_sink = Arc::clone(&sink);
        let dead_stop = Arc::clone(&stop);
        let dead = thread::spawn(move || {
            pump_machine(
                &profile("mdead", "Dead", "dead"),
                &dead_sink,
                &dead_stop,
                test_timings(),
            );
        });
        let live_sink = Arc::clone(&sink);
        let live_stop = Arc::clone(&stop);
        let live = thread::spawn(move || {
            pump_machine(
                &profile("mlive", "Live", "live"),
                &live_sink,
                &live_stop,
                test_timings(),
            );
        });
        let observed = wait_for(
            || {
                let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
                lines
                    .iter()
                    .any(|line| line["machine"]["id"] == "mlive" && line["event"] == "created")
                    && lines.iter().any(|line| {
                        line["machine"]["id"] == "mdead" && line["event"] == "machine_down"
                    })
            },
            Duration::from_secs(10),
        );
        stop.store(true, Ordering::Relaxed);
        dead.join().expect("dead pump");
        live.join().expect("live pump");
        assert!(observed, "live stream survived the dead machine's retries");

        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        let dead_downs = lines
            .iter()
            .filter(|line| line["event"] == "machine_down" && line["machine"]["id"] == "mdead")
            .count();
        assert!(dead_downs >= 1, "dead machine emitted machine_down");
        let live_events: Vec<&Value> = lines
            .iter()
            .filter(|line| line["machine"]["id"] == "mlive")
            .collect();
        assert!(
            live_events
                .iter()
                .any(|line| line["event"] == "created"
                    && line["decision"]["decision_id"] == "dec-live"),
            "live machine's created line was forwarded"
        );
        assert!(
            live_events.iter().any(|line| line["event"] == "machine_up"),
            "live machine's machine_up was forwarded"
        );
    }

    #[test]
    fn tailscale_banner_downs_machine_with_url_and_retries_without_browser() {
        let dir = temp_dir("tailscale");
        install_fake_ssh(&dir);
        std::fs::write(
            dir.join("tailscale.banner"),
            "https://login.tailscale.com/a/watch-test",
        )
        .expect("banner fixture");

        // Any browser open would run `open` (macOS) or `xdg-open` (Linux);
        // point both at a shim that records the attempt.
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let marker = dir.join("browser-opened");
        for tool in ["open", "xdg-open", "wslview", "cmd"] {
            let path = bin.join(tool);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"$@\" >> {}\n", marker.display()),
            )
            .expect("open shim");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod open shim");
            }
        }
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{original_path}", bin.display()));

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = recording_sink(Arc::clone(&lines), 2, Arc::clone(&stop));
        let pump_stop = Arc::clone(&stop);
        let pump = thread::spawn(move || {
            pump_machine(
                &profile("mtail", "Tail", "tailscale"),
                &sink,
                &pump_stop,
                test_timings(),
            );
        });
        pump.join().expect("tailscale pump");

        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            lines.iter().all(|line| line["event"] == "machine_down"),
            "auth check never produces machine_up/created lines: {lines:?}"
        );
        for line in lines.iter() {
            let reason = line["reason"].as_str().unwrap_or("");
            assert!(
                reason.contains("https://login.tailscale.com/a/watch-test"),
                "reason carries the check url: {reason}"
            );
            assert!(
                reason.contains("Tailscale"),
                "reason explains authentication is required: {reason}"
            );
            assert_eq!(line["machine"]["id"], "mtail");
        }
        assert!(!marker.exists(), "watch never invoked a browser opener");
        assert!(
            !attempts(&dir).is_empty(),
            "ssh was attempted at least once"
        );
        std::env::set_var("PATH", original_path);
    }

    #[test]
    fn tailscale_retry_waits_before_attempting_again() {
        let dir = temp_dir("tailscale-retry");
        install_fake_ssh(&dir);
        std::fs::write(
            dir.join("ts.banner"),
            "https://login.tailscale.com/a/retry-check",
        )
        .expect("banner fixture");

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        // auth_retry = 30ms in test_timings: a second attempt should appear
        // only after ~30ms, proving the delay knob gates the retry.
        let sink = recording_sink(Arc::clone(&lines), 2, Arc::clone(&stop));
        let started = Instant::now();
        let pump_stop = Arc::clone(&stop);
        let pump = thread::spawn(move || {
            pump_machine(
                &profile("mts", "Ts", "ts"),
                &sink,
                &pump_stop,
                test_timings(),
            );
        });
        pump.join().expect("retry pump");
        let elapsed = started.elapsed();

        let line_count = lines.lock().map(|l| l.len()).unwrap_or(0);
        assert!(line_count >= 2, "two machine_down lines for two attempts");
        assert!(
            attempts(&dir).len() >= 2,
            "a second ssh attempt happened after the auth wait"
        );
        assert!(
            elapsed >= Duration::from_millis(30),
            "retry respected the auth wait: {elapsed:?}"
        );
    }

    #[test]
    fn remote_pump_retries_after_fast_failure() {
        let dir = temp_dir("retry");
        install_fake_ssh(&dir);
        std::fs::write(dir.join("flap.stderr"), "connection reset\n").expect("stderr");
        std::fs::write(dir.join("flap.exit"), "1").expect("exit");

        let lines = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = recording_sink(Arc::clone(&lines), 3, Arc::clone(&stop));
        pump_machine(
            &profile("mflap", "Flap", "flap"),
            &sink,
            &stop,
            test_timings(),
        );
        let lines = lines.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            lines.iter().all(|line| line["event"] == "machine_down"),
            "fast failures only emit machine_down"
        );
        assert!(attempts(&dir).len() >= 3, "retries kept attempting ssh");
        for line in lines.iter() {
            assert!(
                line["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("connection reset"),
                "reason carries ssh stderr: {line}"
            );
        }
    }
}
