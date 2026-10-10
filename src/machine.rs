//! Saved SSH machine catalog (G11, P1).
//!
//! Stores named SSH connection profiles (`label`, SSH target, explicit Herdr
//! session, enabled state) in `machines.json` next to the session data dir.
//! Only the observable surface of upstream `herdr machine` is re-implemented
//! (no cherry-pick): the remote preparation pipeline, metadata cache, and
//! open-client propagation are intentionally not adopted (see SPEC G11).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// SSH probe binary and timing. BatchMode never prompts; a bounded
/// ConnectTimeout keeps stalls fail-closed instead of hanging the CLI.
pub(crate) const SSH_CONNECT_TIMEOUT_SECS: u64 = 15;

/// Environment override for the remote herdr executable used by `--machine`
/// routing (P2). Defaults to `herdr` on the remote `PATH`.
pub(crate) const MACHINE_REMOTE_BINARY_ENV_VAR: &str = "HERDR_MACHINE_REMOTE_BINARY";

/// A single saved SSH machine profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineProfile {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) session: String,
    pub(crate) enabled: bool,
}

/// The persisted catalog. Serialized as a JSON array of profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineCatalog {
    #[serde(default)]
    pub(crate) profiles: Vec<MachineProfile>,
}

fn new_profile_id(label: &str, target: &str, session: &str, existing: &[MachineProfile]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let mut hasher = DefaultHasher::new();
        label.hash(&mut hasher);
        target.hash(&mut hasher);
        session.hash(&mut hasher);
        nanos.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        attempt.hash(&mut hasher);
        let id = format!("m{:012x}", hasher.finish() & 0xffff_ffff_ffff);
        if !existing.iter().any(|profile| profile.id == id) {
            return id;
        }
    }
    // Practically unreachable: fall back to a nanos-suffixed id.
    format!("m{nanos:x}")
}

fn validate_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("--label is required".to_string());
    }
    if label.len() > 120 {
        return Err("--label must be at most 120 characters".to_string());
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("missing SSH target".to_string());
    }
    if target.starts_with('-') {
        return Err("SSH target must not start with '-'".to_string());
    }
    Ok(())
}

impl MachineCatalog {
    /// Insert a profile. Rejects empty/oversized labels, bad targets, duplicate
    /// labels, and duplicate `(target, session)` pairs. Returns the new id.
    pub(crate) fn add(
        &mut self,
        label: &str,
        target: &str,
        session: &str,
    ) -> Result<String, String> {
        validate_label(label)?;
        validate_target(target)?;
        if self.profiles.iter().any(|p| p.label == label) {
            return Err(format!("machine label {label:?} is already saved"));
        }
        if self
            .profiles
            .iter()
            .any(|p| p.target == target && p.session == session)
        {
            return Err(format!(
                "machine for target {target:?} and session {session:?} is already saved"
            ));
        }
        let id = new_profile_id(label, target, session, &self.profiles);
        self.profiles.push(MachineProfile {
            id: id.clone(),
            label: label.to_string(),
            target: target.to_string(),
            session: session.to_string(),
            enabled: true,
        });
        Ok(id)
    }

