use super::command::*;
use super::env::*;
use super::file_ops::*;
use super::registry::*;
use super::types::*;
use super::version::*;
use super::*;

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn extract_version_triple_parses_common_outputs() {
    assert_eq!(extract_version_triple("0.14.0"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("v1.2.3"), Some((1, 2, 3)));
    assert_eq!(
        extract_version_triple("kimi-code 0.14.0 (linux/x64)"),
        Some((0, 14, 0))
    );
    assert_eq!(extract_version_triple("0.14"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("0.14.1-beta.2"), Some((0, 14, 1)));
    assert_eq!(extract_version_triple("no version here"), None);
    assert_eq!(extract_version_triple(""), None);
}

#[test]
fn extract_version_triple_orders_versions() {
    let old = extract_version_triple("0.12.1").unwrap();
    let min = extract_version_triple(KIMI_MIN_VERSION).unwrap();
    let new = extract_version_triple("0.15.0").unwrap();
    assert!(old < min);
    assert!(min <= min);
    assert!(min < new);
}

#[test]
fn agent_version_requirement_only_set_for_kimi() {
    let requirement = agent_version_requirement(crate::api::schema::IntegrationTarget::Kimi)
        .expect("kimi must have a version requirement");
    assert_eq!(requirement.binary, "kimi");
    assert_eq!(requirement.min_version, KIMI_MIN_VERSION);
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Claude).is_none());
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Codex).is_none());
}

#[test]
fn enforce_agent_version_warns_when_binary_missing() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "herdr-test-binary-that-does-not-exist",
        args: &["--version"],
        min_version: "0.14.0",
    };
    let warning = enforce_agent_version(&requirement)
        .expect("missing binary must not fail the install")
        .expect("missing binary must produce a warning");
    assert!(warning.contains("could not run"));
    assert!(warning.contains("0.14.0"));
}

#[cfg(unix)]
#[test]
fn enforce_agent_version_rejects_old_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.12.1"],
        min_version: "0.14.0",
    };
    let err = enforce_agent_version(&requirement).expect_err("old version must fail the install");
    let message = err.to_string();
    assert!(message.contains("0.12.1"));
    assert!(message.contains("0.14.0"));
    assert!(message.contains("upgrade"));
}

#[cfg(unix)]
#[test]
fn enforce_agent_version_accepts_current_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.14.0"],
        min_version: "0.14.0",
    };
    let result =
        enforce_agent_version(&requirement).expect("matching version must not fail the install");
    assert!(result.is_none(), "matching version must not warn");
}

fn clear_integration_path_env() {
    std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
    std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CODEX_HOME_ENV_VAR);
    std::env::remove_var(COPILOT_HOME_ENV_VAR);
    std::env::remove_var(KIMI_CODE_HOME_ENV_VAR);
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
}

fn kimi_hook_command(hook_path: &Path, action: &str) -> String {
    hook_command(hook_path, Some(action))
}

