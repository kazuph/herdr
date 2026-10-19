use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const TAG_PREFIX: &str = "kazuph-tb-v";
const ARCHIVE: &str = "terminal-browser-darwin-arm64.tar.gz";
const API: &str = "https://api.github.com/repos/kazuph/herdr/releases";
const PLUGIN: &str = "zenbu-labs.terminal-browser";

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
            "tb version must be MAJOR.MINOR.PATCH (for example 0.7.1)",
        ));
    }
    parts
        .iter()
        .map(|p| {
            p.parse()
                .map_err(|_| invalid("tb version component is too large"))
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
    best.ok_or_else(|| invalid("no complete stable kazuph-tb-v release found"))
}

fn download(url: &str, path: &Path) -> io::Result<()> {
    if !url.starts_with("https://") {
        return Err(invalid("tb downloads require HTTPS"));
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
        return Err(io::Error::other(format!("tb download failed: {status}")));
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
    found.ok_or_else(|| invalid("SHA256SUMS is missing the tb archive"))
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
        .map_err(|e| io::Error::other(format!("another tb install is running: {e}")))?;
    Ok(file)
}

fn validate_payload(root: &Path, version: &str) -> io::Result<()> {
    if fs::read_to_string(root.join("VERSION"))?.trim() != version
        || fs::read_to_string(root.join("PLATFORM"))?.trim() != "darwin-arm64"
    {
        return Err(invalid("tb payload version or platform mismatch"));
    }
    for file in [
        "bin/terminal-browser",
        "cli/dist/main.js",
        "electron/terminal-browser.app/Contents/MacOS/terminal-browser",
        "herdr-plugin/herdr-plugin.toml",
        "herdr-plugin/open-split.sh",
    ] {
        let path = root.join(file);
        if !path.is_file() || !path.canonicalize()?.starts_with(root.canonicalize()?) {
            return Err(invalid(format!("tb payload is missing {file}")));
        }
    }
    for executable in [
        "bin/terminal-browser",
        "electron/terminal-browser.app/Contents/MacOS/terminal-browser",
    ] {
        if !crate::platform::tb_file_executable(&fs::metadata(root.join(executable))?) {
            return Err(invalid(format!(
                "tb payload is not executable: {executable}"
            )));
        }
    }
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(
        root.join("herdr-plugin/herdr-plugin.toml"),
    )?)
    .map_err(|e| invalid(e.to_string()))?;
    if manifest.get("id").and_then(toml::Value::as_str) != Some(PLUGIN)
        || manifest.get("build").is_some()
    {
        return Err(invalid(
            "tb plugin must have the bundled id and require no build",
        ));
    }
    Ok(())
}

mod archive;
use archive::unpack;

fn shell_quote(value: &Path) -> io::Result<String> {
    let value = value
        .to_str()
        .ok_or_else(|| invalid("tb installation path must be UTF-8"))?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

fn replace_file(path: &Path, contents: &[u8], executable: bool) -> io::Result<()> {
    let temporary = path.with_extension(format!("tb-install-{}", std::process::id()));
    // If create_new fails, this temporary file is not ours to remove.
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| {
        use std::io::Write;
        file.write_all(contents)?;
        if executable {
            file.set_permissions(crate::platform::tb_archive_permissions(0o755)?)?;
        }
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

// Preserve a pre-existing regular launcher or symlink byte for byte on failure.
enum PreviousFile {
    Missing,
    File(Vec<u8>, fs::Permissions),
    Link(PathBuf),
}
impl PreviousFile {
    fn read(path: &Path) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::Missing),
            Err(e) => Err(e),
            Ok(m) if m.is_symlink() => Ok(Self::Link(fs::read_link(path)?)),
            Ok(m) if m.is_file() => Ok(Self::File(fs::read(path)?, m.permissions())),
            Ok(_) => Err(invalid(format!(
                "refusing to replace non-file {}",
                path.display()
            ))),
        }
    }
    fn restore(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::Missing => match fs::remove_file(path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::File(bytes, permissions) => {
                replace_file(path, bytes, true)?;
                fs::set_permissions(path, permissions.clone())
            }
            Self::Link(target) => {
                let temporary = path.with_extension(format!("tb-rollback-{}", std::process::id()));
                crate::platform::tb_file_symlink(target, &temporary)?;
                fs::rename(temporary, path)
            }
        }
    }
}

fn activate<F>(root: &Path, bin: &Path, version: &str, register: F) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    fs::create_dir_all(bin)?;
    let launcher = bin.join("tb");
    let current = root.join("current-version");
    let previous_launcher = PreviousFile::read(&launcher)?;
    let previous_current = PreviousFile::read(&current)?;
    let destination = root.join(version);
    let wrapper = format!(
        "#!/bin/sh\nexec {} \"$@\"\n",
        shell_quote(&destination.join("bin/terminal-browser"))?
    );
    let _interrupts = crate::platform::defer_tb_activation_interrupts()?;
    let mut current_replaced = false;
    let mut launcher_replaced = false;
    let result = (|| {
        replace_file(&current, version.as_bytes(), false)?;
        current_replaced = true;
        replace_file(&launcher, wrapper.as_bytes(), true)?;
        launcher_replaced = true;
        // Last fallible step: plugin link persists atomically before reporting
        // success. No running browser process is signalled or overwritten.
        register(&destination.join("herdr-plugin"))
    })();
    if let Err(error) = result {
        let current_result = if current_replaced {
            previous_current.restore(&current)
        } else {
            Ok(())
        };
        let launcher_result = if launcher_replaced {
            previous_launcher.restore(&launcher)
        } else {
            Ok(())
        };
        current_result.and(launcher_result).map_err(|rollback| {
            io::Error::other(format!("{error}; rollback failed: {rollback}"))
        })?;
        return Err(error);
    }
    Ok(())
}

