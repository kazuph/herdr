//! Guard assets materialized under `<state dir>/guard` and the pane env
//! injection that activates them.
//!
//! Asset layout:
//!
//! ```text
//! <state>/guard/
//!   bin/<p_N>/    per-pane shim directory, prepended to that pane's PATH.
//!                 Empty while the pane has no agent marker, so unmarked
//!                 commands resolve through PATH with no extra process.
//!                 While a marker exists it holds one forwarder script per
//!                 target that runs `herdr guard exec`.
//!   zdotdir/      injected zsh startup phases: restores the
//!                 original ZDOTDIR, chain-loads the user's .zshenv, then
//!                 installs wrappers without shadowing functions
//!                 the user already defined (e.g. ai_guard).
//!   guard.bash    injected BASH_ENV: chain-loads the original BASH_ENV and
//!                 installs the same non-shadowing wrappers.
//!   agent-panes/  per-pane agent markers (see marker.rs)
//! ```
//!
//! Everything generated is rewritten only when its content changes, so
//! upgrading herdr or changing `[guard].targets` refreshes the assets on the
//! next pane spawn or marker transition.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;

use portable_pty::CommandBuilder;

use crate::guard::{
    BASH_ENV_ORIG_ENV_VAR, ENFORCING_ENV_VAR, HERDR_EXE_ENV_VAR, MARKER_ENV_VAR, SHIM_DIR_ENV_VAR,
    ZDOTDIR_ORIG_ENV_VAR,
};

