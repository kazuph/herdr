//! Custom bindings run on their captured endpoint using the fork's original launch paths.
use super::super::{commands::EndpointCommandResult, ResourceKey};
use super::*;
use crate::api::schema as api;

pub(super) struct CommandRequest {
    owner: copy::Owner,
    request: String,
}

pub(super) fn indexed(action: crate::app::NavigateAction) -> bool {
    matches!(
        action,
        crate::app::NavigateAction::SwitchWorkspace(_)
            | crate::app::NavigateAction::SwitchTab(_)
            | crate::app::NavigateAction::FocusAgent(_)
    )
}

pub(super) fn key(
    frontend: &mut ClientFrontend,
    key: crate::input::TerminalKey,
    prefixed: bool,
) -> io::Result<bool> {
    let dispatch = if prefixed {
        crate::app::BindingDispatch::Prefix
    } else {
        crate::app::BindingDispatch::Direct
    };
    let Some(binding) =
        crate::app::custom_command_for_bindings(&frontend.keybinds.keybinds, key, dispatch)
    else {
        return Ok(false);
    };
    if copy::before_custom(frontend, key, prefixed)? {
        return Ok(true);
    }
    if let Some(pane) = input::focused_pane(frontend) {
        execute(frontend, pane, binding)?;
    }
    Ok(true)
}

fn execute(
    frontend: &mut ClientFrontend,
    pane: ResourceKey,
    binding: crate::config::CustomCommandKeybind,
) -> io::Result<()> {
    if frontend.custom_request.is_some()
        || !frontend.runtime.input_lease_current()
        || frontend.runtime.shell.active_endpoint_id != pane.endpoint
    {
        return Ok(());
    }
    let Some(endpoint) = frontend.runtime.shell.endpoint(&pane.endpoint) else {
        return Ok(());
    };
    let Some(generation) = endpoint.generation else {
        return Ok(());
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(());
    };
    let owner = copy::Owner {
        endpoint: pane.endpoint,
        generation,
        boot: snapshot.boot_id.clone(),
        pane: pane.id,
    };
    if owner.pane(frontend).is_none() {
        return Ok(());
    }
    use crate::config::{CommandKeybindType, CustomCommandAction};
    let action = match binding.action {
        CustomCommandAction::Shell => CommandKeybindType::Shell,
        CustomCommandAction::Pane => CommandKeybindType::Pane,
        CustomCommandAction::Popup => CommandKeybindType::Popup,
        CustomCommandAction::PluginAction => CommandKeybindType::PluginAction,
    };
    match frontend
        .runtime
        .issue_method_with_id(api::Method::PaneCommandExecute(
            api::PaneCommandExecuteParams {
                pane_id: owner.pane.clone(),
                command: binding.command,
                action,
                width: binding.width,
                height: binding.height,
            },
        )) {
        Ok((request, update)) => {
            frontend.custom_request = Some(CommandRequest { owner, request });
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &EndpointCommandResult,
) -> io::Result<bool> {
    let Some(request) = frontend.custom_request.as_ref() else {
        return Ok(false);
    };
    if request.owner.endpoint != result.endpoint_id
        || request.owner.generation != result.generation
        || request.owner.boot != result.boot_id
        || request.request != result.request_id
    {
        return Ok(false);
    }
    frontend.custom_request = None;
    match &result.result {
        Ok(value)
            if value.get("type").and_then(|v| v.as_str()) == Some("command_executed")
                && frontend.runtime.shell.active_endpoint_id == result.endpoint_id =>
        {
            let update =
                frontend
                    .runtime
                    .activate(result.endpoint_id.clone(), None, Instant::now());
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(format!("custom command failed: {}", error.message)),
        _ => {}
    }
    Ok(true)
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend
        .custom_request
        .as_ref()
        .is_some_and(|request| !request.owner.exists(frontend))
    {
        frontend.custom_request = None;
    }
}
