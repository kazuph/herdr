use super::*;
use flate2::{write::GzEncoder, Compression};
use std::sync::atomic::{AtomicUsize, Ordering};
use tar::{Builder, Header};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "herdr-tb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn root(&self) -> PathBuf {
        self.0.join("versions")
    }
    fn bin(&self) -> PathBuf {
        self.0.join("bin")
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
                browser_download_url: "https://example.invalid/tb".into(),
            },
            Asset {
                name: "SHA256SUMS".into(),
                browser_download_url: "https://example.invalid/sums".into(),
            },
        ],
    }
}

#[test]
fn tb_release_selection_filters_and_paginates() {
    let mut pages = vec![
        vec![
            release("10.0.0"),
            release("99.0.0"),
            release("88.0.0"),
            release("77.0.0"),
        ],
        vec![release("11.0.0"), release("9.0.0")],
        vec![],
    ]
    .into_iter();
    let mut first = pages.next().unwrap();
    first[0].tag_name = "kazuph-v10.0.0".into();
    first[1].draft = true;
    first[2].prerelease = true;
    first[3].assets.pop();
    let mut pages = std::iter::once(first).chain(pages);
    let mut seen = vec![];
    let chosen = select_release(None, |page| {
        seen.push(page);
        Ok(pages.next().unwrap())
    })
    .unwrap();
    assert_eq!(chosen.version(), Some("11.0.0"));
    assert_eq!(seen, vec![1, 2, 3]);
    let mut pages = vec![vec![release("1.0.0"), release("2.0.0")], vec![]].into_iter();
    assert_eq!(
        select_release(Some("1.0.0"), |_| Ok(pages.next().unwrap()))
            .unwrap()
            .version(),
        Some("1.0.0")
    );
    assert!(select_release(None, |_| Ok(vec![])).is_err());
}

#[test]
fn tb_versions_and_checksums_fail_closed() {
    for version in [
        "../1.0.0",
        "/1.0.0",
        "1.2",
        "01.2.3",
        "1.2.3-beta",
        "1.2.3/../../foo",
        "1.2.18446744073709551616",
    ] {
        assert!(version_key(version).is_err(), "{version}");
    }
    let line = format!("{}  {ARCHIVE}\n", "a".repeat(64));
    assert_eq!(checksum(&line).unwrap(), "a".repeat(64));
    for bad in [
        String::new(),
        "bad line".into(),
        line.repeat(2),
        format!("{}  other.tar.gz\n", "a".repeat(64)),
        format!("{} *{ARCHIVE}\n", "a".repeat(64)),
        format!("{line}\n"),
        format!("{}  {ARCHIVE}\n", "g".repeat(64)),
    ] {
        assert!(checksum(&bad).is_err(), "{bad}");
    }
    let s = Sandbox::new();
    let file = s.0.join("download");
    fs::write(&file, b"truncated").unwrap();
    assert!(crate::checksum::verify_sha256(&file, &"0".repeat(64)).is_err());
    assert!(download("http://localhost/no-https", &file).is_err());
}

