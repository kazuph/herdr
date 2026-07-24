use std::fs;
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn root_help_is_skill_style_and_matches_help_command() {
    let dash_help = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    let help_command = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("help")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();

    assert!(dash_help.status.success());
    assert!(help_command.status.success());
    assert_eq!(dash_help.stdout, help_command.stdout);

    let stdout = String::from_utf8_lossy(&dash_help.stdout);
    for expected in [
        "---\nname: herdr",
        "# herdr",
        "## When To Use",
        "## Agent Rules",
        "herdr help",
        "herdr --help",
        "herdr pane current",
        "HERDR_PANE_ID",
        "calling process session",
        "parent process tree",
        "Do not infer the requester pane from the focused pane",
        "send=talk, run=execute, log=inspect, inbox=pull fallback",
        "herdr send <agent_target> <message>",
        "herdr run --label tests -- cargo test",
        "herdr run list",
        "herdr log --db",
        "sqlite3 \"$(herdr log --db)\"",
    ] {
        assert!(
            stdout.contains(expected),
            "root help should contain {expected:?}: {stdout}"
        );
    }
}

#[test]
fn root_help_hides_explicit_client_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("herdr client"),
        "root help should not advertise the internal client command: {stdout}"
    );
}

#[test]
fn pane_help_distinguishes_literal_text_from_submitted_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["pane", "help"])
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(
        "pane send-text writes literal text without Enter; pane run submits command text with Enter"
    ));
}

#[test]
fn channel_commands_fail_closed_without_creating_update_config() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base =
        std::env::temp_dir().join(format!("herdr-channel-disabled-{}-{nanos}", process::id()));
    let home_dir = base.join("home");
    let config_dir = base.join("config");
    fs::create_dir_all(&home_dir).unwrap();
    fs::write(home_dir.join("sentinel.txt"), "keep\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["channel", "set", "preview"])
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_dir)
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SOCKET_PATH_EXPLICIT")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("update channels are disabled in the kazuph/herdr fork"));
    assert!(
        !config_dir.exists(),
        "disabled channel commands must not write config"
    );
    assert_eq!(
        fs::read_to_string(home_dir.join("sentinel.txt")).unwrap(),
        "keep\n"
    );

    fs::remove_dir_all(base).unwrap();
}