fn kimi_config_hooks(config: &str) -> Vec<toml::Value> {
    let parsed: toml::Value = toml::from_str(config).unwrap();
    parsed
        .get("hooks")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn assert_kimi_hook(config: &str, hook_path: &Path, event: &str, action: &str) {
    let command = kimi_hook_command(hook_path, action);
    let hooks = kimi_config_hooks(config);
    assert!(
        hooks.iter().any(|hook| {
            hook.get("event").and_then(toml::Value::as_str) == Some(event)
                && hook.get("command").and_then(toml::Value::as_str) == Some(command.as_str())
                && hook.get("timeout").and_then(toml::Value::as_integer) == Some(10)
        }),
        "missing kimi hook for {event} -> {action}"
    );
}

fn unique_base() -> PathBuf {
    clear_integration_path_env();
    std::env::temp_dir().join(format!(
        "herdr-integration-install-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn file_tree_snapshot(root: &Path) -> Vec<(String, String)> {
    fn visit(base: &Path, path: &Path, entries: &mut Vec<(String, String)>) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(base, &child, entries);
            }
        } else if metadata.is_file() {
            entries.push((
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                fs::read_to_string(path).unwrap_or_default(),
            ));
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

#[cfg(windows)]
#[test]
fn home_dir_uses_userprofile_when_home_is_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let previous_home = std::env::var_os("HOME");
    let previous_userprofile = std::env::var_os("USERPROFILE");
    std::env::remove_var("HOME");
    std::env::set_var("USERPROFILE", &base);

    assert_eq!(home_dir().unwrap(), base);

    if let Some(home) = previous_home {
        std::env::set_var("HOME", home);
    }
    if let Some(userprofile) = previous_userprofile {
        std::env::set_var("USERPROFILE", userprofile);
    } else {
        std::env::remove_var("USERPROFILE");
    }
}

#[cfg(windows)]
#[test]
fn windows_supports_only_cli_hook_integrations() {
    use crate::api::schema::IntegrationTarget;

    assert!(!integration_target_supported(IntegrationTarget::Pi));
    assert!(!integration_target_supported(IntegrationTarget::Omp));
    assert!(!integration_target_supported(IntegrationTarget::Opencode));
    assert!(!integration_target_supported(IntegrationTarget::Kilo));
    assert!(!integration_target_supported(IntegrationTarget::Hermes));
    assert!(!integration_target_supported(IntegrationTarget::Cursor));
    assert!(!integration_target_supported(IntegrationTarget::Devin));
    assert!(!integration_target_supported(IntegrationTarget::Mastracode));

    assert!(integration_target_supported(IntegrationTarget::Claude));
    assert!(integration_target_supported(IntegrationTarget::Codex));
    assert!(integration_target_supported(IntegrationTarget::Copilot));
    assert!(integration_target_supported(IntegrationTarget::Droid));
    assert!(integration_target_supported(IntegrationTarget::Kimi));
    assert!(integration_target_supported(IntegrationTarget::Qodercli));
}

#[cfg(windows)]
#[test]
fn windows_does_not_offer_unsupported_integrations_even_when_commands_exist() {
    use crate::api::schema::IntegrationTarget;

    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("pi.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("omp.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("opencode.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("kilo.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("hermes.exe"), "").unwrap();
    fs::write(bin.join("cursor-agent.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("devin.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("mastracode.cmd"), "@echo off\r\n").unwrap();

    assert!(!integration_target_available(IntegrationTarget::Pi));
    assert!(!integration_target_available(IntegrationTarget::Omp));
    assert!(!integration_target_available(IntegrationTarget::Opencode));
    assert!(!integration_target_available(IntegrationTarget::Kilo));
    assert!(!integration_target_available(IntegrationTarget::Hermes));
    assert!(!integration_target_available(IntegrationTarget::Cursor));
    assert!(!integration_target_available(IntegrationTarget::Devin));
    assert!(!integration_target_available(IntegrationTarget::Mastracode));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[cfg(windows)]
#[test]
fn windows_install_rejects_unsupported_integration_before_config_lookup() {
    use crate::api::schema::IntegrationTarget;

    let _lock = integration_env_lock();
    let original_home = std::env::var_os("HOME");
    let original_userprofile = std::env::var_os("USERPROFILE");
    let original_homedrive = std::env::var_os("HOMEDRIVE");
    let original_homepath = std::env::var_os("HOMEPATH");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    std::env::remove_var("HOMEDRIVE");
    std::env::remove_var("HOMEPATH");

    let err = install_target(IntegrationTarget::Pi).unwrap_err();
    assert_eq!(
        err.to_string(),
        "pi integration is not supported on Windows"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
    if let Some(userprofile) = original_userprofile {
        std::env::set_var("USERPROFILE", userprofile);
    }
    if let Some(homedrive) = original_homedrive {
        std::env::set_var("HOMEDRIVE", homedrive);
    }
    if let Some(homepath) = original_homepath {
        std::env::set_var("HOMEPATH", homepath);
    }
}

#[test]
#[cfg(unix)]
fn command_available_requires_executable_file_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    let command = bin.join("claude");
    fs::write(&command, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!command_available("claude"));

    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(command_available("claude"));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn command_available_finds_windows_command_shims_on_path() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("claude.cmd"), "@echo off\r\n").unwrap();
    assert!(command_available("claude"));

    fs::write(bin.join("codex.exe"), "").unwrap();
    assert!(command_available("codex"));

    assert!(!command_available("missing-agent"));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn qodercli_availability_checks_windows_aliases() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("qoder.cmd"), "@echo off\r\n").unwrap();

    assert!(integration_target_available(
        crate::api::schema::IntegrationTarget::Qodercli
    ));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn hermes_layout_can_exist_without_making_unsupported_target_available() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let local_app_data = base.join("local-app-data");
    let hermes_bin = local_app_data.join("hermes").join("bin");
    fs::create_dir_all(&hermes_bin).unwrap();
    fs::write(hermes_bin.join("hermes.exe"), "").unwrap();
    let original_local_app_data = std::env::var_os("LOCALAPPDATA");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("LOCALAPPDATA", &local_app_data);
    std::env::set_var("PATH", "");

    assert!(hermes_install_layout_available());
    assert!(!integration_target_available(
        crate::api::schema::IntegrationTarget::Hermes
    ));

    if let Some(local_app_data) = original_local_app_data {
        std::env::set_var("LOCALAPPDATA", local_app_data);
    } else {
        std::env::remove_var("LOCALAPPDATA");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn codex_availability_finds_standalone_binary_under_codex_home() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    assert!(integration_target_available(
        crate::api::schema::IntegrationTarget::Codex
    ));

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendations_mark_standalone_codex_available() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    let codex = integration_recommendations()
        .into_iter()
        .find(|recommendation| {
            recommendation.target == crate::api::schema::IntegrationTarget::Codex
        })
        .expect("codex recommendation should be present");

    assert!(codex.available);
    assert_eq!(codex.state, IntegrationStatusKind::NotInstalled);
    assert!(codex.needs_install());

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendation_installs_available_or_outdated_targets() {
    let mut recommendation = IntegrationRecommendation {
        target: crate::api::schema::IntegrationTarget::Claude,
        label: "claude",
        command: "claude",
        available: false,
        path: PathBuf::from("/tmp/herdr-agent-state.sh"),
        state: IntegrationStatusKind::NotInstalled,
    };
    assert!(!recommendation.needs_install());

    recommendation.available = true;
    assert!(recommendation.needs_install());

    recommendation.available = false;
    recommendation.state = IntegrationStatusKind::Outdated;
    assert!(recommendation.needs_install());

    recommendation.available = true;
    recommendation.state = IntegrationStatusKind::Current;
    assert!(!recommendation.needs_install());
}

#[test]
fn install_and_uninstall_targets_fail_closed_without_changing_user_dirs() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("sentinel.txt"), "keep\n").unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, home.join(".pi/agent"));
    std::env::set_var(CLAUDE_CONFIG_DIR_ENV_VAR, home.join(".claude"));
    std::env::set_var(CODEX_HOME_ENV_VAR, home.join(".codex"));
    std::env::set_var(COPILOT_HOME_ENV_VAR, home.join(".copilot"));
    std::env::set_var(KIMI_CODE_HOME_ENV_VAR, home.join(".kimi-code"));
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, home.join(".qoder"));
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, home.join(".cursor"));
    let before = file_tree_snapshot(&home);

    for target in [
        crate::api::schema::IntegrationTarget::Pi,
        crate::api::schema::IntegrationTarget::Omp,
        crate::api::schema::IntegrationTarget::Claude,
        crate::api::schema::IntegrationTarget::Codex,
        crate::api::schema::IntegrationTarget::Copilot,
        crate::api::schema::IntegrationTarget::Devin,
        crate::api::schema::IntegrationTarget::Kimi,
        crate::api::schema::IntegrationTarget::Droid,
        crate::api::schema::IntegrationTarget::Opencode,
        crate::api::schema::IntegrationTarget::Kilo,
        crate::api::schema::IntegrationTarget::Hermes,
        crate::api::schema::IntegrationTarget::Qodercli,
        crate::api::schema::IntegrationTarget::Cursor,
        crate::api::schema::IntegrationTarget::Mastracode,
    ] {
        let install_err = install_target(target).unwrap_err().to_string();
        assert!(
            install_err.contains("agent integration install is disabled in the kazuph/herdr fork"),
            "{install_err}"
        );
        assert_eq!(file_tree_snapshot(&home), before);

        let uninstall_err = uninstall_target(target).unwrap_err().to_string();
        assert!(
            uninstall_err
                .contains("agent integration uninstall is disabled in the kazuph/herdr fork"),
            "{uninstall_err}"
        );
        assert_eq!(file_tree_snapshot(&home), before);
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

fn assert_target_action_is_disabled_without_writing_user_dirs(
    target: crate::api::schema::IntegrationTarget,
    action: &str,
) {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("sentinel.txt"), "keep\n").unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, home.join(".pi/agent"));
    std::env::set_var(CLAUDE_CONFIG_DIR_ENV_VAR, home.join(".claude"));
    std::env::set_var(CODEX_HOME_ENV_VAR, home.join(".codex"));
    std::env::set_var(COPILOT_HOME_ENV_VAR, home.join(".copilot"));
    std::env::set_var(KIMI_CODE_HOME_ENV_VAR, home.join(".kimi-code"));
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, home.join(".qoder"));
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, home.join(".cursor"));
    let before = file_tree_snapshot(&home);

    let error = match action {
        "install" => install_target(target).unwrap_err().to_string(),
        "uninstall" => uninstall_target(target).unwrap_err().to_string(),
        _ => unreachable!("test helper only supports integration mutation actions"),
    };
    assert!(
        error.contains("agent integration") && error.contains("disabled in the kazuph/herdr fork"),
        "{error}"
    );
    assert_eq!(file_tree_snapshot(&home), before);

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_writes_embedded_asset_to_pi_extensions_dir_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Pi,
        "install",
    );
}

