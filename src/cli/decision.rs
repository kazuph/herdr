use std::time::Duration;

use crate::api::schema::{
    DecisionAnswerParams, DecisionCancelParams, DecisionCreateParams, DecisionGetParams,
    DecisionKind, DecisionListParams, DecisionOption, DecisionOptionRole, DecisionOrigin,
    DecisionStatus, DecisionWaitParams, Method, PaneCurrentParams, Request,
};

mod watch;

const DECISION_WAIT_RETRY_DELAY: Duration = Duration::from_millis(300);

const DECISION_ASK_USAGE: &str = "usage: herdr decision ask --kind <command_guard|tool_permission|agent_prompt|ask> --title <text> [--body <text>] --option <id:label[:approve|reject|other]>... [--allow-text] [--timeout <secs>]";
const DECISION_ANSWER_USAGE: &str =
    "usage: herdr decision answer <decision_id> <option_id> [--text <text>] [--responder <name>]";
const DECISION_LIST_USAGE: &str =
    "usage: herdr decision list [--status <pending|answered|expired|cancelled>]";

pub(super) fn run_decision_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_decision_help();
        return Ok(2);
    };

    match subcommand {
        "ask" => decision_ask(&args[1..]),
        "answer" => decision_answer(&args[1..]),
        "list" => decision_list(&args[1..]),
        "get" => decision_get(&args[1..]),
        "cancel" => decision_cancel(&args[1..]),
        "watch" => watch::run_watch(&args[1..]),
        "help" | "--help" | "-h" => {
            print_decision_help();
            Ok(0)
        }
        _ => {
            print_decision_help();
            Ok(2)
        }
    }
}

fn decision_ask(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{DECISION_ASK_USAGE}");
        return Ok(0);
    }

    let mut kind = None;
    let mut title = None;
    let mut body = None;
    let mut options = Vec::new();
    let mut allow_text = false;
    let mut timeout_ms = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--kind" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --kind");
                    return Ok(2);
                };
                match parse_decision_kind(value) {
                    Ok(parsed) => kind = Some(parsed),
                    Err(err) => {
                        eprintln!("{err}");
                        eprintln!("{DECISION_ASK_USAGE}");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --title");
                    return Ok(2);
                };
                title = Some(value.clone());
                index += 2;
            }
            "--body" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --body");
                    return Ok(2);
                };
                body = Some(value.clone());
                index += 2;
            }
            "--option" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --option");
                    return Ok(2);
                };
                match parse_decision_option(value) {
                    Ok(parsed) => options.push(parsed),
                    Err(err) => {
                        eprintln!("{err}");
                        eprintln!("{DECISION_ASK_USAGE}");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--allow-text" => {
                allow_text = true;
                index += 1;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                match super::parse_u64_flag("--timeout", value) {
                    Ok(seconds) => timeout_ms = Some(seconds.saturating_mul(1000)),
                    Err(err) => {
                        eprintln!("{err}");
                        eprintln!("{DECISION_ASK_USAGE}");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("{DECISION_ASK_USAGE}");
                return Ok(2);
            }
        }
    }

    let Some(kind) = kind else {
        eprintln!("missing required --kind");
        eprintln!("{DECISION_ASK_USAGE}");
        return Ok(2);
    };
    let Some(title) = title else {
        eprintln!("missing required --title");
        eprintln!("{DECISION_ASK_USAGE}");
        return Ok(2);
    };
    if options.is_empty() {
        eprintln!("missing required --option (at least one)");
        eprintln!("{DECISION_ASK_USAGE}");
        return Ok(2);
    }

    let origin = resolve_caller_pane_id().map(|pane_id| DecisionOrigin {
        pane_id: Some(pane_id),
        ..DecisionOrigin::default()
    });
    let response = super::send_request(&Request {
        id: "cli:decision:create".into(),
        method: Method::DecisionCreate(DecisionCreateParams {
            kind,
            title,
            body,
            options,
            allow_text,
            timeout_ms,
            origin,
        }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let Some(decision_id) = response["result"]["decision"]["decision_id"].as_str() else {
        return Err(std::io::Error::other(
            "decision.create response did not include a decision_id",
        ));
    };
    let decision_id = decision_id.to_string();

    // The server closes long-lived wait requests when it restarts or when the
    // socket drops, so retry the wait against the same decision_id until the
    // decision resolves or the caller interrupts.
    loop {
        let response = super::send_request(&Request {
            id: "cli:decision:wait".into(),
            method: Method::DecisionWait(DecisionWaitParams {
                decision_id: decision_id.clone(),
                timeout_ms: None,
            }),
        });
        let response = match response {
            Err(err) if super::protocol_mismatch_was_reported(&err) => return Err(err),
            Err(_) => {
                std::thread::sleep(DECISION_WAIT_RETRY_DELAY);
                continue;
            }
            Ok(response) => response,
        };
        if response.get("error").is_some() {
            return super::print_response(&response);
        }
        let Ok(decision) = serde_json::from_value::<crate::api::schema::Decision>(
            response["result"]["decision"].clone(),
        ) else {
            return Err(std::io::Error::other(
                "decision.wait response did not include a decision",
            ));
        };
        if decision.status == DecisionStatus::Pending {
            std::thread::sleep(DECISION_WAIT_RETRY_DELAY);
            continue;
        }
        super::print_response(&response)?;
        return Ok(match decision.status {
            DecisionStatus::Answered => 0,
            DecisionStatus::Expired => 3,
            DecisionStatus::Cancelled => 4,
            DecisionStatus::Pending => 1,
        });
    }
}

fn decision_answer(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{DECISION_ANSWER_USAGE}");
        return Ok(0);
    }
    let Some(decision_id) = args.first() else {
        eprintln!("{DECISION_ANSWER_USAGE}");
        return Ok(2);
    };
    let Some(option_id) = args.get(1) else {
        eprintln!("{DECISION_ANSWER_USAGE}");
        return Ok(2);
    };

    let mut text = None;
    let mut responder = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--text" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --text");
                    return Ok(2);
                };
                text = Some(value.clone());
                index += 2;
            }
            "--responder" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --responder");
                    return Ok(2);
                };
                responder = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("{DECISION_ANSWER_USAGE}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:decision:answer".into(),
        method: Method::DecisionAnswer(DecisionAnswerParams {
            decision_id: decision_id.clone(),
            option_id: Some(option_id.clone()),
            text,
            responder,
        }),
    })?)
}

