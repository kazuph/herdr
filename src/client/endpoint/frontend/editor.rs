//! Editor processes and temporary scrollback files stay at the selected endpoint.
use super::super::{commands::EndpointCommandResult, ResourceKey};
use super::*;

pub(super) struct EditorRequest {
    owner: copy::Owner,
    request: String,
}

pub(super) fn open(frontend: &mut ClientFrontend, pane: ResourceKey) -> io::Result<()> {
    if frontend.editor_request.is_some()
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
    match frontend
        .runtime
        .issue_method_with_id(crate::api::schema::Method::PaneScrollbackEdit(
            crate::api::schema::PaneTarget {
                pane_id: owner.pane.clone(),
            },
        )) {
        Ok((request, update)) => {
            frontend.editor_request = Some(EditorRequest { owner, request });
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &EndpointCommandResult,
) -> io::Result<()> {
    let Some(request) = frontend.editor_request.as_ref() else {
        return Ok(());
    };
    if request.owner.endpoint != result.endpoint_id
        || request.owner.generation != result.generation
        || request.owner.boot != result.boot_id
        || request.request != result.request_id
    {
        return Ok(());
    }
    frontend.editor_request = None;
    if frontend.runtime.shell.active_endpoint_id == result.endpoint_id
        && result
            .result
            .as_ref()
            .is_ok_and(|value| value.get("pane").is_some())
    {
        let update = frontend
            .runtime
            .activate(result.endpoint_id.clone(), None, Instant::now());
        frontend.update(update)?;
    }
    Ok(())
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend
        .editor_request
        .as_ref()
        .is_some_and(|request| !request.owner.exists(frontend))
    {
        frontend.editor_request = None;
    }
}