/// Apply the guard environment to a pane spawn. `pane_env_id` is the
/// `HERDR_PANE_ID` value (`p_<n>`) when the pane has managed identity.
///
/// Only identity-bearing terminal panes are injected: overlay/popup spawns
/// (`without_pane_identity`, e.g. the scrollback editor) and identity-less
/// launches are not user command surfaces and get no hooks. The hooks are
/// inert unless the pane's marker file exists, which only happens while
/// herdr believes an agent runs in it. Never injects when
/// `[guard].enabled = false`.
#[cfg(unix)]
pub(crate) fn apply_guard_env(cmd: &mut CommandBuilder, pane_env_id: Option<&str>) {
    let Some(pane_env_id) = pane_env_id else {
        return;
    };
    let loaded = crate::config::Config::load();
    let guard = &loaded.config.guard;
    if !guard.enabled {
        return;
    }
    let Ok(herdr_exe) = std::env::current_exe() else {
        return;
    };
    let targets = crate::guard::targets(guard);

    if let Err(error) = ensure_assets(&herdr_exe, &targets) {
        tracing::warn!(%error, "guard: failed to materialize assets; guard disabled for this pane");
        return;
    }

    // Each pane gets its own shim directory at the front of PATH. It stays
    // empty until the pane is marked as running an agent, so unmarked
    // command lookups fall straight through to the real PATH entries with
    // no extra process spawn.
    let shim = crate::guard::pane_shim_dir(pane_env_id);
    if let Err(error) = std::fs::create_dir_all(&shim) {
        tracing::warn!(%error, "guard: failed to create pane shim dir; guard disabled for this pane");
        return;
    }
    let base_path = cmd
        .get_env("PATH")
        .and_then(OsStr::to_str)
        .map(str::to_string)
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    cmd.env("PATH", format!("{}:{base_path}", shim.to_string_lossy()));

    cmd.env(SHIM_DIR_ENV_VAR, shim.to_string_lossy().as_ref());
    cmd.env(HERDR_EXE_ENV_VAR, herdr_exe.to_string_lossy().as_ref());
    cmd.env(
        MARKER_ENV_VAR,
        crate::guard::marker_path(pane_env_id)
            .to_string_lossy()
            .as_ref(),
    );

    let shell = cmd
        .get_argv()
        .first()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .or_else(|| cmd.get_env("SHELL").and_then(OsStr::to_str))
        .unwrap_or("");
    let shell_name = Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let interactive_bash = shell_name == "bash"
        && !cmd.get_argv().iter().skip(1).any(|arg| {
            arg.to_str().is_some_and(|arg| {
                arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c')
            })
        });
    let supported = matches!(shell_name, "zsh" | "bash")
        || (cmd.get_env("HERDR_GUARD_PANE_SHELL").is_none()
            && (!matches!(shell_name, "sh" | "fish" | "dash" | "ksh")
                || (matches!(shell_name, "sh" | "dash")
                    && cmd.get_argv().iter().any(|arg| arg == "-c")))
            && cmd
                .get_env("SHELL")
                .and_then(OsStr::to_str)
                .and_then(|shell| Path::new(shell).file_name())
                .and_then(OsStr::to_str)
                .is_some_and(|shell| matches!(shell, "zsh" | "bash")));
    cmd.env_remove(ENFORCING_ENV_VAR);
    if supported && !interactive_bash && guard.mode == crate::config::GuardModeConfig::Enforce {
        cmd.env(ENFORCING_ENV_VAR, "1");
    }
    cmd.env(
        "HERDR_GUARD_STARTUP",
        if supported {
            "supported"
        } else {
            "unsupported"
        },
    );
    cmd.env(
        "HERDR_GUARD_MODE",
        if guard.mode == crate::config::GuardModeConfig::Enforce {
            "enforce"
        } else {
            "record"
        },
    );
    if interactive_bash {
        let ready = crate::guard::guard_root().join("ready").join(pane_env_id);
        if let Some(parent) = ready.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&ready);
        cmd.env("HERDR_GUARD_READY", ready.to_string_lossy().as_ref());
        let original = cmd
            .get_env("PROMPT_COMMAND")
            .and_then(OsStr::to_str)
            .unwrap_or("");
        let enforcing = if guard.mode == crate::config::GuardModeConfig::Enforce {
            "export HERDR_GUARD_ENFORCING=1\n"
        } else {
            ""
        };
        cmd.env(
            "PROMPT_COMMAND",
            format!(
                "{original}\n{}{enforcing}builtin printf '' > \"$HERDR_GUARD_READY\"",
                pin_path_script()
            ),
        );
    }

    // zsh: point ZDOTDIR at our .zshenv. The original ZDOTDIR (if any) is
    // recorded so the injected file can restore it before the user's
    // startup files run, keeping ZDOTDIR-derived paths like HISTFILE
    // unchanged.
    if let Some(zdot_orig) = cmd
        .get_env("ZDOTDIR")
        .map(OsStr::to_os_string)
        .filter(|value| !value.is_empty())
    {
        cmd.env(ZDOTDIR_ORIG_ENV_VAR, zdot_orig.to_string_lossy().as_ref());
    }
    cmd.env(
        "HERDR_GUARD_SHELL_DIR",
        crate::guard::zdotdir().to_string_lossy().as_ref(),
    );
    cmd.env(
        "ZDOTDIR",
        crate::guard::zdotdir().to_string_lossy().as_ref(),
    );

    // Non-interactive bash reads BASH_ENV; interactive bash acknowledges
    // the PATH only after its startup files, via PROMPT_COMMAND above.
    if let Some(bash_env) = cmd
        .get_env("BASH_ENV")
        .and_then(OsStr::to_str)
        .map(str::to_owned)
    {
        if !bash_env.is_empty() {
            cmd.env(BASH_ENV_ORIG_ENV_VAR, bash_env);
        }
    }
    cmd.env(
        "BASH_ENV",
        crate::guard::bash_env_file().to_string_lossy().as_ref(),
    );
}

#[cfg(not(unix))]
pub(crate) fn apply_guard_env(_cmd: &mut CommandBuilder, _pane_env_id: Option<&str>) {
    // POSIX-only feature (sh/zsh/bash); a no-op elsewhere.
}

/// Materialize the shared guard assets, rewriting only files whose content
/// changed. Per-pane shim directories are handled separately by
/// `arm_pane_shims`/`disarm_pane_shims`.
pub(crate) fn ensure_assets(herdr_exe: &Path, targets: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::guard::shim_dir())?;

    let zdotdir = crate::guard::zdotdir();
    std::fs::create_dir_all(&zdotdir)?;
    write_if_changed(
        &zdotdir.join(".zshenv"),
        zshenv_script(targets).as_bytes(),
        0o644,
    )?;
    for name in [".zprofile", ".zshrc", ".zlogin"] {
        write_if_changed(
            &zdotdir.join(name),
            zsh_startup_script(name, targets).as_bytes(),
            0o644,
        )?;
    }

    write_if_changed(
        &crate::guard::bash_env_file(),
        bash_env_script(herdr_exe, targets).as_bytes(),
        0o644,
    )?;
    Ok(())
}

