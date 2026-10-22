use super::*;
use flate2::{write::GzEncoder, Compression};
use std::sync::atomic::{AtomicUsize, Ordering};
use tar::{Builder, Header};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-mac-dialog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn versions(&self) -> PathBuf {
        self.0.join("versions")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn release(version: &str) -> Release {
    Release {
        tag_name: format!("{TAG_PREFIX}{version}"),
        draft: false,
        prerelease: false,
        assets: vec![
            Asset {
                name: ARCHIVE.into(),
                browser_download_url: "https://localhost/payload".into(),
            },
            Asset {
                name: "SHA256SUMS".into(),
                browser_download_url: "https://localhost/SHA256SUMS".into(),
            },
        ],
    }
}
#[test]
fn mac_dialog_release_selection_is_tag_scoped_paged_and_numeric() {
    let mut wrong = release("99.0.0");
    wrong.tag_name = "kazuph-tb-v99.0.0".into();
    let mut draft = release("98.0.0");
    draft.draft = true;
    let mut preview = release("97.0.0");
    preview.prerelease = true;
    let mut incomplete = release("96.0.0");
    incomplete.assets.pop();
    let mut pages = vec![
        vec![wrong, draft, preview, incomplete, release("2.0.0")],
        vec![release("11.0.0")],
        vec![],
    ]
    .into_iter();
    let mut seen = vec![];
    let chosen = select_release(None, |n| {
        seen.push(n);
        Ok(pages.next().unwrap())
    })
    .unwrap();
    assert_eq!(chosen.version(), Some("11.0.0"));
    assert_eq!(seen, vec![1, 2, 3]);
    let mut pages = vec![vec![release("0.1.0"), release("0.1.1")], vec![]].into_iter();
    assert_eq!(
        select_release(Some("0.1.0"), |_| Ok(pages.next().unwrap()))
            .unwrap()
            .version(),
        Some("0.1.0")
    );
    assert!(select_release(None, |_| Ok(vec![])).is_err());
}
#[test]
fn mac_dialog_versions_assets_and_checksums_fail_closed() {
    for value in [
        "../0.1.0",
        "0.1",
        "00.1.0",
        "0.1.0-beta",
        "1.2.18446744073709551616",
    ] {
        assert!(version_key(value).is_err());
    }
    let valid = format!("{}  {ARCHIVE}\n", "a".repeat(64));
    assert_eq!(checksum(&valid).unwrap(), "a".repeat(64));
    for text in [
        "".into(),
        format!("{valid}{valid}"),
        format!("{}  other.tgz\n", "a".repeat(64)),
        "not a checksum".into(),
    ] {
        assert!(checksum(&text).is_err());
    }
    let mut item = release("0.1.0");
    item.assets[0].browser_download_url = "http://localhost/payload".into();
    assert!(!item.eligible(None));
}
#[test]
fn mac_dialog_install_lock_excludes_concurrent_installs() {
    let s = Sandbox::new();
    let first = lock(&s.versions()).unwrap();
    assert!(lock(&s.versions()).is_err());
    drop(first);
    assert!(lock(&s.versions()).is_ok());
}
fn pack(source: &Path, target: &Path) {
    let encoder = GzEncoder::new(File::create(target).unwrap(), Compression::fast());
    let mut archive = Builder::new(encoder);
    archive.follow_symlinks(false);
    archive
        .append_dir_all("mac-dialog-decisions", source)
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap();
}
fn sums(archive: &Path) -> String {
    format!("{}  {ARCHIVE}\n", {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(fs::read(archive).unwrap()))
    })
}
#[test]
fn mac_dialog_archive_rejects_paths_links_duplicates_and_truncation_before_extraction() {
    let s = Sandbox::new();
    for (index, name, kind) in [
        (0, "../escape", tar::EntryType::Regular),
        (1, "mac-dialog-decisions/link", tar::EntryType::Symlink),
        (2, "other/file", tar::EntryType::Regular),
    ] {
        let path = s.0.join(format!("bad-{index}.tgz"));
        let encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::fast());
        let mut b = Builder::new(encoder);
        let mut h = Header::new_gnu();
        h.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(kind);
        h.set_size(0);
        h.set_mode(0o755);
        h.set_cksum();
        b.append(&h, io::empty()).unwrap();
        b.into_inner().unwrap().finish().unwrap();
        let destination = s.0.join(format!("extract-{index}"));
        assert!(unpack(&path, &destination).is_err());
        assert!(!destination.exists());
    }
    let duplicate = s.0.join("duplicate.tgz");
    let encoder = GzEncoder::new(File::create(&duplicate).unwrap(), Compression::fast());
    let mut builder = Builder::new(encoder);
    for _ in 0..2 {
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "mac-dialog-decisions/same", io::empty())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    assert!(unpack(&duplicate, &s.0.join("duplicate")).is_err());
    assert!(!s.0.join("duplicate").exists());
    let path = s.0.join("truncated.tgz");
    fs::write(&path, b"gzip truncated").unwrap();
    assert!(unpack(&path, &s.0.join("truncated")).is_err());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos {
    use super::*;
    struct Environment {
        _guard: std::sync::MutexGuard<'static, ()>,
        old: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl Environment {
        fn new(s: &Sandbox) -> Self {
            let guard = crate::config::test_config_env_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut old = vec![];
            for (key, suffix) in [
                ("HOME", "home"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_STATE_HOME", "state"),
                ("HERDR_SOCKET_PATH", "no-api.sock"),
                ("HERDR_CLIENT_SOCKET_PATH", "no-client.sock"),
            ] {
                old.push((key, std::env::var_os(key)));
                std::env::set_var(key, s.0.join(suffix));
            }
            Self { _guard: guard, old }
        }
    }
    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in &self.old {
                if let Some(v) = value {
                    std::env::set_var(key, v);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
    struct LocalFiles {
        root: PathBuf,
        version: String,
    }
    impl Distribution for LocalFiles {
        fn release(&self, requested: Option<&str>, _: &Path) -> io::Result<Release> {
            let r = release(&self.version);
            if !r.eligible(requested) {
                return Err(invalid("local version unavailable"));
            }
            Ok(r)
        }
        fn download(&self, url: &str, path: &Path) -> io::Result<()> {
            fs::copy(
                self.root.join(if url.ends_with("SHA256SUMS") {
                    "SHA256SUMS"
                } else {
                    ARCHIVE
                }),
                path,
            )?;
            Ok(())
        }
    }
    fn install(s: &Sandbox, source: &Path, version: &str) -> io::Result<String> {
        let _lock = lock(&s.versions())?;
        let stage = Stage::new(&s.versions())?;
        install_release(
            &s.versions(),
            &stage.0,
            Some(version),
            &LocalFiles {
                root: source.into(),
                version: version.into(),
            },
        )
    }
    #[test]
    fn mac_dialog_real_unsigned_app_is_rejected_and_signed_local_releases_are_transactional() {
        let source = Sandbox::new();
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("apps/mac-dialog-decisions/scripts/build-distribution.sh");
        assert!(Command::new("bash")
            .arg(script)
            .arg("0.1.0")
            .arg(&source.0)
            .status()
            .unwrap()
            .success());
        let unsigned = source.0.join("mac-dialog-decisions");
        assert!(crate::platform::verify_mac_dialog_signature(
            &unsigned.join("herdr-decision-dialog.app")
        )
        .is_err());
        pack(&unsigned, &source.0.join(ARCHIVE));
        fs::write(source.0.join("SHA256SUMS"), sums(&source.0.join(ARCHIVE))).unwrap();
        let s = Sandbox::new();
        let _environment = Environment::new(&s);
        assert!(install(&s, &source.0, "0.1.0").is_err());
        assert!(!s.versions().join("current-version").exists());
        assert!(!crate::config::config_dir().join("plugins.json").exists());
        // Real Developer ID fixtures are optional additional input. The test
        // always exercises unsigned rejection; signed local verification runs
        // when the invocation explicitly supplies actual signed release files.
        if let Some(signed) = std::env::var_os("MAC_DIALOG_TEST_DISTRIBUTIONS") {
            let signed = PathBuf::from(signed);
            install(&s, &signed.join("0.1.0"), "0.1.0").unwrap();
            let registry = crate::config::config_dir().join("plugins.json");
            install(&s, &signed.join("0.1.0"), "0.1.0").unwrap();
            let previous = fs::read(&registry).unwrap();
            let entries = crate::persist::plugin_registry::try_load().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].plugin_id, PLUGIN);
            fs::create_dir(registry.with_extension("json.tmp")).unwrap();
            assert!(install(&s, &signed.join("0.1.1"), "0.1.1").is_err());
            assert_eq!(fs::read(&registry).unwrap(), previous);
            assert_eq!(
                fs::read_to_string(s.versions().join("current-version")).unwrap(),
                "0.1.0"
            );
            fs::remove_dir(registry.with_extension("json.tmp")).unwrap();
            install(&s, &signed.join("0.1.1"), "0.1.1").unwrap();
            assert!(s
                .versions()
                .join("0.1.0/herdr-decision-dialog.app")
                .is_dir());
            assert_eq!(
                fs::read_to_string(s.versions().join("current-version")).unwrap(),
                "0.1.1"
            );
            let installed = fs::read(&registry).unwrap();
            // Recalculate the archive checksum after tampering: signature
            // verification, rather than a stale SHA, must reject the app.
            let damaged = Sandbox::new();
            let extracted = damaged.0.join("extracted");
            unpack(&signed.join("0.1.0").join(ARCHIVE), &extracted).unwrap();
            let app = extracted.join("mac-dialog-decisions/herdr-decision-dialog.app");
            use std::io::Write;
            OpenOptions::new()
                .append(true)
                .open(app.join("Contents/MacOS/herdr-decision-dialog"))
                .unwrap()
                .write_all(b"tampered")
                .unwrap();
            pack(
                &extracted.join("mac-dialog-decisions"),
                &damaged.0.join(ARCHIVE),
            );
            fs::write(damaged.0.join("SHA256SUMS"), sums(&damaged.0.join(ARCHIVE))).unwrap();
            assert!(install(&s, &damaged.0, "0.1.0").is_err());
            assert_eq!(fs::read(&registry).unwrap(), installed);
            fs::remove_dir_all(&app).unwrap();
            fs::copy(
                signed.join("0.1.0").join(ARCHIVE),
                damaged.0.join("original.tar.gz"),
            )
            .unwrap();
            let adhoc = damaged.0.join("adhoc");
            unpack(&damaged.0.join("original.tar.gz"), &adhoc).unwrap();
            let app = adhoc.join("mac-dialog-decisions/herdr-decision-dialog.app");
            assert!(Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app)
                .status()
                .unwrap()
                .success());
            assert!(crate::platform::verify_mac_dialog_signature(&app).is_err());
        }
    }
}
