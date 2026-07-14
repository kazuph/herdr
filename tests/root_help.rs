use std::process::Command;

#[test]
fn root_help_is_skill_style_and_matches_help_command() {
    let dash_help = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .output()
        .unwrap();
    let help_command = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("help")
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
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("herdr client"),
        "root help should not advertise the internal client command: {stdout}"
    );
}