#[test]
fn install_pi_uses_pi_coding_agent_dir_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Pi,
        "install",
    );
}

#[test]
fn install_pi_expands_tilde_in_pi_coding_agent_dir_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Pi,
        "install",
    );
}

#[test]
fn install_omp_writes_embedded_asset_to_omp_extensions_dir_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn install_omp_removes_legacy_pi_integration_from_omp_extensions_dir_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn install_omp_preserves_non_herdr_file_with_pi_install_name_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn install_omp_uses_pi_coding_agent_dir_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn install_omp_creates_extensions_dir_when_agent_dir_exists_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn uninstall_omp_removes_embedded_extension_when_present_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "uninstall",
    );
}

#[test]
fn install_omp_errors_when_extension_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Omp,
        "install",
    );
}

#[test]
fn uninstall_pi_removes_embedded_extension_when_present_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Pi,
        "uninstall",
    );
}

#[test]
fn outdated_integrations_treat_missing_version_marker_as_legacy() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&extension_path, "// installed by herdr\n").unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Pi
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, None);
    assert_eq!(outdated[0].expected_version, PI_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_detect_previous_pi_version() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(
        &extension_path,
        "// HERDR_INTEGRATION_ID=pi\n// HERDR_INTEGRATION_VERSION=4\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Pi
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, Some(4));
    assert_eq!(outdated[0].expected_version, PI_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_detect_previous_omp_version() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(OMP_EXTENSION_INSTALL_NAME);
    fs::write(
        &extension_path,
        "// HERDR_INTEGRATION_ID=omp\n// HERDR_INTEGRATION_VERSION=4\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Omp
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, Some(4));
    assert_eq!(outdated[0].expected_version, OMP_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_accept_current_version_marker() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join(PI_EXTENSION_INSTALL_NAME),
        format!(
            "// HERDR_INTEGRATION_ID=pi\n// HERDR_INTEGRATION_VERSION={PI_INTEGRATION_VERSION}\n"
        ),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    assert!(outdated_installed_integrations().is_empty());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_errors_when_extension_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Pi,
        "install",
    );
}

#[test]
fn install_claude_writes_hook_and_updates_settings_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "install",
    );
}

