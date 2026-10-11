//! Guard assets materialized under `<state dir>/guard` and the pane env
//! injection that activates them.
//!
//! Asset layout:
//!
//! ```text
//! <state>/guard/
//!   bin/            one POSIX shim per target command, prepended to PATH
//!   zdotdir/        injected ZDOTDIR: .zshenv/.zshrc/.zprofile/.zlogin that
//!                   chain-load the user's real startup files
//!   guard.bash      injected BASH_ENV: chain-loads the original BASH_ENV and
//!                   defines the same wrappers for interactive bash
//!   agent-panes/    per-pane agent markers (see marker.rs)
//! ```
//!
//! Everything in `bin/`/`zdotdir/`/`guard.bash` is regenerated when stale, so
//! upgrading herdr or changing `[guard].targets` refreshes the assets on the
//! next pane spawn.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;

use portable_pty::CommandBuilder;

use crate::guard::{
    BASH_ENV_ORIG_ENV_VAR, HERDR_EXE_ENV_VAR, MARKER_ENV_VAR, SHIM_DIR_ENV_VAR,
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

    // Guard shims go to the front of PATH.
    let shim = crate::guard::shim_dir();
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

    // zsh: point ZDOTDIR at our .zshenv. The original ZDOTDIR (if any) is
    // recorded so the injected file can restore it before the user's
    // startup files run, keeping ZDOTDIR-derived paths like HISTFILE
    // unchanged.
    if let Some(zdot_orig) = std::env::var_os("ZDOTDIR").filter(|value| !value.is_empty()) {
        cmd.env(ZDOTDIR_ORIG_ENV_VAR, zdot_orig.to_string_lossy().as_ref());
    }
    cmd.env(
        "ZDOTDIR",
        crate::guard::zdotdir().to_string_lossy().as_ref(),
    );

    // bash: non-interactive shells read BASH_ENV. Interactive bash panes get
    // no function coverage (BASH_ENV is ignored there); the PATH shims still
    // judge every target command, so coverage holds either way.
    if let Ok(bash_env) = std::env::var("BASH_ENV") {
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

/// Materialize all guard assets, rewriting only files whose content changed.
pub(crate) fn ensure_assets(herdr_exe: &Path, targets: &[String]) -> std::io::Result<()> {
    let shim = crate::guard::shim_dir();
    std::fs::create_dir_all(&shim)?;
    for target in targets {
        write_if_changed(&shim.join(target), shim_script(target).as_bytes(), 0o755)?;
    }
    remove_stale_shims(&shim, targets)?;

    let zdotdir = crate::guard::zdotdir();
    std::fs::create_dir_all(&zdotdir)?;
    write_if_changed(
        &zdotdir.join(".zshenv"),
        zshenv_script(targets).as_bytes(),
        0o644,
    )?;
    // Earlier guard builds injected chain-loading .zshrc/.zprofile/.zlogin
    // files. They are obsolete now that .zshenv restores ZDOTDIR before the
    // user's files run; remove leftovers so they cannot shadow anything.
    for name in [".zshrc", ".zprofile", ".zlogin"] {
        let _ = std::fs::remove_file(zdotdir.join(name));
    }

    write_if_changed(
        &crate::guard::bash_env_file(),
        bash_env_script(herdr_exe, targets).as_bytes(),
        0o644,
    )?;
    Ok(())
}

/// Remove shims for commands no longer in `targets` (renamed config, stale
/// binary from an older build) so PATH lookups stay predictable.
fn remove_stale_shims(shim_dir: &Path, targets: &[String]) -> std::io::Result<()> {
    let target_names: std::collections::BTreeSet<&str> =
        targets.iter().map(String::as_str).collect();
    for entry in std::fs::read_dir(shim_dir)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !target_names.contains(name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
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

/// POSIX shim prepended to `PATH` for each target command. When the pane's
/// marker exists the shim hands the command to `herdr guard exec` (which
/// judges + records and then runs the real command); without a marker it
/// strips its own directory from `PATH` and execs the real binary directly.
fn shim_script(target: &str) -> String {
    let shim_dir = crate::guard::shim_dir();
    format!(
        r#"#!/bin/sh
# herdr command guard shim for `{target}` (record-only). Generated; do not edit.
if [ -n "${{{MARKER_ENV_VAR}:-}}" ] && [ -f "${{{MARKER_ENV_VAR}}}" ]; then
  exec "${{{HERDR_EXE_ENV_VAR}:-herdr}}" guard exec -- {} "$@"
fi
PATH="$(printf '%s' "$PATH" | awk -v dir='{shim_dir}' 'BEGIN {{RS=":"}} $0 != dir {{printf "%s%s", sep, $0; sep=":"}}')"
exec {target} "$@"
"#,
        shell_quote(target),
        target = target,
        shim_dir = shim_dir.to_string_lossy(),
        MARKER_ENV_VAR = MARKER_ENV_VAR,
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
    )
}

/// Injected `.zshenv` for the guard ZDOTDIR. First restores `ZDOTDIR` to
/// its original value (or unsets it), then chain-loads the user's real
/// `.zshenv`, defines target-command functions and a `command` passthrough,
/// and defers the `accept-line` recorder to `precmd` so it wraps whatever
/// widget the user's `.zshrc` installed. The functions check the pane's
/// marker file at call time; without it commands resolve through the PATH
/// shims, which make the same check per invocation.
fn zshenv_script(targets: &[String]) -> String {
    let herdr = r#""${HERDR_GUARD_HERDR:-herdr}""#;
    let target_words = targets.join(" ");
    let function_defs = targets
        .iter()
        .map(|target| {
            format!(
                concat!(
                    "{target}() {{ if [[ -f \"${{{marker_var}:-}}\" ]]; then ",
                    "HERDR_GUARD_VIA=shell-func {herdr} guard exec -- {q} \"$@\"; ",
                    "else builtin command {target} \"$@\"; fi }}"
                ),
                target = target,
                marker_var = MARKER_ENV_VAR,
                herdr = herdr,
                q = shell_quote(target),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"# herdr command guard .zshenv (record-only). Generated; do not edit.
# Restores ZDOTDIR to its original value (or unsets it) before chain-loading
# the user's real .zshenv — the same technique terminal emulators use for
# shell integration — so ZDOTDIR-derived paths like HISTFILE stay unchanged.
# zsh then reads .zprofile/.zshrc/.zlogin from the restored directory on its
# own. Active only while this pane's marker file exists.

typeset -g _hg_orig_zdot="${{{ZDOTDIR_ORIG_ENV_VAR}:-}}"
if [[ -n "$_hg_orig_zdot" ]]; then
  export ZDOTDIR="$_hg_orig_zdot"
else
  unset ZDOTDIR
fi
if [[ -r "${{_hg_orig_zdot:-$HOME}}/.zshenv" ]]; then
  source "${{_hg_orig_zdot:-$HOME}}/.zshenv"
fi
unset _hg_orig_zdot

if [[ -n "${{{HERDR_EXE_ENV_VAR}:-}}" ]]; then
{function_defs}
command() {{
  local _hg_cmd="$1"
  case " {target_words} " in
    *" $_hg_cmd "*)
      shift
      if [[ -f "${{{MARKER_ENV_VAR}:-}}" ]]; then
        HERDR_GUARD_VIA=shell-func {herdr} guard exec -- "$_hg_cmd" "$@"
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
        ZDOTDIR_ORIG_ENV_VAR = ZDOTDIR_ORIG_ENV_VAR,
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
        MARKER_ENV_VAR = MARKER_ENV_VAR,
        function_defs = function_defs,
        target_words = target_words,
        herdr = herdr,
    )
}

/// `BASH_ENV` content: chain-loads the original BASH_ENV, then defines the
/// same wrappers as zsh for non-interactive bash. Like `.zshenv`, functions
/// are only defined when the pane's marker file exists.
fn bash_env_script(herdr_exe: &Path, targets: &[String]) -> String {
    let herdr = shell_quote(&herdr_exe.to_string_lossy());
    let target_words = targets.join(" ");
    let function_defs = targets
        .iter()
        .map(|target| {
            format!(
                concat!(
                    "{target}() {{ if [ -f \"${{{marker_var}:-}}\" ]; then ",
                    "HERDR_GUARD_VIA=shell-func {herdr} guard exec -- {q} \"$@\"; ",
                    "else builtin command {target} \"$@\"; fi; }}"
                ),
                target = target,
                marker_var = MARKER_ENV_VAR,
                herdr = herdr,
                q = shell_quote(target),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"# herdr command guard BASH_ENV (record-only). Generated; do not edit.
if [ -n "${{{BASH_ENV_ORIG_ENV_VAR}:-}}" ] && [ -f "${{{BASH_ENV_ORIG_ENV_VAR}}}" ]; then
  . "${{{BASH_ENV_ORIG_ENV_VAR}}}"
fi
if [ -n "${{{HERDR_EXE_ENV_VAR}:-}}" ]; then
{function_defs}
command() {{
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
        BASH_ENV_ORIG_ENV_VAR = BASH_ENV_ORIG_ENV_VAR,
        HERDR_EXE_ENV_VAR = HERDR_EXE_ENV_VAR,
        MARKER_ENV_VAR = MARKER_ENV_VAR,
        function_defs = function_defs,
        target_words = target_words,
        herdr = herdr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_script_strips_own_dir_and_execs_without_marker() {
        let script = shim_script("rm");
        assert!(script.contains("guard exec -- 'rm'"));
        let shim = crate::guard::shim_dir().to_string_lossy().to_string();
        assert!(script.contains(&shim));
        assert!(script.contains("exec rm"));
    }

    #[test]
    fn zshenv_restores_zdotdir_and_defines_functions() {
        let targets = vec!["rm".to_string(), "git".to_string()];
        let script = zshenv_script(&targets);
        assert!(script.contains("export ZDOTDIR=\"$_hg_orig_zdot\""));
        assert!(script.contains("unset ZDOTDIR"));
        assert!(script.contains("source \"${_hg_orig_zdot:-$HOME}/.zshenv\""));
        assert!(script.contains("rm() {"));
        assert!(script.contains("git() {"));
        assert!(script.contains("command()"));
        assert!(script.contains("_herdr_guard_precmd"));
    }

    #[test]
    fn bash_env_exports_chain_load() {
        let script = bash_env_script(Path::new("/usr/local/bin/herdr"), &["rm".to_string()]);
        assert!(script.contains("HERDR_GUARD_BASH_ENV_ORIG"));
        assert!(script.contains("HERDR_GUARD_MARKER"));
        assert!(script.contains(
            "rm() { if [ -f \"${HERDR_GUARD_MARKER:-}\" ]; then HERDR_GUARD_VIA=shell-func '/usr/local/bin/herdr' guard exec"
        ));
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
}
