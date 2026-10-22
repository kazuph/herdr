use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const TAG_PREFIX: &str = "kazuph-mac-dialog-v";
const ARCHIVE: &str = "mac-dialog-decisions-darwin-arm64.tar.gz";
const API: &str = "https://api.github.com/repos/kazuph/herdr/releases";
const PLUGIN: &str = "kazuph.mac-dialog-decisions";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

// Stable numeric releases only; never interpret a version as a path or option.
fn version_key(value: &str) -> io::Result<Vec<u64>> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
        })
    {
        return Err(invalid(
            "mac-dialog version must be MAJOR.MINOR.PATCH (for example 0.7.1)",
        ));
    }
    parts
        .iter()
        .map(|p| {
            p.parse()
                .map_err(|_| invalid("mac-dialog version component is too large"))
        })
        .collect()
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

impl Release {
    fn version(&self) -> Option<&str> {
        self.tag_name
            .strip_prefix(TAG_PREFIX)
            .filter(|v| version_key(v).is_ok())
    }

    fn asset(&self, name: &str) -> Option<&str> {
        let mut matches = self.assets.iter().filter(|a| a.name == name);
        let first = matches.next()?;
        if matches.next().is_some() || !first.browser_download_url.starts_with("https://") {
            return None;
        }
        Some(&first.browser_download_url)
    }

    fn eligible(&self, requested: Option<&str>) -> bool {
        !self.draft
            && !self.prerelease
            && self
                .version()
                .is_some_and(|v| requested.is_none_or(|wanted| wanted == v))
            && self.asset(ARCHIVE).is_some()
            && self.asset("SHA256SUMS").is_some()
    }
}

fn select_release<F>(requested: Option<&str>, mut page: F) -> io::Result<Release>
where
    F: FnMut(usize) -> io::Result<Vec<Release>>,
{
    if let Some(version) = requested {
        version_key(version)?;
    }
    let mut best: Option<Release> = None;
    for index in 1.. {
        let releases = page(index)?;
        if releases.is_empty() {
            break;
        }
        for release in releases {
            if release.eligible(requested)
                && best.as_ref().is_none_or(|old| {
                    release.version().and_then(|v| version_key(v).ok())
                        > old.version().and_then(|v| version_key(v).ok())
                })
            {
                best = Some(release);
            }
        }
    }
    best.ok_or_else(|| invalid("no complete stable kazuph-mac-dialog-v release found"))
}

fn download(url: &str, path: &Path) -> io::Result<()> {
    if !url.starts_with("https://") {
        return Err(invalid("mac-dialog downloads require HTTPS"));
    }
    let status = Command::new("curl")
        .args([
            "--disable",
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--output",
        ])
        .arg(path)
        .arg(url)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "mac-dialog download failed: {status}"
        )));
    }
    Ok(())
}

fn latest(requested: Option<&str>, staging: &Path) -> io::Result<Release> {
    select_release(requested, |page| {
        let path = staging.join("releases.json");
        // GitHub's documented maximum page size; keep paging until empty, since
        // Herdr and tb releases are interleaved and API ordering is not semver.
        download(&format!("{API}?per_page=100&page={page}"), &path)?;
        serde_json::from_reader(File::open(path)?).map_err(invalid_json)
    })
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(error.to_string())
}

fn checksum(text: &str) -> io::Result<String> {
    let mut found = None;
    let mut names = std::collections::HashSet::new();
    for line in text.lines() {
        let bytes = line.as_bytes();
        if bytes.len() < 67
            || !bytes[..64].iter().all(u8::is_ascii_hexdigit)
            || &bytes[64..66] != b"  "
        {
            return Err(invalid("invalid SHA256SUMS line"));
        }
        let name = &line[66..];
        if name.is_empty() || name.chars().any(char::is_whitespace) || !names.insert(name) {
            return Err(invalid("invalid or duplicate SHA256SUMS filename"));
        }
        if name == ARCHIVE {
            found = Some(line[..64].to_ascii_lowercase());
        }
    }
    found.ok_or_else(|| invalid("SHA256SUMS is missing the mac-dialog archive"))
}