#[test]
fn install_claude_uses_claude_config_dir_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "install",
    );
}

#[test]
fn install_claude_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "install",
    );
}

#[test]
fn install_claude_removes_deprecated_completion_hooks_and_preserves_user_hooks_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "install",
    );
}

#[test]
fn claude_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(1));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn claude_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\n# HERDR_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(2));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_claude_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "uninstall",
    );
}

#[test]
fn install_claude_errors_when_claude_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Claude,
        "install",
    );
}

#[test]
fn codex_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=codex\n# HERDR_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let codex = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Codex)
        .unwrap();

    assert_eq!(codex.path, hook_path);
    assert_eq!(codex.installed_version, Some(2));
    assert_eq!(codex.expected_version, 6);
    assert_eq!(codex.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_writes_hook_and_updates_hooks_and_config_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "install",
    );
}

#[test]
fn install_codex_uses_codex_home_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "install",
    );
}

#[test]
fn install_codex_is_idempotent_for_hook_entries_and_feature_flag_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "install",
    );
}

#[test]
fn install_codex_only_migrates_top_level_feature_flags_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "install",
    );
}

#[test]
fn uninstall_codex_removes_herdr_hooks_and_leaves_config_alone_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "uninstall",
    );
}

#[test]
fn install_codex_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Codex,
        "install",
    );
}

#[test]
fn install_kimi_writes_hook_and_updates_config_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kimi,
        "install",
    );
}

#[test]
fn install_kimi_uses_kimi_code_home_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kimi,
        "install",
    );
}

#[test]
fn install_kimi_is_idempotent_for_config_block_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kimi,
        "install",
    );
}

#[test]
fn uninstall_kimi_removes_hook_and_config_block_preserves_other_hooks_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kimi,
        "uninstall",
    );
}

#[test]
fn install_kimi_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kimi,
        "install",
    );
}

#[test]
fn install_copilot_writes_hook_and_updates_settings_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Copilot,
        "install",
    );
}

#[test]
fn copilot_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_hooks_dir = home.join(".copilot").join("hooks");
    fs::create_dir_all(&copilot_hooks_dir).unwrap();
    let hook_path = copilot_hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=copilot\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let copilot = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Copilot)
        .unwrap();

    assert_eq!(copilot.path, hook_path);
    assert_eq!(copilot.installed_version, Some(1));
    assert_eq!(copilot.expected_version, COPILOT_INTEGRATION_VERSION);
    assert_eq!(copilot.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_uses_copilot_home_env_and_is_idempotent_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Copilot,
        "install",
    );
}

#[test]
fn uninstall_copilot_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Copilot,
        "uninstall",
    );
}

#[test]
fn install_copilot_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Copilot,
        "install",
    );
}

#[test]
fn install_devin_writes_hook_and_updates_settings_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Devin,
        "install",
    );
}

