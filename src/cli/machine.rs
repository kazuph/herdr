//! `herdr machine` saved-SSH-machine management (G11, P1).
//!
//! Thin CLI wrapper over [`crate::machine`]: parsing, human/JSON rendering,
//! and exit codes live here; catalog rules live in the core module.

use crate::machine::{self, MachineCatalog};

const HELP: &str = "Usage:
  herdr machine list [--json]
  herdr machine add <ssh-target> --label <label> [--remote-session <name>]
  herdr machine rename <label-or-id> --label <label>
  herdr machine remove <label-or-id>
  herdr machine enable <label-or-id>
  herdr machine disable <label-or-id>

Saved machines contain only a label, SSH target, explicit Herdr session, and enabled state.
SSH credentials and key material remain owned by OpenSSH.
Adding probes SSH reachability first; unreachable machines are not saved.
Removing or disabling a machine leaves its remote sessions running.";

pub(super) fn run_machine_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("add") => add(&args[1..]),
        Some("rename") => rename(&args[1..]),
        Some("remove") => remove(&args[1..]),
        Some("enable") => set_enabled(&args[1..], true),
        Some("disable") => set_enabled(&args[1..], false),
        Some("help" | "--help" | "-h") => {
            println!("{HELP}");
            Ok(0)
        }
        _ => {
            eprintln!("{HELP}");
            Ok(2)
        }
    }
}

fn render_list(catalog: &MachineCatalog) -> String {
    if catalog.profiles.is_empty() {
        return "No saved SSH machines.".to_string();
    }
    catalog
        .profiles
        .iter()
        .map(|profile| {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                profile.id,
                profile.label,
                profile.target,
                profile.session,
                if profile.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list_json(catalog: &MachineCatalog) -> Result<String, String> {
    serde_json::to_string_pretty(&catalog.profiles).map_err(|err| err.to_string())
}

fn list(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: herdr machine list [--json]");
            return Ok(2);
        }
    };
    let catalog = machine::load();
    if json {
        match render_list_json(&catalog) {
            Ok(rendered) => println!("{rendered}"),
            Err(err) => {
                eprintln!("error: {err}");
                return Ok(1);
            }
        }
        return Ok(0);
    }
    println!("{}", render_list(&catalog));
    Ok(0)
}

#[derive(Debug, PartialEq, Eq)]
struct AddArgs {
    target: String,
    label: String,
    session: String,
}

/// Split `--opt=value` into two args for the options we accept; anything else
/// passes through so unknown options keep their exact spelling in errors.
fn expand_equals_arg(arg: &str) -> Vec<String> {
    for option in ["--label", "--remote-session"] {
        if let Some(value) = arg
            .strip_prefix(option)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return vec![option.to_string(), value.to_string()];
        }
    }
    vec![arg.to_string()]
}

fn parse_add_args(raw: &[String]) -> Result<AddArgs, String> {
    let args: Vec<String> = raw.iter().flat_map(|a| expand_equals_arg(a)).collect();
    let mut target = None;
    let mut label = None;
    let mut session = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--label" | "--remote-session" => {
                let name = args[index].clone();
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("missing value for {}", args[index]));
                };
                if name == "--label" {
                    if label.is_some() {
                        return Err("--label can only be specified once".to_string());
                    }
                    label = Some(value.clone());
                } else {
                    if session.is_some() {
                        return Err("--remote-session can only be specified once".to_string());
                    }
                    session = Some(value.clone());
                }
                index += 2;
            }
            positional if !positional.starts_with('-') && target.is_none() => {
                target = Some(positional.to_string());
                index += 1;
            }
            unknown => {
                return Err(format!("unknown machine add option: {unknown}"));
            }
        }
    }
    let target = target.ok_or_else(|| {
        "usage: herdr machine add <ssh-target> --label <label> [--remote-session <name>]"
            .to_string()
    })?;
    let label = label.ok_or_else(|| "--label is required".to_string())?;
    let session = session.unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    Ok(AddArgs {
        target,
        label,
        session,
    })
}

fn add(args: &[String]) -> std::io::Result<i32> {
    let AddArgs {
        target,
        label,
        session,
    } = match parse_add_args(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    let mut catalog = machine::load();
    // Validate before probing so usage errors never trigger network I/O.
    let mut staged = catalog.clone();
    if let Err(error) = staged.add(&label, &target, &session) {
        eprintln!("error: {error}");
        return Ok(2);
    }
    if let Err(error) = machine::probe_ssh_reachable(&target) {
        eprintln!("error: {error}; machine was not saved");
        return Ok(1);
    }
    // Reload: the probe may have taken a while; do not overwrite concurrent edits.
    catalog = machine::load();
    let id = match catalog.add(&label, &target, &session) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    };
    if let Err(error) = machine::save(&catalog) {
        eprintln!("error: remote reachable, but machine was not saved: {error}");
        return Ok(1);
    }
    println!("Saved SSH machine {id}.");
    Ok(0)
}