struct Stage(PathBuf);
impl Stage {
    fn new(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        // The install lock serializes ownership; create_dir refuses stale paths.
        let path = root.join(format!(".staging-{}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn lock(root: &Path) -> io::Result<File> {
    fs::create_dir_all(root)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".install.lock"))?;
    file.try_lock()
        .map_err(|e| io::Error::other(format!("another mac-dialog install is running: {e}")))?;
    Ok(file)
}

fn validate_payload(root: &Path, version: &str) -> io::Result<()> {
    if fs::read_to_string(root.join("VERSION"))?.trim() != version
        || fs::read_to_string(root.join("PLATFORM"))?.trim() != "darwin-arm64"
    {
        return Err(invalid("mac-dialog payload version or platform mismatch"));
    }
    for name in [
        "herdr-plugin.toml",
        "mac-dialog-decisions.mjs",
        "ensure-daemon",
        "herdr-decision-dialog.app/Contents/Info.plist",
        "herdr-decision-dialog.app/Contents/MacOS/herdr-decision-dialog",
    ] {
        if !root.join(name).is_file()
            || !root
                .join(name)
                .canonicalize()?
                .starts_with(root.canonicalize()?)
        {
            return Err(invalid(format!("mac-dialog payload is missing {name}")));
        }
    }
    for name in [
        "ensure-daemon",
        "herdr-decision-dialog.app/Contents/MacOS/herdr-decision-dialog",
    ] {
        if !crate::platform::tb_file_executable(&fs::metadata(root.join(name))?) {
            return Err(invalid(format!(
                "mac-dialog payload is not executable: {name}"
            )));
        }
    }
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("herdr-plugin.toml"))?)
            .map_err(|e| invalid(e.to_string()))?;
    if manifest.get("id").and_then(toml::Value::as_str) != Some(PLUGIN)
        || manifest.get("build").is_some()
    {
        return Err(invalid(
            "mac-dialog plugin must have the bundled id and require no build",
        ));
    }
    Ok(())
}

mod archive;
use archive::unpack;

fn replace_current(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("mac-dialog-install-{}", std::process::id()));
    let mut file = File::create_new(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn activate<F>(root: &Path, version: &str, register: F) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let current = root.join("current-version");
    let previous = match fs::symlink_metadata(&current) {
        Ok(meta) if meta.is_file() && !meta.is_symlink() => Some(fs::read(&current)?),
        Ok(_) => {
            return Err(invalid(
                "refusing to replace non-file mac-dialog current-version",
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    // Reuse the existing critical-section signal guard without changing tb.
    let _interrupts = crate::platform::defer_tb_activation_interrupts()?;
    replace_current(&current, version.as_bytes())?;
    if let Err(error) = register(&root.join(version)) {
        let rollback = match previous {
            Some(bytes) => replace_current(&current, &bytes),
            None => fs::remove_file(&current),
        };
        rollback.map_err(|rollback| {
            io::Error::other(format!("{error}; rollback failed: {rollback}"))
        })?;
        return Err(error);
    }
    Ok(())
}

fn install_archive<F>(
    root: &Path,
    staging: &Path,
    archive: &Path,
    sums: &str,
    version: &str,
    register: F,
) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    version_key(version)?;
    let digest = checksum(sums)?;
    crate::checksum::verify_sha256(archive, &digest)?;
    let extracted = staging.join("extracted");
    unpack(archive, &extracted)?;
    let payload = extracted.join("mac-dialog-decisions");
    validate_payload(&payload, version)?;
    crate::platform::verify_mac_dialog_signature(&payload.join("herdr-decision-dialog.app"))?;
    let destination = root.join(version);
    match fs::symlink_metadata(&destination) {
        Ok(meta) => {
            if !meta.is_dir()
                || meta.is_symlink()
                || fs::read_to_string(destination.join(".archive-sha256"))? != digest
            {
                return Err(invalid(
                    "existing mac-dialog version differs; refusing to overwrite it",
                ));
            }
            validate_payload(&destination, version)?;
            crate::platform::verify_mac_dialog_signature(
                &destination.join("herdr-decision-dialog.app"),
            )?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            use std::io::Write;
            let mut marker = File::create_new(payload.join(".archive-sha256"))?;
            marker.write_all(digest.as_bytes())?;
            fs::rename(&payload, &destination)?;
        }
        Err(e) => return Err(e),
    }
    activate(root, version, register)
}

trait Distribution {
    fn release(&self, version: Option<&str>, stage: &Path) -> io::Result<Release>;
    fn download(&self, url: &str, path: &Path) -> io::Result<()>;
}
struct GithubDistribution;
impl Distribution for GithubDistribution {
    fn release(&self, version: Option<&str>, stage: &Path) -> io::Result<Release> {
        latest(version, stage)
    }
    fn download(&self, url: &str, path: &Path) -> io::Result<()> {
        download(url, path)
    }
}

pub(super) fn run_mac_dialog_command(args: &[String]) -> io::Result<i32> {
    run_with_distribution(args, &GithubDistribution)
}
fn run_with_distribution(args: &[String], distribution: &impl Distribution) -> io::Result<i32> {
    let requested = match args {
        [action] if action == "install" || action == "status" => None,
        [action, option, version] if action == "install" && option == "--version" => {
            version_key(version)?;
            Some(version.as_str())
        }
        _ => {
            eprintln!("usage: herdr mac-dialog install [--version MAJOR.MINOR.PATCH] | herdr mac-dialog status");
            return Ok(2);
        }
    };
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "判断待ちダイアログの導入は macOS Apple silicon (arm64) のみに対応しています",
        ));
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| invalid("HOME is not set"))?);
    let root = home.join(".local/share/herdr/mac-dialog-decisions");
    let _lock = lock(&root)?;
    let stage = Stage::new(&root)?;
    if args[0] == "status" {
        let current = fs::read_to_string(root.join("current-version"))
            .unwrap_or_else(|_| "not installed".into());
        println!(
            "installed: {}\nlocation: {}",
            current.trim(),
            root.display()
        );
        print_runtime_status()?;
        match distribution.release(None, &stage.0) {
            Ok(release) => println!("latest: {}", release.version().unwrap_or("unknown")),
            Err(error) => {
                eprintln!("latest: unavailable ({error})");
                return Ok(1);
            }
        }
        return Ok(0);
    }
    let node = Command::new("node")
        .arg("--version")
        .output()
        .map_err(|e| {
            io::Error::other(format!(
                "Node.js が必要です。先に Node を導入してください: {e}"
            ))
        })?;
    if !node.status.success() {
        return Err(invalid("Node.js を起動できません"));
    }
    let version = install_release(&root, &stage.0, requested, distribution)?;
    println!(
        "mac-dialog {version} installed: {}",
        root.join(&version).display()
    );
    let result = plugin_node(&root.join(&version), "ensure")?;
    if !result.status.success() {
        eprintln!(
            "登録は完了しましたが常駐役の起動は未完了です: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
        return Ok(1);
    }
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    Ok(0)
}