#[test]
fn install_devin_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Devin,
        "install",
    );
}

#[test]
fn install_devin_removes_legacy_lifecycle_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Devin,
        "install",
    );
}

#[test]
fn uninstall_devin_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Devin,
        "uninstall",
    );
}

#[test]
fn install_devin_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Devin,
        "install",
    );
}

#[test]
fn install_droid_writes_hook_to_settings_and_cleans_legacy_hooks_json_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Droid,
        "install",
    );
}

#[test]
fn install_droid_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Droid,
        "install",
    );
}

#[test]
fn droid_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_hooks_dir = home.join(".factory").join("hooks");
    fs::create_dir_all(&droid_hooks_dir).unwrap();
    let hook_path = droid_hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=droid\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let droid = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Droid)
        .unwrap();

    assert_eq!(droid.path, hook_path);
    assert_eq!(droid.installed_version, Some(1));
    assert_eq!(droid.expected_version, DROID_INTEGRATION_VERSION);
    assert_eq!(droid.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_droid_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Droid,
        "uninstall",
    );
}

#[test]
fn install_droid_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Droid,
        "install",
    );
}

#[test]
fn install_opencode_writes_plugin_to_plugins_dir_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Opencode,
        "install",
    );
}

#[test]
fn uninstall_opencode_removes_plugin_when_present_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Opencode,
        "uninstall",
    );
}

#[test]
fn install_opencode_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Opencode,
        "install",
    );
}

#[test]
fn install_kilo_writes_plugin_to_plugin_dir_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kilo,
        "install",
    );
}

#[test]
fn uninstall_kilo_removes_plugin_when_present_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kilo,
        "uninstall",
    );
}

#[test]
fn install_kilo_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Kilo,
        "install",
    );
}

#[test]
fn install_hermes_writes_plugin_and_enables_it_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn install_hermes_is_idempotent_for_enabled_entry_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn install_hermes_preserves_flat_plugin_list_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn install_hermes_converts_flow_plugin_list_to_block_list_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn install_hermes_is_idempotent_for_quoted_flat_plugin_entry_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn uninstall_hermes_removes_plugin_and_enabled_entry_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "uninstall",
    );
}

#[test]
fn uninstall_hermes_preserves_flat_plugin_list_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "uninstall",
    );
}

#[test]
fn uninstall_hermes_removes_flow_plugin_list_entry_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "uninstall",
    );
}

#[test]
fn uninstall_hermes_removes_commented_flat_plugin_entry_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "uninstall",
    );
}

#[test]
fn install_hermes_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Hermes,
        "install",
    );
}

#[test]
fn bundled_integration_asset_versions_match_expected_versions() {
    for (name, asset, expected_version) in [
        ("omp", OMP_EXTENSION_ASSET, OMP_INTEGRATION_VERSION),
        ("claude", CLAUDE_HOOK_ASSET, CLAUDE_INTEGRATION_VERSION),
        ("codex", CODEX_HOOK_ASSET, CODEX_INTEGRATION_VERSION),
        ("kimi", KIMI_HOOK_ASSET, KIMI_INTEGRATION_VERSION),
        ("copilot", COPILOT_HOOK_ASSET, COPILOT_INTEGRATION_VERSION),
        ("devin", DEVIN_HOOK_ASSET, DEVIN_INTEGRATION_VERSION),
        ("droid", DROID_HOOK_ASSET, DROID_INTEGRATION_VERSION),
        (
            "opencode",
            OPENCODE_PLUGIN_ASSET,
            OPENCODE_INTEGRATION_VERSION,
        ),
        ("kilo", KILO_PLUGIN_ASSET, KILO_INTEGRATION_VERSION),
        (
            "hermes",
            HERMES_PLUGIN_INIT_ASSET,
            HERMES_INTEGRATION_VERSION,
        ),
        (
            "qodercli",
            QODERCLI_HOOK_ASSET,
            QODERCLI_INTEGRATION_VERSION,
        ),
        ("cursor", CURSOR_HOOK_ASSET, CURSOR_INTEGRATION_VERSION),
        (
            "mastracode",
            MASTRACODE_HOOK_ASSET,
            MASTRACODE_INTEGRATION_VERSION,
        ),
    ] {
        if asset.is_empty() {
            continue;
        }
        assert_eq!(
            parse_integration_version(asset),
            Some(expected_version),
            "{name} asset version must match its integration version constant"
        );
    }
    assert!(
        PI_EXTENSION_ASSET.is_empty(),
        "kazuph/herdr fork does not bundle a pi hook integration asset"
    );
}

