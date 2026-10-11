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
//!   zdotdir/      injected ZDOTDIR holding only .zshenv: restores the
//!                 original ZDOTDIR, chain-loads the user's .zshenv, then
//!                 installs record-only wrappers without shadowing
//!                 functions the user already defined (e.g. ai_guard).
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
# herdr command guard shim for `{target}` (record-only). Generated; do not edit.
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
/// same technique terminal emulators use for shell integration. zsh then
/// reads `.zprofile`/`.zshrc`/`.zlogin` from the restored directory on its
/// own, so no chain files are needed for those.
///
/// After sourcing the user's `.zshenv` it defines record-only wrappers for
/// target commands, but only for names that are not already a function or
/// alias: existing wrappers such as ai_guard's must keep their behavior —
/// they still end up calling the real binary through `command`, which
/// resolves through the PATH shims and gets recorded there.
///
/// The `accept-line` recorder is installed from `precmd` so it runs after
/// the user's `.zshrc` and wraps whatever `accept-line` widget exists then
/// (including one installed by ai_guard), instead of replacing it.
fn zshenv_script(targets: &[String]) -> String {
    let herdr = r#""${HERDR_GUARD_HERDR:-herdr}""#;
    let target_words = targets.join(" ");
    format!(
        r#"# herdr command guard .zshenv (record-only). Generated; do not edit.

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
  _herdr_guard_wrap() {{
    local _hg_t="$1"
    (( $+functions[$_hg_t] || $+aliases[$_hg_t] )) && return 0
    functions[$_hg_t]='if [[ -f "${{{MARKER_ENV_VAR}:-}}" ]]; then HERDR_GUARD_VIA=shell-func '"${{HERDR_GUARD_HERDR:-herdr}}"' guard exec -- '"$_hg_t"' "$@"; else builtin command '"$_hg_t"' "$@"; fi'
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
        r#"# herdr command guard BASH_ENV (record-only). Generated; do not edit.
if [ -n "${{{BASH_ENV_ORIG_ENV_VAR}:-}}" ] && [ -f "${{{BASH_ENV_ORIG_ENV_VAR}}}" ]; then
  . "${{{BASH_ENV_ORIG_ENV_VAR}}}"
fi
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
}