fn append(
    builder: &mut Builder<GzEncoder<File>>,
    name: &str,
    kind: tar::EntryType,
    link: Option<&str>,
    content: &[u8],
) {
    let mut header = Header::new_gnu();
    // Raw names deliberately allow adversarial paths that Builder::append_data
    // normally rejects before they can reach the installer under test.
    header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    header.set_mode(0o755);
    header.set_entry_type(kind);
    header.set_size(content.len() as u64);
    if let Some(link) = link {
        header.set_link_name(link).unwrap();
    }
    header.set_cksum();
    builder.append(&header, content).unwrap();
}
fn archive_at(path: &Path, extra: impl FnOnce(&mut Builder<GzEncoder<File>>)) {
    let mut builder = Builder::new(GzEncoder::new(
        File::create(path).unwrap(),
        Compression::fast(),
    ));
    extra(&mut builder);
    builder.into_inner().unwrap().finish().unwrap();
}
fn fixture(path: &Path, version: &str) {
    archive_at(path, |b| {
        for (name, data) in [
            ("VERSION", format!("{version}\n")),
            ("PLATFORM", "darwin-arm64\n".into()),
            (
                "bin/terminal-browser",
                format!("#!/bin/sh\nif [ \"${{1:-}}\" = --hold ]; then printf 'ready\\n'; read -r reply; exit 0; fi\nprintf '%s\\n' '{version}' \"$*\"\n"),
            ),
            ("cli/dist/main.js", "// local archive test payload\n".into()),
            (
                "electron/terminal-browser.app/Contents/MacOS/terminal-browser",
                "#!/bin/sh\nexit 0\n".into(),
            ),
            (
                "herdr-plugin/herdr-plugin.toml",
                include_str!(
                    "../../../apps/terminal-browser/herdr-plugin-release/herdr-plugin.toml"
                )
                .replace("version = \"0.7.1\"", &format!("version = \"{version}\"")),
            ),
            (
                "herdr-plugin/open-split.sh",
                include_str!("../../../apps/terminal-browser/herdr-plugin-release/open-split.sh")
                    .into(),
            ),
        ] {
            append(
                b,
                &format!("terminal-browser/{name}"),
                tar::EntryType::Regular,
                None,
                data.as_bytes(),
            );
        }
    });
}
fn sums(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}  {ARCHIVE}\n", Sha256::digest(fs::read(path).unwrap()))
}
fn local_install(
    s: &Sandbox,
    archive: &Path,
    version: &str,
    register: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let _lock = lock(&s.root())?;
    let stage = Stage::new(&s.root())?;
    install_archive(
        &s.root(),
        &s.bin(),
        &stage.0,
        archive,
        &sums(archive),
        version,
        register,
    )
}