    /// Remove a profile by exact id. Returns false when not found.
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        self.profiles.len() != before
    }

    /// Rename a profile by exact id. Returns false when not found.
    pub(crate) fn rename(&mut self, id: &str, label: &str) -> Result<bool, String> {
        validate_label(label)?;
        if self.profiles.iter().any(|p| p.id != id && p.label == label) {
            return Err(format!("machine label {label:?} is already saved"));
        }
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(profile) => {
                profile.label = label.to_string();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Enable/disable a profile by exact id. Returns false when not found.
    pub(crate) fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(profile) => {
                profile.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Resolve a label-or-id to a profile, including disabled ones.
    /// Exact id wins; a label is accepted only when it matches exactly one
    /// profile. Unknown and ambiguous references are rejected (fail-closed).
    pub(crate) fn resolve_any(&self, label_or_id: &str) -> Result<&MachineProfile, String> {
        if let Some(profile) = self.profiles.iter().find(|p| p.id == label_or_id) {
            return Ok(profile);
        }
        let mut matches = self.profiles.iter().filter(|p| p.label == label_or_id);
        match (matches.next(), matches.next()) {
            (Some(profile), None) => Ok(profile),
            (Some(_), Some(_)) => Err(format!(
                "machine label {label_or_id:?} is ambiguous; use the profile id"
            )),
            (None, _) => Err(format!("machine {label_or_id:?} was not found")),
        }
    }

    /// Resolve a label-or-id to an enabled profile. Exact id wins; otherwise
    /// the label must match exactly one profile. Disabled, unknown, and
    /// ambiguous references are rejected (fail-closed, no guessing).
    pub(crate) fn resolve(&self, label_or_id: &str) -> Result<&MachineProfile, String> {
        Self::require_enabled(self.resolve_any(label_or_id)?)
    }

    fn require_enabled(profile: &MachineProfile) -> Result<&MachineProfile, String> {
        if profile.enabled {
            Ok(profile)
        } else {
            Err(format!(
                "machine {:?} is disabled; enable it before use",
                profile.label
            ))
        }
    }
}

pub(crate) fn default_path() -> PathBuf {
    crate::session::data_dir().join("machines.json")
}

fn save_json_to_path(path: &Path, catalog: &MachineCatalog) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&catalog.profiles)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    #[cfg(windows)]
    if path.exists() {
        if let Err(err) = std::fs::remove_file(path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// Load the catalog from the default path. Missing or corrupt files yield an
/// empty catalog so a bad file never blocks CLI startup.
pub(crate) fn load() -> MachineCatalog {
    load_from_path(&default_path())
}

pub(crate) fn load_from_path(path: &Path) -> MachineCatalog {
    if !path.exists() {
        return MachineCatalog::default();
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to read machine catalog");
            return MachineCatalog::default();
        }
    };
    match serde_json::from_str::<Vec<MachineProfile>>(&content) {
        Ok(profiles) => MachineCatalog { profiles },
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to parse machine catalog, starting empty");
            MachineCatalog::default()
        }
    }
}

/// Live clients must distinguish a missing catalog from a read/parse failure.
/// The legacy CLI loader above deliberately retains its corrupt-file-as-empty contract.
pub(crate) fn load_profiles_result(path: &Path) -> std::io::Result<Vec<MachineProfile>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    serde_json::from_str(&content)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Persist the catalog to the default path. Write failures are returned, never
/// swallowed.
pub(crate) fn save(catalog: &MachineCatalog) -> std::io::Result<()> {
    save_to_path(&default_path(), catalog)
}

pub(crate) fn save_to_path(path: &Path, catalog: &MachineCatalog) -> std::io::Result<()> {
    save_json_to_path(path, catalog)
}

/// Probe SSH reachability without prompts and with a bounded timeout.
/// Failure means the machine must not be saved (fail-closed).
pub(crate) fn probe_ssh_reachable(target: &str) -> std::io::Result<()> {
    probe_ssh_reachable_with_timeout(target, SSH_CONNECT_TIMEOUT_SECS)
}

fn probe_ssh_reachable_with_timeout(target: &str, timeout_secs: u64) -> std::io::Result<()> {
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={timeout_secs}"))
        .arg("--")
        .arg(target)
        .arg("true")
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "ssh probe to {target:?} failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Remote herdr executable name for `--machine` routing (P2).
pub(crate) fn remote_binary() -> String {
    std::env::var(MACHINE_REMOTE_BINARY_ENV_VAR)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "herdr".to_string())
}

/// Subcommands that may be routed to a saved machine (G11 P2).
/// Everything else is rejected so local-only commands (`run`, `inbox`,
/// `machine` itself, …) can never silently execute against the wrong host.
/// `decision` is routable so `decision answer` can resolve a saved machine's
/// pending decision; `local` stays reserved for this machine.
pub(crate) const ROUTABLE_SUBCOMMANDS: &[&str] =
    &["agent", "decision", "pane", "workspace", "worktree"];

/// The `--machine` value that means "this machine" for the `decision`
/// subcommand, matching the `local` machine id `decision watch` emits. Other
/// subcommands keep the existing saved-profile resolution for that label.
pub(crate) const LOCAL_MACHINE_ROUTE: &str = "local";

/// True when the route names the reserved local machine for a subcommand
/// that supports a local target, meaning no SSH hop should happen.
pub(crate) fn route_targets_local(args: &[String], route: &MachineRoute) -> bool {
    route.label_or_id == LOCAL_MACHINE_ROUTE && args.get(1).map(String::as_str) == Some("decision")
}

/// Parsed `--machine <label-or-id>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineRoute {
    pub(crate) label_or_id: String,
}