fn install_archive<F>(
    root: &Path,
    bin: &Path,
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
    let payload = extracted.join("terminal-browser");
    validate_payload(&payload, version)?;
    let destination = root.join(version);
    match fs::symlink_metadata(&destination) {
        Ok(meta) => {
            if !meta.is_dir()
                || meta.is_symlink()
                || fs::read_to_string(destination.join(".archive-sha256"))? != digest
            {
                return Err(invalid(
                    "existing tb version differs; refusing to overwrite it",
                ));
            }
            validate_payload(&destination, version)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            use std::io::Write;
            let mut marker = File::create_new(payload.join(".archive-sha256"))?;
            marker.write_all(digest.as_bytes())?;
            fs::rename(&payload, &destination)?;
        }
        Err(e) => return Err(e),
    }
    activate(root, bin, version, register)
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

pub(super) fn run_tb_command(args: &[String]) -> io::Result<i32> {
    run_tb_with_distribution(args, &GithubDistribution)
}

fn run_tb_with_distribution(args: &[String], distribution: &impl Distribution) -> io::Result<i32> {
    let requested = match args {
        [action] if action == "install" || action == "status" => None,
        [action, option, version] if action == "install" && option == "--version" => {
            version_key(version)?;
            Some(version.as_str())
        }
        _ => {
            eprintln!("usage: herdr tb install [--version MAJOR.MINOR.PATCH] | herdr tb status");
            return Ok(2);
        }
    };
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "tb installation supports macOS Apple silicon (darwin-arm64) only",
        ));
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| invalid("HOME is not set"))?);
    let root = home.join(".local/share/herdr/terminal-browser");
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
        match distribution.release(None, &stage.0) {
            Ok(release) => println!("latest: {}", release.version().unwrap_or("unknown")),
            Err(error) => {
                eprintln!("latest: unavailable ({error})");
                return Ok(1);
            }
        }
        return Ok(0);
    }
    let release = distribution.release(requested, &stage.0)?;
    let version = release
        .version()
        .ok_or_else(|| invalid("invalid tb release version"))?;
    let archive = stage.0.join(ARCHIVE);
    let sums = stage.0.join("SHA256SUMS");
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
        &root,
        &home.join(".local/bin"),
        &stage.0,
        &archive,
        &fs::read_to_string(sums)?,
        version,
        super::plugin::link_for_tb,
    )?;
    println!(
        "tb {version} installed; add {} to PATH",
        home.join(".local/bin").display()
    );
    Ok(0)
}

#[cfg(test)]
mod tests;