#[test]
fn tb_archive_rejects_paths_devices_and_escaping_links_before_writing() {
    let s = Sandbox::new();
    for (index, (name, kind, link)) in [
        ("/tmp/outside", tar::EntryType::Regular, None),
        ("terminal-browser/../outside", tar::EntryType::Regular, None),
        ("terminal-browser/device", tar::EntryType::Char, None),
        (
            "terminal-browser/link",
            tar::EntryType::Symlink,
            Some("../../outside"),
        ),
        (
            "terminal-browser/link",
            tar::EntryType::Symlink,
            Some("/tmp/outside"),
        ),
        (
            "terminal-browser/link",
            tar::EntryType::Link,
            Some("../outside"),
        ),
        (
            "terminal-browser/link",
            tar::EntryType::Symlink,
            Some("link"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let archive = s.0.join(format!("bad-{index}.tgz"));
        archive_at(&archive, |b| append(b, name, kind, link, b""));
        let destination = s.0.join(format!("stage-{index}"));
        assert!(unpack(&archive, &destination).is_err(), "{name}");
        assert!(!destination.exists());
    }
    let archive = s.0.join("parent-link.tgz");
    archive_at(&archive, |b| {
        append(
            b,
            "terminal-browser/link",
            tar::EntryType::Symlink,
            Some("real"),
            b"",
        );
        append(
            b,
            "terminal-browser/link/escape",
            tar::EntryType::Regular,
            None,
            b"bad",
        );
        append(
            b,
            "terminal-browser/real",
            tar::EntryType::Directory,
            None,
            b"",
        );
    });
    assert!(unpack(&archive, &s.0.join("parent-link-stage")).is_err());
}

#[test]
fn tb_archive_preserves_electron_framework_links() {
    let s = Sandbox::new();
    let archive = s.0.join("framework.tgz");
    archive_at(&archive, |b| {
        for name in [
            "terminal-browser",
            "terminal-browser/F.framework",
            "terminal-browser/F.framework/Versions",
            "terminal-browser/F.framework/Versions/A",
        ] {
            append(b, name, tar::EntryType::Directory, None, b"");
        }
        append(
            b,
            "terminal-browser/F.framework/Versions/A/F",
            tar::EntryType::Regular,
            None,
            b"framework",
        );
        append(
            b,
            "terminal-browser/F.framework/Versions/Current",
            tar::EntryType::Symlink,
            Some("A"),
            b"",
        );
        append(
            b,
            "terminal-browser/F.framework/F",
            tar::EntryType::Symlink,
            Some("Versions/Current/F"),
            b"",
        );
        append(
            b,
            "terminal-browser/hard",
            tar::EntryType::Link,
            Some("terminal-browser/F.framework/Versions/A/F"),
            b"",
        );
    });
    let destination = s.0.join("extract");
    unpack(&archive, &destination).unwrap();
    assert_eq!(
        fs::read(destination.join("terminal-browser/F.framework/F")).unwrap(),
        b"framework"
    );
    assert_eq!(
        fs::read(destination.join("terminal-browser/hard")).unwrap(),
        b"framework"
    );
    assert!(fs::symlink_metadata(
        destination.join("terminal-browser/F.framework/Versions/Current")
    )
    .unwrap()
    .is_symlink());
}

struct PluginEnvironment {
    _guard: std::sync::MutexGuard<'static, ()>,
    old: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl PluginEnvironment {
    fn new(s: &Sandbox) -> Self {
        let guard = crate::config::test_config_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut old = vec![];
        for (key, suffix) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
        ] {
            old.push((key, std::env::var_os(key)));
            std::env::set_var(key, s.0.join(suffix));
        }
        Self { _guard: guard, old }
    }
}
impl Drop for PluginEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.old {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}
fn register_offline(path: &Path) -> io::Result<()> {
    let value = super::super::plugin::offline_plugin_link_response(
        &crate::api::schema::PluginLinkParams {
            path: path.to_string_lossy().into_owned(),
            enabled: true,
            source: None,
        },
    )?;
    if value.get("error").is_some() {
        return Err(io::Error::other(value.to_string()));
    }
    Ok(())
}
fn launch(s: &Sandbox) -> String {
    String::from_utf8(
        Command::new(s.bin().join("tb"))
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
}

#[test]
fn tb_local_archive_install_upgrade_reinstall_and_registration_rollback() {
    let s = Sandbox::new();
    let _environment = PluginEnvironment::new(&s);
    let first = s.0.join("one.tgz");
    fixture(&first, "0.7.1");
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    assert_eq!(launch(&s), "0.7.1\n--version\n");
    let registry = crate::config::config_dir().join("plugins.json");
    let old_registry = fs::read(&registry).unwrap();
    let old_wrapper = fs::read(s.bin().join("tb")).unwrap();
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    assert_eq!(fs::read(&registry).unwrap(), old_registry);
    // Actual registry persistence failure: the temporary-file name is a
    // directory. This does not substitute a registration implementation.
    fs::create_dir(registry.with_extension("json.tmp")).unwrap();
    let second = s.0.join("two.tgz");
    fixture(&second, "0.7.2");
    assert!(local_install(&s, &second, "0.7.2", register_offline).is_err());
    assert_eq!(fs::read(&registry).unwrap(), old_registry);
    assert_eq!(fs::read(s.bin().join("tb")).unwrap(), old_wrapper);
    assert_eq!(
        fs::read_to_string(s.root().join("current-version")).unwrap(),
        "0.7.1"
    );
    assert_eq!(launch(&s), "0.7.1\n--version\n");
    fs::remove_dir(registry.with_extension("json.tmp")).unwrap();
    let changed = s.0.join("changed.tgz");
    fs::copy(&first, &changed).unwrap();
    // Same valid tar payload with different archive bytes must never replace
    // the existing version directory, even with matching new checksums.
    use std::io::Write;
    OpenOptions::new()
        .append(true)
        .open(&changed)
        .unwrap()
        .write_all(b"different release bytes")
        .unwrap();
    assert!(local_install(&s, &changed, "0.7.1", register_offline).is_err());
    assert_eq!(launch(&s), "0.7.1\n--version\n");
    local_install(&s, &second, "0.7.2", register_offline).unwrap();
    assert_eq!(launch(&s), "0.7.2\n--version\n");
    assert!(s.root().join("0.7.1/bin/terminal-browser").is_file());
    let plugins = crate::persist::plugin_registry::try_load().unwrap();
    assert_eq!(
        plugins
            .iter()
            .find(|p| p.plugin_id == PLUGIN)
            .unwrap()
            .manifest_path,
        s.root()
            .join("0.7.2/herdr-plugin/herdr-plugin.toml")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
    );
    let output = Command::new("bash")
        .arg(s.root().join("0.7.2/herdr-plugin/open-split.sh"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "0.7.2\nopen --split right\n"
    );
}

#[test]
fn tb_extract_checksum_switch_failures_preserve_old_version() {
    let s = Sandbox::new();
    let _environment = PluginEnvironment::new(&s);
    let first = s.0.join("one.tgz");
    fixture(&first, "0.7.1");
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    let wrapper = fs::read(s.bin().join("tb")).unwrap();
    let registry = fs::read(crate::config::config_dir().join("plugins.json")).unwrap();
    let bad = s.0.join("bad.tgz");
    fs::write(&bad, b"truncated gzip").unwrap();
    assert!(local_install(&s, &bad, "0.7.2", register_offline).is_err());
    let stage = Stage::new(&s.root()).unwrap();
    assert!(install_archive(
        &s.root(),
        &s.bin(),
        &stage.0,
        &first,
        &format!("{}  {ARCHIVE}\n", "0".repeat(64)),
        "0.7.1",
        register_offline
    )
    .is_err());
    // A directory at the launcher's temporary pathname makes the atomic switch
    // fail at the filesystem, after current-version has been written.
    let blocker = s
        .bin()
        .join("tb")
        .with_extension(format!("tb-install-{}", std::process::id()));
    fs::create_dir(&blocker).unwrap();
    assert!(activate(&s.root(), &s.bin(), "0.7.2", register_offline).is_err());
    assert_eq!(fs::read(s.bin().join("tb")).unwrap(), wrapper);
    assert_eq!(
        fs::read_to_string(s.root().join("current-version")).unwrap(),
        "0.7.1"
    );
    assert_eq!(
        fs::read(crate::config::config_dir().join("plugins.json")).unwrap(),
        registry
    );
    assert_eq!(launch(&s), "0.7.1\n--version\n");
}

#[test]
fn tb_concurrent_install_is_rejected_and_lock_is_released() {
    let s = Sandbox::new();
    let first = lock(&s.root()).unwrap();
    assert!(lock(&s.root()).is_err());
    drop(first);
    assert!(lock(&s.root()).is_ok());
}

#[test]
fn tb_unsupported_platform_is_explicit() {
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        assert_eq!(
            run_tb_command(&["install".into()]).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}

#[test]
fn tb_https_download_handles_http_error_truncation_and_insecure_redirect() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let s = Sandbox::new();
    let cert = s.0.join("cert.pem");
    let key = s.0.join("key.pem");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mut child = Command::new("python3")
        .args(["-I", "-c", include_str!("test_https_server.py")])
        .arg(&cert)
        .arg(&key)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = Server(child);
    let _guard = crate::config::test_config_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    struct Ca(Option<std::ffi::OsString>);
    impl Drop for Ca {
        fn drop(&mut self) {
            if let Some(old) = &self.0 {
                std::env::set_var("CURL_CA_BUNDLE", old);
            } else {
                std::env::remove_var("CURL_CA_BUNDLE");
            }
        }
    }
    let _ca = Ca(std::env::var_os("CURL_CA_BUNDLE"));
    std::env::set_var("CURL_CA_BUNDLE", &cert);
    let output = s.0.join("download");
    let base = format!("https://localhost:{}", port.trim());
    download(&format!("{base}/ok"), &output).unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"test");
    for route in ["missing", "partial", "redirect"] {
        assert!(download(&format!("{base}/{route}"), &output).is_err());
    }
}

#[test]
fn tb_archive_rejects_link_parent_escape_after_symlink_resolution() {
    let s = Sandbox::new();
    let archive = s.0.join("pivot.tgz");
    archive_at(&archive, |b| {
        append(b, "terminal-browser", tar::EntryType::Directory, None, b"");
        append(
            b,
            "terminal-browser/deep",
            tar::EntryType::Directory,
            None,
            b"",
        );
        append(
            b,
            "terminal-browser/file",
            tar::EntryType::Regular,
            None,
            b"inside",
        );
        append(
            b,
            "terminal-browser/deep/pivot",
            tar::EntryType::Symlink,
            Some(".."),
            b"",
        );
        append(
            b,
            "terminal-browser/deep/escape",
            tar::EntryType::Symlink,
            Some("pivot/../file"),
            b"",
        );
    });
    let destination = s.0.join("extract");
    assert!(unpack(&archive, &destination).is_err());
    assert!(!destination.exists());
}

#[test]
fn tb_registration_restore_preserves_exact_entry_and_other_plugins() {
    let s = Sandbox::new();
    let _environment = PluginEnvironment::new(&s);
    let first = s.0.join("one.tgz");
    fixture(&first, "0.7.1");
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    let previous = crate::persist::plugin_registry::try_load()
        .unwrap()
        .into_iter()
        .find(|p| p.plugin_id == PLUGIN);
    let mut unrelated = previous.clone().unwrap();
    unrelated.plugin_id = "test.unrelated".into();
    crate::persist::plugin_registry::update(|entries| entries.push(unrelated.clone())).unwrap();
    let second = s.0.join("two.tgz");
    fixture(&second, "0.7.2");
    local_install(&s, &second, "0.7.2", register_offline).unwrap();
    super::super::plugin::restore_tb_registration(previous.clone()).unwrap();
    let entries = crate::persist::plugin_registry::try_load().unwrap();
    assert_eq!(
        entries.iter().find(|p| p.plugin_id == PLUGIN),
        previous.as_ref()
    );
    assert_eq!(
        entries.iter().find(|p| p.plugin_id == "test.unrelated"),
        Some(&unrelated)
    );
    super::super::plugin::restore_tb_registration(None).unwrap();
    assert_eq!(
        crate::persist::plugin_registry::try_load().unwrap(),
        vec![unrelated]
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn tb_mini_isolated_cli_installs_local_distribution_without_production_override() {
    struct LocalDistribution(PathBuf, String);
    impl Distribution for LocalDistribution {
        fn release(&self, version: Option<&str>, _: &Path) -> io::Result<Release> {
            let selected = release(&self.1);
            if !selected.eligible(version) {
                return Err(invalid("fixture version unavailable"));
            }
            Ok(selected)
        }
        fn download(&self, url: &str, path: &Path) -> io::Result<()> {
            let source = if url.ends_with("/sums") {
                self.0.join("SHA256SUMS")
            } else {
                self.0.join("payload.tgz")
            };
            fs::copy(source, path)?;
            Ok(())
        }
    }
    let s = Sandbox::new();
    let mut environment = PluginEnvironment::new(&s);
    for key in ["HERDR_SOCKET_PATH", "HERDR_CLIENT_SOCKET_PATH"] {
        environment.old.push((key, std::env::var_os(key)));
        std::env::set_var(key, s.0.join(format!("no-server-{key}.sock")));
    }
    fs::create_dir_all(s.0.join("home")).unwrap();
    let source_dir = s.0.join("source");
    fs::create_dir(&source_dir).unwrap();
    let source = source_dir.join("payload.tgz");
    fixture(&source, "0.7.1");
    fs::write(source_dir.join("SHA256SUMS"), sums(&source)).unwrap();
    let distribution = LocalDistribution(source_dir.clone(), "0.7.1".into());
    assert_eq!(
        run_tb_with_distribution(&["install".into()], &distribution).unwrap(),
        0
    );
    let home = s.0.join("home");
    let root = home.join(".local/share/herdr/terminal-browser");
    let launcher = home.join(".local/bin/tb");
    assert!(Command::new(&launcher)
        .arg("--version")
        .output()
        .unwrap()
        .stdout
        .starts_with(b"0.7.1\n"));
    assert_eq!(
        run_tb_with_distribution(&["status".into()], &distribution).unwrap(),
        0
    );
    assert_eq!(
        run_tb_with_distribution(
            &["install".into(), "--version".into(), "0.7.1".into()],
            &distribution
        )
        .unwrap(),
        0
    );
    let old_registry = fs::read(crate::config::config_dir().join("plugins.json")).unwrap();
    let old_launcher = fs::read(&launcher).unwrap();
    fixture(&source, "0.7.2");
    fs::write(source_dir.join("SHA256SUMS"), sums(&source)).unwrap();
    let distribution = LocalDistribution(source_dir.clone(), "0.7.2".into());
    let registry_tmp = crate::config::config_dir().join("plugins.json.tmp");
    fs::create_dir(&registry_tmp).unwrap();
    assert!(run_tb_with_distribution(&["install".into()], &distribution).is_err());
    assert_eq!(fs::read(&launcher).unwrap(), old_launcher);
    assert_eq!(
        fs::read(crate::config::config_dir().join("plugins.json")).unwrap(),
        old_registry
    );
    fs::remove_dir(registry_tmp).unwrap();
    assert_eq!(
        run_tb_with_distribution(&["install".into()], &distribution).unwrap(),
        0
    );
    assert_eq!(
        fs::read_to_string(root.join("current-version")).unwrap(),
        "0.7.2"
    );
    assert!(root.join("0.7.1/bin/terminal-browser").exists());
    fs::write(
        source_dir.join("SHA256SUMS"),
        format!("{}  {ARCHIVE}\n", "0".repeat(64)),
    )
    .unwrap();
    assert!(run_tb_with_distribution(&["install".into()], &distribution).is_err());
    assert_eq!(
        fs::read_to_string(root.join("current-version")).unwrap(),
        "0.7.2"
    );
}

#[test]
fn tb_reserved_checksum_marker_and_missing_execute_permission_are_rejected() {
    let s = Sandbox::new();
    let _environment = PluginEnvironment::new(&s);
    let first = s.0.join("one.tgz");
    fixture(&first, "0.7.1");
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    let stage = s.0.join("payload-stage");
    unpack(&first, &stage).unwrap();
    let payload = stage.join("terminal-browser");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            payload.join("bin/terminal-browser"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(validate_payload(&payload, "0.7.1").is_err());
        fs::set_permissions(
            payload.join("bin/terminal-browser"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("bin/terminal-browser", payload.join(".archive-sha256"))
            .unwrap();
    }
    let reserved = s.0.join("reserved.tgz");
    let encoder = GzEncoder::new(File::create(&reserved).unwrap(), Compression::fast());
    let mut builder = Builder::new(encoder);
    builder.follow_symlinks(false);
    builder
        .append_dir_all("terminal-browser", &payload)
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    // Keep a clean version directory from a different installation while
    // testing a malicious reserved marker on first installation.
    let clean = Sandbox::new();
    assert!(local_install(&clean, &reserved, "0.7.1", register_offline).is_err());
    assert!(!clean.root().join("0.7.1").exists());
    assert_eq!(launch(&s), "0.7.1\n--version\n");
}

#[test]
fn tb_running_old_version_survives_upgrade() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let s = Sandbox::new();
    let _environment = PluginEnvironment::new(&s);
    let first = s.0.join("one.tgz");
    fixture(&first, "0.7.1");
    local_install(&s, &first, "0.7.1", register_offline).unwrap();
    struct Running(std::process::Child);
    impl Drop for Running {
        fn drop(&mut self) {
            if let Some(mut stdin) = self.0.stdin.take() {
                let _ = stdin.write_all(b"finished\n");
            }
            let _ = self.0.wait();
        }
    }
    let mut running = Running(
        Command::new(s.bin().join("tb"))
            .arg("--hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(running.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "ready\n");
    let second = s.0.join("two.tgz");
    fixture(&second, "0.7.2");
    local_install(&s, &second, "0.7.2", register_offline).unwrap();
    assert!(running.0.try_wait().unwrap().is_none());
    assert_eq!(launch(&s), "0.7.2\n--version\n");
    assert!(s.root().join("0.7.1/bin/terminal-browser").exists());
}