#[test]
fn bundled_integration_assets_report_session_refs() {
    assert!(
        PI_EXTENSION_ASSET.is_empty(),
        "kazuph/herdr fork keeps pi hook integration unbundled"
    );
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_path"));
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_id"));
    assert!(OMP_EXTENSION_ASSET.contains("ctx?.hasUI !== true"));
    assert!(OMP_EXTENSION_ASSET.contains("pane.report_agent_session"));
    assert!(OMP_EXTENSION_ASSET.contains("pane.report_agent\""));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"agent_start\""));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"agent_end\""));
    assert!(OMP_EXTENSION_ASSET.contains("pane.release_agent"));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"session_shutdown\""));
    if !CLAUDE_HOOK_ASSET.is_empty() {
        assert!(
            CLAUDE_HOOK_ASSET.contains("agent_session_id")
                || CLAUDE_HOOK_ASSET.contains("--agent-session-id")
        );
        assert!(
            CLAUDE_HOOK_ASSET.contains("agent_session_path")
                || CLAUDE_HOOK_ASSET.contains("--agent-session-path")
        );
        assert!(CLAUDE_HOOK_ASSET.contains("agent_id"));
        assert!(
            CLAUDE_HOOK_ASSET.contains("session_start_source")
                || CLAUDE_HOOK_ASSET.contains("--session-start-source")
        );
        assert!(
            CLAUDE_HOOK_ASSET.contains("pane.report_agent_session")
                || CLAUDE_HOOK_ASSET.contains("report-agent-session")
        );
        assert!(!CLAUDE_HOOK_ASSET.contains("\"state\": action"));
        assert!(!CLAUDE_HOOK_ASSET.contains("pane.release_agent"));
    }
    if !CODEX_HOOK_ASSET.is_empty() {
        assert!(
            CODEX_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE")
                || CODEX_HOOK_ASSET.contains("In.ReadToEnd")
        );
        assert!(
            CODEX_HOOK_ASSET.contains("agent_session_id")
                || CODEX_HOOK_ASSET.contains("--agent-session-id")
        );
        assert!(
            CODEX_HOOK_ASSET.contains("session_start_source")
                || CODEX_HOOK_ASSET.contains("--session-start-source")
        );
        assert!(
            CODEX_HOOK_ASSET.contains("pane.report_agent_session")
                || CODEX_HOOK_ASSET.contains("report-agent-session")
        );
        assert!(!CODEX_HOOK_ASSET.contains("\"state\": action"));
        assert!(!CODEX_HOOK_ASSET.contains("pane.release_agent"));
    }
    assert!(KIMI_HOOK_ASSET.contains("source = \"herdr:kimi\""));
    assert!(KIMI_HOOK_ASSET.contains("agent_session_id"));
    assert!(KIMI_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(KIMI_HOOK_ASSET.contains("\"state\": action"));
    assert!(!KIMI_HOOK_ASSET.contains("pane.release_agent"));
    assert!(COPILOT_HOOK_ASSET.contains("agent_session_id"));
    assert!(COPILOT_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!COPILOT_HOOK_ASSET.contains("\"state\":"));
    assert!(!COPILOT_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("HERDR_DEVIN_LIST_JSON"));
    assert!(DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent_session\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"state\":"));
    assert!(!DEVIN_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!DROID_HOOK_ASSET.contains("\"state\": action"));
    assert!(!DROID_HOOK_ASSET.contains("pane.release_agent"));
    if !OPENCODE_PLUGIN_ASSET.is_empty() {
        assert!(OPENCODE_PLUGIN_ASSET.contains("properties?.sessionID"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("params.agent_session_id = sessionID"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("pane.report_agent_session"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("reportState"));
        assert!(!OPENCODE_PLUGIN_ASSET.contains("pane.release_agent"));
    }
    assert!(KILO_PLUGIN_ASSET.contains("SOURCE = \"herdr:kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("AGENT = \"kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("pane.report_agent_session"));
    assert!(KILO_PLUGIN_ASSET.contains("reportState"));
    assert!(!KILO_PLUGIN_ASSET.contains("pane.release_agent"));
    if !HERMES_PLUGIN_INIT_ASSET.is_empty() {
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("session_id = _session_id(kwargs)"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("agent_session_id"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("pane.report_agent\","));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("on_session_end"));
        assert!(!HERMES_PLUGIN_INIT_ASSET.contains("on_session_finalize"));
        assert!(!HERMES_PLUGIN_INIT_ASSET.contains("pane.release_agent"));
    }
    assert!(QODERCLI_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE"));
    assert!(QODERCLI_HOOK_ASSET.contains("agent_session_id"));
    assert!(QODERCLI_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!QODERCLI_HOOK_ASSET.contains("\"state\": action"));
    assert!(!QODERCLI_HOOK_ASSET.contains("pane.release_agent"));
    assert!(!QODERCLI_HOOK_ASSET.contains("QODER_HOOK_EVENT"));
    assert!(CURSOR_HOOK_ASSET.contains("HERDR_INTEGRATION_ID=cursor"));
    assert!(CURSOR_HOOK_ASSET.contains("conversation_id"));
    assert!(CURSOR_HOOK_ASSET.contains("conversationId"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionId"));
    assert!(CURSOR_HOOK_ASSET.contains("agent_session_id"));
    assert!(CURSOR_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(CURSOR_HOOK_ASSET.contains("hook_event_name"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionStart"));
    assert!(!CURSOR_HOOK_ASSET.contains("\"state\":"));
    assert!(!CURSOR_HOOK_ASSET.contains("pane.release_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("HERDR_INTEGRATION_ID=mastracode"));
    assert!(MASTRACODE_HOOK_ASSET.contains("HERDR_INTEGRATION_VERSION=1"));
    assert!(MASTRACODE_HOOK_ASSET.contains("session_id"));
    assert!(!MASTRACODE_HOOK_ASSET.contains("run_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("agent_session_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.report_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.release_agent"));
}

#[test]
fn pi_extension_releases_only_for_quit_session_shutdown() {
    assert!(
        PI_EXTENSION_ASSET.is_empty(),
        "kazuph/herdr fork does not ship pi lifecycle hook release code"
    );
}

#[test]
fn pi_extension_refreshes_session_ref_before_agent_start_state() {
    assert!(
        PI_EXTENSION_ASSET.is_empty(),
        "kazuph/herdr fork does not ship pi agent_start hook code"
    );
}

#[test]
fn omp_extension_releases_only_for_quit_session_shutdown() {
    let release_policy = OMP_EXTENSION_ASSET
        .find("function shouldReleaseOnSessionShutdown")
        .expect("omp extension should centralize session shutdown release policy");
    let quit_check = OMP_EXTENSION_ASSET
        .find("reason === \"quit\"")
        .expect("omp extension should release only for true quit shutdowns");
    let shutdown_handler = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_shutdown\", async (event)")
        .expect("omp extension should inspect the session_shutdown event");
    let guarded_release = OMP_EXTENSION_ASSET[shutdown_handler..]
        .find("if (shouldReleaseOnSessionShutdown(event))")
        .expect("omp extension should guard releaseAgent by shutdown reason");

    assert!(release_policy < shutdown_handler);
    assert!(release_policy < quit_check);
    assert!(quit_check < shutdown_handler);
    assert!(guarded_release > 0);
}

#[test]
fn omp_extension_refreshes_session_ref_before_agent_start_state() {
    let agent_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"agent_start\", (_event, ctx)")
        .expect("omp extension should receive agent_start context");
    let handler = &OMP_EXTENSION_ASSET[agent_start..];
    let update_session = handler
        .find("updateSessionRef(ctx);")
        .expect("omp extension should refresh the active session on agent_start");
    let report_session = handler
        .find("void reportSession();")
        .expect("omp extension should report the refreshed session before state");
    let publish_state = handler
        .find("publishState();")
        .expect("omp extension should publish working state after refreshing session");

    assert!(update_session < report_session);
    assert!(report_session < publish_state);
}

fn omp_handler(event: &str) -> &'static str {
    let start = OMP_EXTENSION_ASSET
        .find(&format!("pi.on(\"{event}\""))
        .unwrap_or_else(|| panic!("omp extension registers {event} handler"));
    let rest = &OMP_EXTENSION_ASSET[start..];
    let end = rest[1..]
        .find("\n\n  pi.")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn omp_root_activation_requires_ui_context() {
    let activator = OMP_EXTENSION_ASSET
        .find("function activateRootSession(ctx: any, sessionStartSource = \"startup\"): boolean")
        .expect("omp extension should centralize root session activation");
    let helper = &OMP_EXTENSION_ASSET[activator..];
    let non_ui_guard = helper
        .find("ctx?.hasUI !== true")
        .expect("omp extension checks UI context before activating");
    let root_session = helper
        .find("rootSession = true;")
        .expect("omp extension activates root session after UI guard");
    let session_report = helper
        .find("void reportSession(sessionStartSource);")
        .expect("omp extension reports root session");

    assert!(non_ui_guard < root_session);
    assert!(root_session < session_report);
}

#[test]
fn omp_session_start_and_switch_use_root_activation() {
    let session_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_start\", (_event, ctx)")
        .expect("omp extension registers session_start handler");
    let session_start_handler = &OMP_EXTENSION_ASSET[session_start..];
    session_start_handler
        .find("if (!activateRootSession(ctx))")
        .expect("omp session_start handler should activate root session");

    let session_switch = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_switch\", (event, ctx)")
        .expect("omp extension registers session_switch handler");
    let session_switch_handler = &OMP_EXTENSION_ASSET[session_switch..];
    session_switch_handler
        .find("if (!activateRootSession(ctx, event?.reason || \"resume\"))")
        .expect("omp session_switch handler should activate root session with switch reason");
}

#[test]
fn omp_session_reports_include_start_source() {
    let report_session = OMP_EXTENSION_ASSET
        .find("function reportSession(sessionStartSource = \"startup\"): Promise<void>")
        .expect("omp extension should label session reports with a lifecycle source");
    let helper = &OMP_EXTENSION_ASSET[report_session..];
    let session_source = helper
        .find("session_start_source: sessionStartSource")
        .expect("omp session reports should include the lifecycle source");
    let session_ref = helper
        .find("...sessionRef")
        .expect("omp session reports should include the native session ref");

    assert!(session_source < session_ref);
}

#[test]
fn omp_socket_requests_are_serialized() {
    let queue = OMP_EXTENSION_ASSET
        .find("let requestQueue = Promise.resolve();")
        .expect("omp extension should keep socket reports ordered");
    let send_request = OMP_EXTENSION_ASSET[queue..]
        .find("function sendRequest(request: unknown): Promise<void>")
        .expect("omp extension should wrap socket sends in an ordered queue");
    let queued_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("requestQueue = requestQueue.then(")
        .expect("omp extension should serialize socket requests through the queue");
    let raw_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("sendRequestNow(request)")
        .expect("omp extension should enqueue the raw socket send");

    assert!(queued_send < raw_send);
}

#[test]
fn omp_runtime_events_can_activate_root_session_after_resume() {
    for event in [
        "agent_start",
        "tool_approval_requested",
        "tool_approval_resolved",
        "tool_execution_start",
        "tool_execution_end",
    ] {
        let handler = omp_handler(event);
        handler
            .find("!rootSession && !activateRootSession(ctx)")
            .unwrap_or_else(|| panic!("omp {event} handler should recover missing root session"));
    }
}

#[test]
fn omp_ask_and_approval_events_report_blocked_state() {
    let approval_handler = omp_handler("tool_approval_requested");
    approval_handler
        .find("activateBlocked(label);")
        .expect("approval requests should block the pane");

    let approval_resolved = omp_handler("tool_approval_resolved");
    approval_resolved
        .find("deactivateBlocked();")
        .expect("approval resolution should unblock the pane");

    let ask_handler = omp_handler("tool_execution_start");
    ask_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution handler should only treat Ask as blocked");
    ask_handler
        .find("activateBlocked(askBlockedMessage(event.args));")
        .expect("Ask start should block the pane");

    let ask_end_handler = omp_handler("tool_execution_end");
    ask_end_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution end should only treat Ask as blocked");
    ask_end_handler
        .find("deactivateBlocked();")
        .expect("Ask end should unblock the pane");
}

#[test]
fn install_qodercli_writes_hook_and_updates_settings_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Qodercli,
        "install",
    );
}

#[test]
fn install_qodercli_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Qodercli,
        "install",
    );
}

#[test]
fn uninstall_qodercli_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Qodercli,
        "uninstall",
    );
}

#[test]
fn install_qodercli_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Qodercli,
        "install",
    );
}

#[test]
fn install_cursor_writes_hook_and_updates_hooks_json_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Cursor,
        "install",
    );
}

