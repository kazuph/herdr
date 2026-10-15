#![cfg(unix)]

use std::process::Command;

#[test]
fn guard_exec_herdr_recovers_from_removed_executable() {
    let root = std::env::temp_dir().join(format!("herdr-stale-exe-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_herdr"))
            .env_clear()
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", &root)
            .env("XDG_STATE_HOME", &root)
            .env("PATH", "/usr/bin:/bin")
            .env("HERDR_GUARD_HERDR", root.join("removed/bin/herdr"))
            .args(args)
            .output()
            .unwrap()
    };
    let direct = run(&["--version"]);
    let guarded = run(&["guard", "exec", "--", "herdr", "--version"]);
    assert!(direct.status.success(), "{direct:?}");
    assert!(guarded.status.success(), "{guarded:?}");
    assert_eq!(guarded.stdout, direct.stdout);
    assert_eq!(guarded.stderr, direct.stderr);
    std::fs::remove_dir_all(root).unwrap();
}