fn decision_list(args: &[String]) -> std::io::Result<i32> {
    let mut status = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                let Some(parsed) = parse_decision_status(value) else {
                    eprintln!("invalid status: {value}");
                    eprintln!("{DECISION_LIST_USAGE}");
                    return Ok(2);
                };
                status = Some(parsed);
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{DECISION_LIST_USAGE}");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("{DECISION_LIST_USAGE}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:decision:list".into(),
        method: Method::DecisionList(DecisionListParams { status }),
    })?)
}

fn decision_get(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: herdr decision get <decision_id>";
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{usage}");
        return Ok(0);
    }
    if args.len() != 1 {
        eprintln!("{usage}");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:decision:get".into(),
        method: Method::DecisionGet(DecisionGetParams {
            decision_id: args[0].clone(),
        }),
    })?)
}

fn decision_cancel(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: herdr decision cancel <decision_id>";
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{usage}");
        return Ok(0);
    }
    if args.len() != 1 {
        eprintln!("{usage}");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:decision:cancel".into(),
        method: Method::DecisionCancel(DecisionCancelParams {
            decision_id: args[0].clone(),
        }),
    })?)
}

fn parse_decision_kind(value: &str) -> std::io::Result<DecisionKind> {
    match value {
        "command_guard" => Ok(DecisionKind::CommandGuard),
        "tool_permission" => Ok(DecisionKind::ToolPermission),
        "agent_prompt" => Ok(DecisionKind::AgentPrompt),
        "ask" => Ok(DecisionKind::Ask),
        _ => Err(std::io::Error::other(format!(
            "invalid decision kind: {value} (expected command_guard, tool_permission, agent_prompt, or ask)"
        ))),
    }
}

fn parse_decision_status(value: &str) -> Option<DecisionStatus> {
    match value {
        "pending" => Some(DecisionStatus::Pending),
        "answered" => Some(DecisionStatus::Answered),
        "expired" => Some(DecisionStatus::Expired),
        "cancelled" => Some(DecisionStatus::Cancelled),
        _ => None,
    }
}

fn parse_decision_option(value: &str) -> std::io::Result<DecisionOption> {
    let Some((id, rest)) = value.split_once(':') else {
        return Err(std::io::Error::other(format!(
            "invalid --option {value:?} (expected id:label[:approve|reject|other])"
        )));
    };
    let (label, role) = match rest.rsplit_once(':') {
        Some((label, "approve")) => (label, DecisionOptionRole::Approve),
        Some((label, "reject")) => (label, DecisionOptionRole::Reject),
        Some((label, "other")) => (label, DecisionOptionRole::Other),
        _ => (rest, DecisionOptionRole::Other),
    };
    if id.is_empty() || label.is_empty() {
        return Err(std::io::Error::other(format!(
            "invalid --option {value:?} (id and label must not be empty)"
        )));
    }
    Ok(DecisionOption {
        id: id.to_string(),
        label: label.to_string(),
        role,
    })
}

/// Resolve the caller pane exactly like `herdr pane current`: HERDR_PANE_ID
/// wins when set, and the server falls back to the calling process. Returns
/// None when the command runs outside a Herdr pane.
fn resolve_caller_pane_id() -> Option<String> {
    let caller_pane_id = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| super::normalize_pane_id(&value));
    let response = super::send_request(&Request {
        id: "cli:decision:ask:origin".into(),
        method: Method::PaneCurrent(PaneCurrentParams {
            caller_pane_id,
            caller_process_id: Some(std::process::id()),
        }),
    })
    .ok()?;
    response["result"]["pane"]["pane_id"]
        .as_str()
        .map(str::to_string)
}

fn print_decision_help() {
    eprintln!("herdr decision commands:");
    eprintln!("  {DECISION_ASK_USAGE}");
    eprintln!("  {DECISION_ANSWER_USAGE}");
    eprintln!("  {DECISION_LIST_USAGE}");
    eprintln!("  usage: herdr decision get <decision_id>");
    eprintln!("  usage: herdr decision cancel <decision_id>");
    eprintln!("  {}", watch::DECISION_WATCH_USAGE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_parser_splits_id_label_and_role() {
        let option = parse_decision_option("yes:Approve it:approve").unwrap();
        assert_eq!(option.id, "yes");
        assert_eq!(option.label, "Approve it");
        assert_eq!(option.role, DecisionOptionRole::Approve);

        let option = parse_decision_option("no:Reject it").unwrap();
        assert_eq!(option.role, DecisionOptionRole::Other);

        let option = parse_decision_option("id:label:with:colons").unwrap();
        assert_eq!(option.label, "label:with:colons");
        assert_eq!(option.role, DecisionOptionRole::Other);
    }

    #[test]
    fn option_parser_rejects_missing_parts() {
        assert!(parse_decision_option("nocolon").is_err());
        assert!(parse_decision_option(":label").is_err());
        assert!(parse_decision_option("id:").is_err());
    }
}