#[test]
fn install_cursor_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Cursor,
        "install",
    );
}

#[test]
fn uninstall_cursor_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs()
{
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Cursor,
        "uninstall",
    );
}

#[test]
fn install_cursor_uses_cursor_config_dir_env_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Cursor,
        "install",
    );
}

#[test]
fn cursor_v1_integration_status_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    let hook_path = cursor_dir.join(CURSOR_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=cursor\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let statuses = installed_integration_statuses();
    let cursor = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Cursor)
        .expect("cursor integration status");
    assert_eq!(cursor.state, IntegrationStatusKind::Current);
    assert_eq!(cursor.installed_version, Some(CURSOR_INTEGRATION_VERSION));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_errors_when_config_dir_missing_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Cursor,
        "install",
    );
}

#[test]
fn install_mastracode_writes_hook_and_updates_hooks_json_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Mastracode,
        "install",
    );
}

#[test]
fn install_mastracode_is_idempotent_for_hook_entries_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Mastracode,
        "install",
    );
}

#[test]
fn uninstall_mastracode_removes_herdr_hooks_and_preserves_others_is_disabled_without_writing_user_dirs(
) {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Mastracode,
        "uninstall",
    );
}

#[test]
fn install_mastracode_errors_when_event_value_not_array_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Mastracode,
        "install",
    );
}

#[test]
fn uninstall_mastracode_errors_when_event_value_not_array_is_disabled_without_writing_user_dirs() {
    assert_target_action_is_disabled_without_writing_user_dirs(
        crate::api::schema::IntegrationTarget::Mastracode,
        "uninstall",
    );
}