/// Split `--machine` out of the argv, mirroring the `--remote` extraction
/// shape. Returns the cleaned argv plus the routing request, if any.
pub(crate) fn extract_machine_args(
    args: &[String],
) -> Result<(Vec<String>, Option<MachineRoute>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }
    // `herdr run --machine` keeps the job local and only runs the command
    // remotely, so `run` parses its own `--machine`.
    if args.get(1).map(String::as_str) == Some("run") {
        return Ok((args.to_vec(), None));
    }
    let mut route = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--machine" {
            if route.is_some() {
                return Err("--machine can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --machine".to_string());
            };
            if value.is_empty() || value.starts_with('-') {
                return Err("--machine requires a saved machine label or id".to_string());
            }
            route = Some(MachineRoute {
                label_or_id: value.clone(),
            });
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--machine=") {
            if route.is_some() {
                return Err("--machine can only be specified once".to_string());
            }
            if value.is_empty() || value.starts_with('-') {
                return Err("--machine requires a saved machine label or id".to_string());
            }
            route = Some(MachineRoute {
                label_or_id: value.to_string(),
            });
            index += 1;
            continue;
        }
        cleaned.push(arg.clone());
        index += 1;
    }
    Ok((cleaned, route))
}

/// POSIX single-quote one argv element so a remote login shell reconstructs
/// the original argument. Every element is quoted (no "safe unquoted" path)
/// so spaces, quotes, `;`, `$()`, and empty strings stay literal.
fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Join argv into a single remote-shell command string.
fn posix_shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| posix_shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the ssh argv that executes the subcommand on the saved machine.
///
/// OpenSSH concatenates extra arguments into one remote-command string and
/// the remote login shell parses it, so raw argv boundaries are not
/// preserved. Each remote argv element is POSIX single-quoted and passed as
/// that single command string. Pure so the wire shape is unit-testable
/// without a host.
pub(crate) fn build_routed_argv(
    profile: &MachineProfile,
    subcommand_args: &[String],
) -> Vec<String> {
    let mut remote_argv = vec![
        remote_binary(),
        "--session".to_string(),
        profile.session.clone(),
    ];
    remote_argv.extend(subcommand_args.iter().cloned());
    vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
        "--".to_string(),
        profile.target.clone(),
        posix_shell_join(&remote_argv),
    ]
}

/// Where a `herdr run --machine` command runs on the saved machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteDir {
    /// A path relative to the remote user's home directory (empty = home).
    Home(String),
    /// An absolute remote path.
    Absolute(String),
}

impl RemoteDir {
    /// Map a local directory onto the saved machine: paths under the local
    /// home keep their home-relative position, other paths stay absolute.
    pub(crate) fn from_local(dir: &Path, home: Option<&Path>) -> Self {
        if let Some(rest) = home.and_then(|home| dir.strip_prefix(home).ok()) {
            return Self::Home(rest.to_string_lossy().into_owned());
        }
        Self::Absolute(dir.to_string_lossy().into_owned())
    }

    /// Parse an explicit `--cwd` for a remote job: `~` / `~/x` are home
    /// relative, absolute paths stay absolute, anything else is relative to
    /// the remote home.
    pub(crate) fn from_remote_arg(value: &str) -> Self {
        if value == "~" {
            Self::Home(String::new())
        } else if let Some(rest) = value.strip_prefix("~/") {
            Self::Home(rest.trim_end_matches('/').to_string())
        } else if value.starts_with('/') {
            Self::Absolute(value.to_string())
        } else {
            Self::Home(value.trim_end_matches('/').to_string())
        }
    }

    /// Shell expression that expands to the directory on the remote host.
    fn shell_expr(&self) -> String {
        match self {
            Self::Home(rest) if rest.is_empty() => "\"$HOME\"".to_string(),
            Self::Home(rest) => format!("\"$HOME\"/{}", posix_shell_quote(rest)),
            Self::Absolute(path) => posix_shell_quote(path),
        }
    }

    /// rsync destination path (rsync resolves relative paths from home).
    fn rsync_path(&self) -> String {
        match self {
            Self::Home(rest) if rest.is_empty() => ".".to_string(),
            Self::Home(rest) => rest.clone(),
            Self::Absolute(path) => path.clone(),
        }
    }
}