/// Populate the pane's shim directory with one forwarder per target command.
/// Called when the pane gains an agent marker, before the marker file is
/// written, so a command racing the transition is judged (guard exec also
/// checks the marker itself).
pub(crate) fn arm_pane_shims(pane_env_id: &str, targets: &[String]) {
    let dir = crate::guard::pane_shim_dir(pane_env_id);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::debug!(%error, "guard: failed to create pane shim dir");
        return;
    }
    for target in targets {
        if let Err(error) =
            write_if_changed(&dir.join(target), shim_script(target).as_bytes(), 0o755)
        {
            tracing::debug!(%error, "guard: failed to write shim");
        }
    }
}

/// Remove every forwarder from the pane's shim directory so unmarked
/// commands fall through PATH again. Called when the pane loses its marker,
/// after the marker file is removed.
pub(crate) fn disarm_pane_shims(pane_env_id: &str) {
    let dir = crate::guard::pane_shim_dir(pane_env_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let _ = std::fs::remove_dir(&dir);
}

/// Disarm every pane shim directory. Called alongside `marker::clear_all`
/// at server startup so a dead server cannot leave active forwarders.
pub(crate) fn disarm_all_pane_shims() {
    let Ok(entries) = std::fs::read_dir(crate::guard::shim_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let Ok(children) = std::fs::read_dir(&path) else {
                continue;
            };
            for child in children.flatten() {
                let _ = std::fs::remove_file(child.path());
            }
            let _ = std::fs::remove_dir(&path);
        } else {
            // Remove pre-per-pane-era flat shim files if any remain.
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn write_if_changed(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    if std::fs::read(path)
        .map(|existing| existing == content)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Forwarder placed in the pane's shim directory while its agent marker
/// exists. It simply hands the command to `herdr guard exec`, which judges
/// and records (when the marker is still present) and always runs the real
/// command. Resolution of the real binary happens inside `guard exec` on
/// the sanitized PATH, so the shim carries no path logic at all.
fn shim_script(target: &str) -> String {
    format!(
        r#"#!/bin/sh
# herdr command guard shim for `{target}`. Generated; do not edit.
exec "${{{HERDR_EXE_ENV_VAR}:-herdr}}" guard exec -- {} "$@"
"#,
        shell_quote(target),
        target = target,
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
    )
}

/// Injected `.zshenv` for the guard ZDOTDIR.
///
/// Order matters: the file first restores `ZDOTDIR` to its original value
/// (or unsets it), so the user's `.zshenv` and every path derived from
/// `ZDOTDIR` (e.g. `HISTFILE`) behave exactly as without injection — the
/// same technique terminal emulators use for shell integration. Each phase
/// chain-loads the user file, captures any ZDOTDIR change, and pins PATH.
/// Only intervals between startup files use the injected directory.
///
/// After sourcing the user's `.zshenv` it defines wrappers for
/// target commands, but only for names that are not already a function or
/// alias: existing wrappers such as ai_guard's must keep their behavior —
/// they still end up calling the real binary through `command`, which
/// resolves through the PATH shims and gets recorded there.
///
/// The `accept-line` recorder is installed from `precmd` so it runs after
/// the user's `.zshrc` and wraps whatever `accept-line` widget exists then
/// (including one installed by ai_guard), instead of replacing it.
fn pin_path_script() -> &'static str {
    r#"case "$PATH" in "$HERDR_GUARD_SHIM_DIR"|"$HERDR_GUARD_SHIM_DIR":*) ;; *) export PATH="$HERDR_GUARD_SHIM_DIR:$PATH";; esac
"#
}

fn zshenv_script(targets: &[String]) -> String {
    zsh_startup_script(".zshenv", targets)
}

fn zsh_startup_script(name: &str, targets: &[String]) -> String {
    let next = match name {
        ".zshenv" => "[[ -o login || -o interactive ]]",
        ".zprofile" => "true",
        ".zshrc" => "[[ -o login ]]",
        _ => "false",
    };
    let startup = format!(
        r#"typeset -g _hg_orig_zdot="${{HERDR_GUARD_ZDOTDIR_ORIG:-}}"
if [[ -n "$_hg_orig_zdot" ]]; then
  export ZDOTDIR="$_hg_orig_zdot"
else
  unset ZDOTDIR
fi
if [[ -r "${{_hg_orig_zdot:-$HOME}}/{name}" ]]; then
  source "${{_hg_orig_zdot:-$HOME}}/{name}"
fi
export HERDR_GUARD_ZDOTDIR_ORIG="${{ZDOTDIR:-}}"
unset _hg_orig_zdot
{pin}
if {next}; then export ZDOTDIR="${{HERDR_GUARD_SHELL_DIR}}"; fi
"#,
        pin = pin_path_script()
    );
    if name != ".zshenv" {
        return startup;
    }
    let herdr = r#""${HERDR_GUARD_HERDR:-herdr}""#;
    let target_words = targets.join(" ");
    format!(
        r#"# herdr command guard .zshenv. Generated; do not edit.

{startup}

if [[ -n "${{{HERDR_EXE_ENV_VAR}:-}}" ]]; then
  _herdr_guard_wrap() {{
    local _hg_t="$1"
    (( $+functions[$_hg_t] || $+aliases[$_hg_t] )) && return 0
    functions[$_hg_t]='if [[ -f "${{{MARKER_ENV_VAR}:-}}" ]]; then {pin}HERDR_GUARD_VIA=shell-func '"${{HERDR_GUARD_HERDR:-herdr}}"' guard exec -- '"$_hg_t"' "$@"; else builtin command '"$_hg_t"' "$@"; fi'
  }}
  for _hg_t in {target_words}; do
    _herdr_guard_wrap "$_hg_t"
  done
  unfunction _herdr_guard_wrap
  unset _hg_t
  (( $+functions[command] || $+aliases[command] )) || command() {{
    local _hg_cmd="$1"
    case " {target_words} " in
      *" $_hg_cmd "*)
        shift
        if [[ -f "${{{MARKER_ENV_VAR}:-}}" ]]; then
          {pin}HERDR_GUARD_VIA=shell-func {herdr} guard exec -- "$_hg_cmd" "$@"
        else
          builtin command "$_hg_cmd" "$@"
        fi
        ;;
      *) builtin command "$@" ;;
    esac
  }}
fi

if [[ -o interactive && -n "${{{HERDR_EXE_ENV_VAR}:-}}" ]]; then
  _herdr_guard_precmd() {{
    zle -A accept-line herdr-guard-orig-accept-line 2>/dev/null
    herdr-guard-accept-line() {{
      if [[ -f "${{{MARKER_ENV_VAR}:-}}" ]]; then
        "${{HERDR_GUARD_HERDR:-herdr}}" guard line -- "$BUFFER" >/dev/null 2>&1 &!
      fi
      zle herdr-guard-orig-accept-line
    }}
    zle -N accept-line herdr-guard-accept-line
    precmd_functions=(${{precmd_functions:#_herdr_guard_precmd}})
  }}
  precmd_functions+=(_herdr_guard_precmd)
fi
"#,
        startup = startup,
        pin = pin_path_script(),
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
        MARKER_ENV_VAR = MARKER_ENV_VAR,
        target_words = target_words,
        herdr = herdr,
    )
}

/// `BASH_ENV` content: chain-loads the original BASH_ENV, then installs the
/// same wrappers as zsh for non-interactive bash. Names that already exist
/// as a function or alias (e.g. ai_guard's wrappers loaded via the original
/// BASH_ENV) are left untouched; the PATH shims still record the command
/// when the original wrapper runs the real binary.
fn bash_env_script(herdr_exe: &Path, targets: &[String]) -> String {
    let herdr = shell_quote(&herdr_exe.to_string_lossy());
    let target_words = targets.join(" ");
    format!(
        r#"# herdr command guard BASH_ENV. Generated; do not edit.
if [ -n "${{{BASH_ENV_ORIG_ENV_VAR}:-}}" ] && [ -f "${{{BASH_ENV_ORIG_ENV_VAR}}}" ]; then
  . "${{{BASH_ENV_ORIG_ENV_VAR}}}"
fi
{pin}
if [ -n "${{{HERDR_EXE_ENV_VAR}:-}}" ]; then
  _herdr_guard_wrap() {{
    declare -F "$1" >/dev/null 2>&1 && return 0
    alias "$1" >/dev/null 2>&1 && return 0
    eval "$1() {{ if [ -f \"\${{{MARKER_ENV_VAR}:-}}\" ]; then HERDR_GUARD_VIA=shell-func \"\${{{HERDR_EXE_ENV_VAR}:-herdr}}\" guard exec -- '$1' \"\$@\"; else builtin command $1 \"\$@\"; fi; }}"
  }}
  for _hg_t in {target_words}; do
    _herdr_guard_wrap "$_hg_t"
  done
  unset -f _herdr_guard_wrap
  unset _hg_t
  declare -F command >/dev/null 2>&1 || command() {{
    local _hg_cmd="$1"
    case " {target_words} " in
      *" $_hg_cmd "*)
        shift
        if [ -f "${{{MARKER_ENV_VAR}:-}}" ]; then
          HERDR_GUARD_VIA=shell-func {herdr} guard exec -- "$_hg_cmd" "$@"
        else
          builtin command "$_hg_cmd" "$@"
        fi
        ;;
      *) builtin command "$@" ;;
    esac
  }}
fi
"#,
        pin = pin_path_script(),
        BASH_ENV_ORIG_ENV_VAR = BASH_ENV_ORIG_ENV_VAR,
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
        MARKER_ENV_VAR = MARKER_ENV_VAR,
        target_words = target_words,
        herdr = herdr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armed_shim_forwards_to_guard_exec() {
        let script = shim_script("rm");
        assert!(script.contains("guard exec -- 'rm'"));
        assert!(script.contains("HERDR_GUARD_HERDR"));
        // No PATH manipulation and no marker check: the shim exists only
        // while the pane is marked, and guard exec re-checks the marker.
        assert!(!script.contains("awk"));
        assert!(!script.contains(MARKER_ENV_VAR));
    }

    #[test]
    fn zshenv_restores_zdotdir_and_does_not_shadow() {
        let targets = vec!["rm".to_string(), "git".to_string()];
        let script = zshenv_script(&targets);
        assert!(script.contains("export ZDOTDIR=\"$_hg_orig_zdot\""));
        assert!(script.contains("unset ZDOTDIR"));
        assert!(script.contains("source \"${_hg_orig_zdot:-$HOME}/.zshenv\""));
        assert!(script.contains("$+functions[$_hg_t]"));
        assert!(script.contains("$+aliases[$_hg_t]"));
        assert!(script.contains("$+functions[command]"));
        assert!(script.contains("_herdr_guard_precmd"));
        assert!(script.contains(MARKER_ENV_VAR));
    }

    #[test]
    fn bash_env_chain_loads_and_does_not_shadow() {
        let script = bash_env_script(Path::new("/usr/local/bin/herdr"), &["rm".to_string()]);
        assert!(script.contains("HERDR_GUARD_BASH_ENV_ORIG"));
        assert!(script.contains("declare -F \"$1\""));
        assert!(script.contains("declare -F command"));
        assert!(script.contains("HERDR_GUARD_MARKER"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn login_zsh_keeps_shim_first_after_startup() {
        let root = std::env::temp_dir().join(format!("hga-startup-{}", std::process::id()));
        let user = root.join("user");
        let injected = root.join("injected");
        let shim = root.join("shim");
        for dir in [&user, &injected, &shim] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(
                injected.join(name),
                zsh_startup_script(name, &["rm".into()]),
            )
            .unwrap();
        }
        for name in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            std::fs::write(
                user.join(name),
                "export PATH=/usr/bin:/bin:$PATH\nHISTFILE=$ZDOTDIR/.zsh_history\n",
            )
            .unwrap();
        }
        let output = std::process::Command::new("/bin/zsh")
            .env_clear()
            .env("HOME", &user)
            .env("ZDOTDIR", &injected)
            .env("HERDR_GUARD_SHELL_DIR", &injected)
            .env(ZDOTDIR_ORIG_ENV_VAR, &user)
            .env(SHIM_DIR_ENV_VAR, &shim)
            .env(HERDR_EXE_ENV_VAR, "/usr/bin/true")
            .env("PATH", format!("{}:/usr/bin:/bin", shim.display()))
            .args([
                "-lic",
                "print -r -- $PATH; print -r -- $ZDOTDIR; print -r -- $HISTFILE",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let path = String::from_utf8(output.stdout).unwrap();
        assert!(path.starts_with(&format!("{}:", shim.display())), "{path}");
        assert!(path.contains(&shim.to_string_lossy().to_string()), "{path}");
        let lines: Vec<_> = path.lines().collect();
        assert_eq!(lines[1], user.to_string_lossy());
        assert_eq!(lines[2], user.join(".zsh_history").to_string_lossy());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn interactive_bash_acknowledges_path_only_after_user_startup() {
        let _lock = crate::config::lock_test_config_env();
        let root = std::env::temp_dir().join(format!("hga-bash-{}", std::process::id()));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "[guard]\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::fs::write(home.join(".bash_profile"),
            "export PATH=/usr/bin:/bin:$PATH\nprintf 'before=%s\\n' \"${HERDR_GUARD_ENFORCING:-}\"\n").unwrap();
        let mut builder = CommandBuilder::new("/bin/bash");
        builder.env_clear();
        builder.env("HOME", &home);
        builder.env("SHELL", "/bin/bash");
        builder.env("PATH", "/usr/bin:/bin");
        apply_guard_env(&mut builder, Some("p_bash"));
        assert!(builder.get_env(ENFORCING_ENV_VAR).is_none());
        let ready = builder.get_env("HERDR_GUARD_READY").unwrap();
        assert!(!Path::new(ready).exists());
        let mut command = std::process::Command::new("/bin/bash");
        command
            .env_clear()
            .envs(builder.iter_full_env_as_str())
            .args(["--login", "-i"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"printf 'after=%s|%s\\n' \"${HERDR_GUARD_ENFORCING:-}\" \"$PATH\"\nexit\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("before=\n"), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "after=1|{}:",
                crate::guard::pane_shim_dir("p_bash").display()
            )),
            "{stdout}"
        );
        assert!(Path::new(ready).is_file());
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::remove_var("XDG_STATE_HOME");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_pane_shell_never_suppresses_ai_guard() {
        let _lock = crate::config::lock_test_config_env();
        let root = std::env::temp_dir().join(format!("hga-unsupported-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, "[guard]\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        let mut builder = CommandBuilder::new("/bin/sh");
        builder.env("HERDR_GUARD_PANE_SHELL", "1");
        builder.env(ENFORCING_ENV_VAR, "1");
        apply_guard_env(&mut builder, Some("p_unsupported"));
        assert!(builder.get_env(ENFORCING_ENV_VAR).is_none());
        assert_eq!(
            builder
                .get_env("HERDR_GUARD_STARTUP")
                .and_then(OsStr::to_str),
            Some("unsupported")
        );
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::remove_var("XDG_STATE_HOME");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_if_changed_sets_mode_and_skips_unchanged() {
        let root = std::env::temp_dir().join(format!("herdr-guard-assets-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("rm");
        write_if_changed(&file, b"#!/bin/sh\n", 0o755).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        write_if_changed(&file, b"#!/bin/sh\n", 0o755).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), mtime);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn arm_and_disarm_pane_shims() {
        let pane_id = format!("p_test_{}", std::process::id());
        let dir = crate::guard::pane_shim_dir(&pane_id);
        let targets = vec!["rm".to_string(), "git".to_string()];
        arm_pane_shims(&pane_id, &targets);
        assert!(dir.join("rm").is_file());
        assert!(dir.join("git").is_file());
        disarm_pane_shims(&pane_id);
        assert!(!dir.exists());
    }

    #[test]
    fn apply_guard_env_marks_enforcing_panes_for_ai_guard_skip() {
        let _lock = crate::config::lock_test_config_env();
        let root = std::env::temp_dir().join(format!("hga-env-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        let config_path = root.join("config.toml");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);

        // CommandBuilder::new inherits this process's environment; drop the
        // guard vars so only what apply_guard_env sets is measured.
        let builder = || {
            let mut builder = CommandBuilder::new("zsh");
            builder.env_remove(ENFORCING_ENV_VAR);
            builder.env_remove(MARKER_ENV_VAR);
            builder
        };

        // Enforce (the default): panes get HERDR_GUARD_ENFORCING=1 so the
        // dotfiles ai_guard loader can skip itself.
        std::fs::write(&config_path, "[guard]\n").unwrap();
        let mut enforcing = builder();
        apply_guard_env(&mut enforcing, Some("p_envtest"));
        assert_eq!(
            enforcing.get_env(ENFORCING_ENV_VAR).and_then(OsStr::to_str),
            Some("1")
        );

        // Record mode: no marker — the pane's ai_guard (if any) keeps
        // handling confirmations itself.
        std::fs::write(&config_path, "[guard]\nmode = \"record\"\n").unwrap();
        let mut recording = builder();
        apply_guard_env(&mut recording, Some("p_envtest"));
        assert!(recording.get_env(ENFORCING_ENV_VAR).is_none());

        // Disabled guard: no injection at all.
        std::fs::write(&config_path, "[guard]\nenabled = false\n").unwrap();
        let mut disabled = builder();
        apply_guard_env(&mut disabled, Some("p_envtest"));
        assert!(disabled.get_env(ENFORCING_ENV_VAR).is_none());
        assert!(disabled.get_env(MARKER_ENV_VAR).is_none());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(crate::guard::pane_shim_dir("p_envtest"));
    }
}