fn parse_id_arg(args: &[String], usage: &str) -> Result<String, i32> {
    match args {
        [raw] => Ok(raw.clone()),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn rename(args: &[String]) -> std::io::Result<i32> {
    let expanded: Vec<String> = args.iter().flat_map(|a| expand_equals_arg(a)).collect();
    let (raw_id, label) = match expanded.as_slice() {
        [raw_id, flag, label] if flag == "--label" => (raw_id.clone(), label.clone()),
        _ => {
            eprintln!("usage: herdr machine rename <label-or-id> --label <label>");
            return Ok(2);
        }
    };
    let mut catalog = machine::load();
    let id = match catalog.resolve(&raw_id) {
        Ok(profile) => profile.id.clone(),
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(1);
        }
    };
    match catalog.rename(&id, &label) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("machine {raw_id:?} was not found");
            return Ok(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    }
    if let Err(error) = machine::save(&catalog) {
        eprintln!("error: renamed in memory, but machine was not saved: {error}");
        return Ok(1);
    }
    println!("Renamed SSH machine {id}.");
    Ok(0)
}

fn remove(args: &[String]) -> std::io::Result<i32> {
    let raw_id = match parse_id_arg(args, "usage: herdr machine remove <label-or-id>") {
        Ok(raw_id) => raw_id,
        Err(code) => return Ok(code),
    };
    let mut catalog = machine::load();
    let id = match catalog.resolve(&raw_id) {
        Ok(profile) => profile.id.clone(),
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(1);
        }
    };
    if !catalog.remove(&id) {
        eprintln!("machine {raw_id:?} was not found");
        return Ok(1);
    }
    if let Err(error) = machine::save(&catalog) {
        eprintln!("error: removed in memory, but machine was not saved: {error}");
        return Ok(1);
    }
    println!("Removed SSH machine {id}.");
    Ok(0)
}

fn set_enabled(args: &[String], enabled: bool) -> std::io::Result<i32> {
    let action = if enabled { "enable" } else { "disable" };
    let usage = format!("usage: herdr machine {action} <label-or-id>");
    let raw_id = match parse_id_arg(args, &usage) {
        Ok(raw_id) => raw_id,
        Err(code) => return Ok(code),
    };
    let mut catalog = machine::load();
    // Resolve first so unknown/ambiguous references fail before any write.
    // Disabled profiles resolve by id/label directly for enable.
    let id = match catalog.resolve(&raw_id) {
        Ok(profile) => profile.id.clone(),
        Err(_) => {
            // `enable` must accept currently-disabled profiles, which
            // `resolve` rejects; fall back to an exact id/label lookup.
            match catalog
                .profiles
                .iter()
                .find(|p| p.id == raw_id || p.label == raw_id)
            {
                Some(profile) => profile.id.clone(),
                None => {
                    eprintln!("error: machine {raw_id:?} was not found");
                    return Ok(1);
                }
            }
        }
    };
    if !catalog.set_enabled(&id, enabled) {
        eprintln!("machine {raw_id:?} was not found");
        return Ok(1);
    }
    if let Err(error) = machine::save(&catalog) {
        eprintln!("error: updated in memory, but machine was not saved: {error}");
        return Ok(1);
    }
    println!(
        "{} SSH machine {id}.",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_with(label: &str, target: &str) -> MachineCatalog {
        let mut catalog = MachineCatalog::default();
        catalog.add(label, target, "default").unwrap();
        catalog
    }

    #[test]
    fn parse_add_args_accepts_both_option_spellings() {
        let space = vec!["h".to_string(), "--label".to_string(), "l".to_string()];
        let equals = vec!["h".to_string(), "--label=l".to_string()];
        for raw in [space, equals] {
            let parsed = parse_add_args(&raw).unwrap();
            assert_eq!(
                parsed,
                AddArgs {
                    target: "h".to_string(),
                    label: "l".to_string(),
                    session: "default".to_string(),
                }
            );
        }
        assert!(parse_add_args(&["h".to_string()]).is_err());
        assert!(parse_add_args(&["--label".to_string(), "l".to_string()]).is_err());
        assert!(parse_add_args(&["h".to_string(), "--bogus".to_string()]).is_err());
        assert!(parse_add_args(&[
            "h".to_string(),
            "--label".to_string(),
            "a".to_string(),
            "--label".to_string(),
            "b".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn render_list_covers_empty_filled_and_json() {
        assert_eq!(
            render_list(&MachineCatalog::default()),
            "No saved SSH machines."
        );
        let catalog = catalog_with("office", "h1");
        let rendered = render_list(&catalog);
        assert!(rendered.contains("\toffice\th1\tdefault\tenabled"));
        let json = render_list_json(&catalog).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["label"], "office");
    }

    #[test]
    fn help_and_unknown_verbs_exit_correctly() {
        // Rendering contract only: unknown verbs print help with exit 2.
        assert!(HELP.contains("herdr machine add <ssh-target>"));
        assert!(HELP.contains("leaves its remote sessions running"));
    }
}