fn install_release(
    root: &Path,
    stage: &Path,
    requested: Option<&str>,
    distribution: &impl Distribution,
) -> io::Result<String> {
    let release = distribution.release(requested, stage)?;
    let version = release
        .version()
        .ok_or_else(|| invalid("invalid mac-dialog release version"))?;
    let archive = stage.join(ARCHIVE);
    let sums = stage.join("SHA256SUMS");
    distribution.download(
        release
            .asset(ARCHIVE)
            .ok_or_else(|| invalid("missing archive"))?,
        &archive,
    )?;
    distribution.download(
        release
            .asset("SHA256SUMS")
            .ok_or_else(|| invalid("missing checksums"))?,
        &sums,
    )?;
    install_archive(
        root,
        stage,
        &archive,
        &fs::read_to_string(sums)?,
        version,
        super::plugin::link_for_mac_dialog,
    )?;
    Ok(version.to_string())
}

fn plugin_node(root: &Path, action: &str) -> io::Result<std::process::Output> {
    Command::new("node")
        .arg(root.join("mac-dialog-decisions.mjs"))
        .arg(action)
        .env("HERDR_BIN_PATH", std::env::current_exe()?)
        .env(
            "HERDR_PLUGIN_STATE_DIR",
            crate::plugin_paths::plugin_state_dir(PLUGIN),
        )
        .env(
            "HERDR_PLUGIN_CONFIG_DIR",
            crate::plugin_paths::plugin_config_dir(PLUGIN),
        )
        .output()
}

fn print_runtime_status() -> io::Result<()> {
    let registered = crate::persist::plugin_registry::try_load()?
        .into_iter()
        .find(|p| p.plugin_id == PLUGIN);
    let Some(registered) = registered else {
        println!("registered: not registered\nrunning: unknown");
        return Ok(());
    };
    let root = Path::new(&registered.manifest_path)
        .parent()
        .ok_or_else(|| invalid("invalid registered mac-dialog path"))?;
    let version = fs::read_to_string(root.join("VERSION")).unwrap_or_else(|_| "development".into());
    println!(
        "registered: {}\nregistered location: {}",
        version.trim(),
        root.display()
    );
    // Read private state directly: legacy scripts do not implement `status`.
    // Executing one with an unknown action could start a daemon as a side effect.
    let state_path = crate::plugin_paths::plugin_state_dir(PLUGIN).join("daemon.json");
    let state: serde_json::Value = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid_json)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error),
    };
    let running = state
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(crate::platform::mac_dialog_daemon_running);
    if running {
        let running = state
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("不明（旧常駐役）");
        println!("running: {running}");
        let same = state.get("plugin_root").and_then(serde_json::Value::as_str) == root.to_str();
        if !same {
            println!("切替待ち: 登録版／稼働版が異なります。旧版は強制停止しません");
        }
        if let Some(windows) = state
            .get("open_windows")
            .and_then(serde_json::Value::as_u64)
        {
            println!("open windows: {windows}");
        }
    } else {
        println!("running: not running");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