/// Local work tree copied to the saved machine before a remote job runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteSync {
    pub(crate) local_dir: PathBuf,
    /// Remote copy of `local_dir`; usually contains the job's directory.
    pub(crate) remote_dir: RemoteDir,
    /// rsync exclude patterns. Excluded paths are never sent and never
    /// deleted on the remote side, so remote build caches survive.
    pub(crate) excludes: Vec<String>,
    /// Delete remote files that no longer exist locally. Only enabled when
    /// the excludes come from git, so ignored remote files stay protected.
    pub(crate) delete: bool,
}

fn ssh_options() -> Vec<String> {
    vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
    ]
}

/// Build the local argv for `herdr run --machine`: the job (log, status,
/// completion notice) stays local while the command runs on the saved
/// machine through ssh, inside the remote user's interactive login shell so
/// it sees the same PATH as a terminal there. `interactive` allocates a tty
/// for `--pane` runs; background runs get no stdin.
pub(crate) fn build_remote_job_argv(
    profile: &MachineProfile,
    dir: &RemoteDir,
    command: &[String],
    sync: Option<&RemoteSync>,
    interactive: bool,
) -> Vec<String> {
    let inner = format!(
        "cd {} && exec {}",
        dir.shell_expr(),
        posix_shell_join(command)
    );
    let remote = format!(
        "exec \"${{SHELL:-/bin/sh}}\" -lic {}",
        posix_shell_quote(&inner)
    );
    let mut ssh = vec!["ssh".to_string()];
    ssh.extend(ssh_options());
    ssh.push(if interactive { "-t" } else { "-n" }.to_string());
    ssh.extend(["--".to_string(), profile.target.clone(), remote]);
    let Some(sync) = sync else {
        return ssh;
    };

    let mut mkdir = vec!["ssh".to_string()];
    mkdir.extend(ssh_options());
    mkdir.extend([
        "-n".to_string(),
        "--".to_string(),
        profile.target.clone(),
        format!("mkdir -p {}", sync.remote_dir.shell_expr()),
    ]);
    let mut rsync = vec![
        "rsync".to_string(),
        "-a".to_string(),
        "-e".to_string(),
        format!("ssh {}", ssh_options().join(" ")),
    ];
    if sync.delete {
        rsync.push("--delete".to_string());
    }
    rsync.push("--exclude=/.git".to_string());
    rsync.extend(
        sync.excludes
            .iter()
            .map(|pattern| format!("--exclude={pattern}")),
    );
    rsync.push(format!("{}/", sync.local_dir.to_string_lossy()));
    rsync.push(format!(
        "{}:{}/",
        profile.target,
        sync.remote_dir.rsync_path()
    ));
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{} && {} && exec {}",
            posix_shell_join(&mkdir),
            posix_shell_join(&rsync),
            posix_shell_join(&ssh)
        ),
    ]
}

/// What `--machine` wants: local help or a routed subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MachineRequest {
    LocalHelp,
    Route { subcommand: String },
}

/// Refuse anything that must never execute locally or remotely.
/// Pure so the fail-closed gate is unit-testable without a host.
pub(crate) fn classify_machine_request(args: &[String]) -> Result<MachineRequest, (String, i32)> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(MachineRequest::LocalHelp);
    }
    let Some(subcommand) = args.get(1).map(String::as_str) else {
        return Err(("--machine requires a subcommand".to_string(), 2));
    };
    if !ROUTABLE_SUBCOMMANDS.contains(&subcommand) {
        return Err((
            "--machine can only route agent, decision, pane, workspace, or worktree".to_string(),
            2,
        ));
    }
    Ok(MachineRequest::Route {
        subcommand: subcommand.to_string(),
    })
}

/// Execute a routed subcommand, inheriting stdio and the remote exit code.
/// Any transport failure is an error: the caller must never fall back to
/// local execution (G11 P2).
pub(crate) fn run_routed(
    profile: &MachineProfile,
    subcommand_args: &[String],
) -> std::io::Result<i32> {
    let argv = build_routed_argv(profile, subcommand_args);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|err| {
            std::io::Error::other(format!(
                "failed to reach saved machine {:?}: {err}; not falling back to local execution",
                profile.label
            ))
        })?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-machine-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("test temp dir");
        dir.join("machines.json")
    }

    #[test]
    fn add_rejects_bad_and_duplicate_profiles() {
        let mut catalog = MachineCatalog::default();
        assert!(catalog.add("", "host", "default").is_err());
        assert!(catalog.add("a", "", "default").is_err());
        assert!(catalog.add("a", "-oProxy", "default").is_err());
        let id = catalog.add("office", "ssh.example.com", "default").unwrap();
        assert!(id.starts_with('m'));
        assert!(catalog
            .add("office", "other.example.com", "default")
            .is_err());
        assert!(catalog.add("other", "ssh.example.com", "default").is_err());
        // Same target with a different session is a distinct profile.
        assert!(catalog.add("office2", "ssh.example.com", "s2").is_ok());
        assert_eq!(catalog.profiles.len(), 2);
    }

    #[test]
    fn rename_remove_enable_roundtrip() {
        let mut catalog = MachineCatalog::default();
        let id = catalog.add("a", "h1", "default").unwrap();
        assert!(!catalog.remove("missing"));
        assert!(!catalog.set_enabled("missing", false));
        assert!(!catalog.rename("missing", "x").unwrap());
        assert!(catalog.rename(&id, "b").is_ok());
        assert_eq!(catalog.resolve("b").unwrap().id, id);
        assert!(catalog.set_enabled(&id, false));
        assert!(catalog.resolve("b").is_err());
        assert!(catalog.set_enabled(&id, true));
        assert!(catalog.remove(&id));
        assert!(catalog.resolve("b").is_err());
    }

    #[test]
    fn resolve_rejects_unknown_and_ambiguous_references() {
        let mut catalog = MachineCatalog::default();
        assert!(catalog.resolve("nope").is_err());
        let id1 = catalog.add("dup", "h1", "default").unwrap();
        // Force a second profile with the same label (bypasses add's guard)
        // to prove ambiguity is rejected instead of first-match wins.
        catalog.profiles.push(MachineProfile {
            id: "mdeadbeef000".to_string(),
            label: "dup".to_string(),
            target: "h2".to_string(),
            session: "default".to_string(),
            enabled: true,
        });
        assert!(catalog.resolve("dup").is_err());
        // Exact id still wins over the ambiguous label.
        assert_eq!(catalog.resolve(&id1).unwrap().target, "h1");
    }

    #[test]
    fn resolve_any_accepts_disabled_and_rejects_ambiguous_labels() {
        let mut catalog = MachineCatalog::default();
        let id1 = catalog.add("office", "h1", "default").unwrap();
        assert!(catalog.set_enabled(&id1, false));
        // Unique disabled label is resolvable for enable/disable.
        assert_eq!(catalog.resolve_any("office").unwrap().id, id1);
        assert!(catalog.resolve("office").is_err());
        assert!(catalog.set_enabled(&catalog.resolve_any("office").unwrap().id.clone(), true));
        assert!(catalog.resolve("office").is_ok());
        assert!(catalog.set_enabled(&id1, false));
        // Exact id still works while disabled.
        assert_eq!(catalog.resolve_any(&id1).unwrap().target, "h1");

        // Corrupt/legacy duplicate labels stay fail-closed for enable.
        catalog.profiles.push(MachineProfile {
            id: "mdeadbeef000".to_string(),
            label: "office".to_string(),
            target: "h2".to_string(),
            session: "s2".to_string(),
            enabled: false,
        });
        let err = catalog.resolve_any("office").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        // Exact id still wins over the ambiguous label, including a label
        // that equals another profile's id.
        assert_eq!(catalog.resolve_any(&id1).unwrap().target, "h1");
        assert_eq!(catalog.resolve_any("mdeadbeef000").unwrap().target, "h2");
        catalog.profiles.push(MachineProfile {
            id: "mlabelasid000".to_string(),
            label: id1.clone(),
            target: "h3".to_string(),
            session: "s3".to_string(),
            enabled: false,
        });
        assert_eq!(catalog.resolve_any(&id1).unwrap().target, "h1");
        assert_eq!(catalog.resolve_any("mlabelasid000").unwrap().target, "h3");
    }

    #[test]
    fn catalog_persists_and_tolerates_corrupt_files() {
        let path = temp_path("persist");
        let _ = std::fs::remove_file(&path);
        assert!(load_from_path(&path).profiles.is_empty());
        let mut catalog = MachineCatalog::default();
        let id = catalog.add("office", "h1", "default").unwrap();
        save_to_path(&path, &catalog).unwrap();
        let loaded = load_from_path(&path);
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].id, id);
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_from_path(&path).profiles.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ssh_probe_to_unroutable_host_fails_fast() {
        let err = probe_ssh_reachable_with_timeout("invalid.invalid.invalid", 2).unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn remote_binary_defaults_to_herdr() {
        assert_eq!(remote_binary(), "herdr");
    }

    fn office() -> MachineProfile {
        MachineProfile {
            id: "m000000000001".to_string(),
            label: "office".to_string(),
            target: "office-host".to_string(),
            session: "default".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn remote_dir_maps_home_relative_and_absolute_paths() {
        let home = Path::new("/Users/me");
        assert_eq!(
            RemoteDir::from_local(Path::new("/Users/me/src/app"), Some(home)),
            RemoteDir::Home("src/app".to_string())
        );
        assert_eq!(
            RemoteDir::from_local(Path::new("/Users/me"), Some(home)),
            RemoteDir::Home(String::new())
        );
        assert_eq!(
            RemoteDir::from_local(Path::new("/opt/work"), Some(home)),
            RemoteDir::Absolute("/opt/work".to_string())
        );
        assert_eq!(
            RemoteDir::from_remote_arg("~"),
            RemoteDir::Home(String::new())
        );
        assert_eq!(
            RemoteDir::from_remote_arg("~/src/app/"),
            RemoteDir::Home("src/app".to_string())
        );
        assert_eq!(
            RemoteDir::from_remote_arg("/srv/app"),
            RemoteDir::Absolute("/srv/app".to_string())
        );
        assert_eq!(
            RemoteDir::from_remote_arg("src/app"),
            RemoteDir::Home("src/app".to_string())
        );
    }

    #[test]
    fn remote_job_argv_runs_quoted_command_in_remote_login_shell() {
        let command = vec!["echo".to_string(), "it's $HOME".to_string()];
        let argv = build_remote_job_argv(
            &office(),
            &RemoteDir::Home("src/app".to_string()),
            &command,
            None,
            false,
        );
        assert_eq!(
            &argv[..6],
            &[
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
                "-n"
            ]
        );
        assert_eq!(&argv[6..8], &["--", "office-host"]);
        let remote = &argv[8];
        // The remote shell expands $HOME for the directory but receives the
        // command arguments literally through two quoting layers.
        let inner = r#"cd "$HOME"/'src/app' && exec 'echo' 'it'\''s $HOME'"#;
        assert_eq!(
            remote,
            &format!(
                "exec \"${{SHELL:-/bin/sh}}\" -lic {}",
                posix_shell_quote(inner)
            )
        );

        let pane = build_remote_job_argv(
            &office(),
            &RemoteDir::Absolute("/srv/app".to_string()),
            &command,
            None,
            true,
        );
        assert_eq!(pane[5], "-t");
        assert!(pane[8].contains("cd '\\''/srv/app'\\''"));
    }

    #[test]
    fn remote_job_argv_syncs_before_running_and_protects_ignored_paths() {
        let sync = RemoteSync {
            local_dir: PathBuf::from("/Users/me/src/app"),
            remote_dir: RemoteDir::Home("src/app".to_string()),
            excludes: vec!["/target/".to_string(), "node_modules".to_string()],
            delete: true,
        };
        let argv = build_remote_job_argv(
            &office(),
            &RemoteDir::Home("src/app/sub".to_string()),
            &["just".to_string(), "ci".to_string()],
            Some(&sync),
            false,
        );
        assert_eq!(&argv[..2], &["sh", "-c"]);
        let script = &argv[2];
        let mkdir = script.find("'mkdir -p").unwrap();
        let rsync = script.find("'rsync' '-a'").unwrap();
        let run = script.find("exec 'ssh'").unwrap();
        assert!(mkdir < rsync && rsync < run);
        assert!(script.contains(
            "'--delete' '--exclude=/.git' '--exclude=/target/' '--exclude=node_modules'"
        ));
        assert!(script.contains("'/Users/me/src/app/' 'office-host:src/app/'"));
        assert!(script.contains("src/app/sub"));

        let copy_only = RemoteSync {
            delete: false,
            excludes: Vec::new(),
            ..sync
        };
        let argv = build_remote_job_argv(
            &office(),
            &RemoteDir::Home("src/app".to_string()),
            &["ls".to_string()],
            Some(&copy_only),
            false,
        );
        assert!(!argv[2].contains("--delete"));
    }

    #[test]
    fn run_keeps_its_own_machine_flag() {
        let argv: Vec<String> = ["herdr", "run", "--machine", "office", "--", "ls"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        let (cleaned, route) = extract_machine_args(&argv).unwrap();
        assert_eq!(route, None);
        assert_eq!(cleaned, argv);
    }

    #[test]
    fn extract_machine_args_parses_and_cleans() {
        let argv = vec![
            "herdr".to_string(),
            "--machine".to_string(),
            "office".to_string(),
            "agent".to_string(),
            "list".to_string(),
        ];
        let (cleaned, route) = extract_machine_args(&argv).unwrap();
        assert_eq!(
            route,
            Some(MachineRoute {
                label_or_id: "office".to_string()
            })
        );
        assert_eq!(cleaned, vec!["herdr", "agent", "list"]);

        let argv = vec![
            "herdr".to_string(),
            "--machine=office".to_string(),
            "pane".to_string(),
        ];
        let (cleaned, route) = extract_machine_args(&argv).unwrap();
        assert!(route.is_some());
        assert_eq!(cleaned, vec!["herdr", "pane"]);

        // After `--` the flag is positional, not routing.
        let argv = vec![
            "herdr".to_string(),
            "run".to_string(),
            "--".to_string(),
            "--machine".to_string(),
        ];
        let (cleaned, route) = extract_machine_args(&argv).unwrap();
        assert_eq!(route, None);
        assert_eq!(cleaned, argv);

        assert!(extract_machine_args(&["herdr".to_string(), "--machine".to_string()]).is_err());
        assert!(extract_machine_args(&[
            "herdr".to_string(),
            "--machine".to_string(),
            "a".to_string(),
            "--machine".to_string(),
            "b".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn classify_machine_request_gates_routing() {
        use MachineRequest::*;
        let route = |argv: &[&str]| {
            classify_machine_request(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            route(&["herdr", "agent", "list"]),
            Ok(Route {
                subcommand: "agent".to_string()
            })
        );
        for subcommand in ["decision", "pane", "workspace", "worktree"] {
            assert!(matches!(route(&["herdr", subcommand]), Ok(Route { .. })));
        }
        // `decision answer` keeps its full argument tail on the remote argv.
        assert_eq!(
            route(&["herdr", "decision", "answer", "dec-1", "yes"]),
            Ok(Route {
                subcommand: "decision".to_string()
            })
        );
        assert_eq!(route(&["herdr", "--help"]), Ok(LocalHelp));
        // Anything else is refused before any resolve/spawn, so a refusal
        // can never fall through to local execution.
        for argv in [
            vec!["herdr"],
            vec!["herdr", "run", "--", "true"],
            vec!["herdr", "machine", "list"],
            vec!["herdr", "inbox"],
            vec!["herdr", "send", "p1", "hi"],
        ] {
            let Err((_, code)) = route(&argv) else {
                panic!("must refuse {argv:?}");
            };
            assert_eq!(code, 2);
        }
    }

    #[test]
    fn run_routed_to_unreachable_host_never_succeeds() {
        let profile = MachineProfile {
            id: "m1".to_string(),
            label: "gone".to_string(),
            target: "invalid.invalid.invalid".to_string(),
            session: "default".to_string(),
            enabled: true,
        };
        // Transport failure surfaces as Err (spawn) or a non-zero remote
        // status (ssh 255); it is never Ok(0) and never touches local state.
        assert!(!matches!(
            run_routed(&profile, &["agent".to_string(), "list".to_string()]),
            Ok(0)
        ));
    }

    #[test]
    fn routed_argv_shape_is_exact() {
        let profile = test_route_profile();
        let argv = build_routed_argv(&profile, &["agent".to_string(), "list".to_string()]);
        assert_eq!(
            &argv[..7],
            [
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
                "--",
                "ssh.example.com",
            ]
        );
        assert_eq!(argv.len(), 8);
        let remote = posix_shell_split(argv.last().expect("remote command")).unwrap();
        assert_eq!(remote, ["herdr", "--session", "work", "agent", "list"]);
        assert!(ROUTABLE_SUBCOMMANDS.contains(&"agent"));
        assert!(ROUTABLE_SUBCOMMANDS.contains(&"decision"));
        assert!(ROUTABLE_SUBCOMMANDS.contains(&"pane"));
        assert!(ROUTABLE_SUBCOMMANDS.contains(&"workspace"));
        assert!(ROUTABLE_SUBCOMMANDS.contains(&"worktree"));
        assert!(!ROUTABLE_SUBCOMMANDS.contains(&"run"));
        assert!(!ROUTABLE_SUBCOMMANDS.contains(&"machine"));
    }

    #[test]
    fn route_targets_local_only_covers_decision() {
        let route = MachineRoute {
            label_or_id: "local".to_string(),
        };
        let decision_args = vec![
            "herdr".to_string(),
            "decision".to_string(),
            "answer".to_string(),
            "dec-1".to_string(),
            "yes".to_string(),
        ];
        assert!(route_targets_local(&decision_args, &route));
        let agent_args = vec!["herdr".to_string(), "agent".to_string(), "list".to_string()];
        assert!(!route_targets_local(&agent_args, &route));
        let saved_route = MachineRoute {
            label_or_id: "office".to_string(),
        };
        assert!(!route_targets_local(&decision_args, &saved_route));
    }

    fn test_route_profile() -> MachineProfile {
        MachineProfile {
            id: "m1".to_string(),
            label: "office".to_string(),
            target: "ssh.example.com".to_string(),
            session: "work".to_string(),
            enabled: true,
        }
    }

    /// Inverse of `posix_shell_join` for the always-single-quoted encoding.
    fn posix_shell_split(command: &str) -> Result<Vec<String>, String> {
        let chars: Vec<char> = command.chars().collect();
        let mut args = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            if chars[i] != '\'' {
                return Err(format!(
                    "expected POSIX single-quoted argument, got {command:?}"
                ));
            }
            i += 1;
            let mut arg = String::new();
            loop {
                if i >= chars.len() {
                    return Err("unterminated POSIX single quote".to_string());
                }
                if chars[i] == '\'' {
                    i += 1;
                    // `'\''` continues the same argument with a literal quote.
                    if i + 2 < chars.len()
                        && chars[i] == '\\'
                        && chars[i + 1] == '\''
                        && chars[i + 2] == '\''
                    {
                        arg.push('\'');
                        i += 3;
                        continue;
                    }
                    break;
                }
                arg.push(chars[i]);
                i += 1;
            }
            args.push(arg);
        }
        Ok(args)
    }

    fn reconstruct_argv_via_posix_shell(command: &str) -> Vec<String> {
        // Mimic OpenSSH: the remote command string is `$SHELL -c <command>`.
        // Reconstruct argv without executing the first word by eval'ing
        // `set --` against the already-quoted command passed as $1.
        let output = Command::new("bash")
            .arg("-c")
            .arg(r#"eval "set -- $1"; printf '%s\0' "$@""#)
            .arg("reconstruct")
            .arg(command)
            .output()
            .expect("bash reconstruct");
        assert!(
            output.status.success(),
            "posix shell reconstruct failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut got: Vec<String> = output
            .stdout
            .split(|byte| *byte == 0)
            .map(|chunk| String::from_utf8(chunk.to_vec()).expect("utf8 argv"))
            .collect();
        if got.last().is_some_and(String::is_empty) {
            got.pop();
        }
        got
    }

    #[test]
    fn routed_remote_argv_round_trips_special_characters() {
        let profile = test_route_profile();
        let bodies = [
            "hello world",
            "it's fine",
            "foo; rm -rf /",
            "$(echo INJECTED)",
            "`echo INJECTED`",
            "a'b;c $(d) `e`",
            "",
            "trailing space ",
            " say \"hi\" ",
        ];
        for body in bodies {
            let subcommand = vec![
                "agent".to_string(),
                "send".to_string(),
                "p1".to_string(),
                body.to_string(),
            ];
            let argv = build_routed_argv(&profile, &subcommand);
            assert_eq!(argv.len(), 8, "remote command must be one ssh argument");
            let remote_command = argv.last().expect("remote command");
            let expected = ["herdr", "--session", "work", "agent", "send", "p1", body];
            assert_eq!(posix_shell_split(remote_command).unwrap(), expected);
            assert_eq!(reconstruct_argv_via_posix_shell(remote_command), expected);
        }
    }

    #[test]
    fn posix_shell_join_round_trips_quotes_spaces_and_semicolons() {
        let args = [
            "herdr",
            "--session",
            "work",
            "agent",
            "send",
            "p1",
            "hello world",
            "it's fine",
            "foo; echo PWNED",
        ]
        .map(str::to_string);
        let command = posix_shell_join(&args);
        assert_eq!(posix_shell_split(&command).unwrap(), args);
        assert_eq!(reconstruct_argv_via_posix_shell(&command), args);
        assert!(command.contains("'hello world'"));
        assert!(command.contains("'\\''"));
        assert!(command.contains("'foo; echo PWNED'"));
    }
}
