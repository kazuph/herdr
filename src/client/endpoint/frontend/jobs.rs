//! Log paths remain owned by the server; this viewer only issues the original two-click action.
use super::super::{commands::EndpointCommandResult, ResourceKey};
use super::*;

pub(super) struct JobRequest {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    request: String,
}

pub(super) fn activate(frontend: &mut ClientFrontend, key: &ResourceKey) -> io::Result<()> {
    if frontend.job_request.is_some()
        || !frontend.runtime.input_lease_current()
        || frontend.runtime.shell.active_endpoint_id != key.endpoint
    {
        return Ok(());
    }
    let Some(endpoint) = frontend.runtime.shell.endpoint(&key.endpoint) else {
        return Ok(());
    };
    let Some(generation) = endpoint.generation else {
        return Ok(());
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(());
    };
    if !endpoint
        .jobs
        .for_snapshot(snapshot)
        .is_some_and(|jobs| jobs.jobs.iter().any(|job| job.id == key.id))
    {
        return Ok(());
    }
    let boot = snapshot.boot_id.clone();
    match frontend
        .runtime
        .issue_method_with_id(crate::api::schema::Method::RunLogOpen(
            crate::api::schema::RunLogOpenParams {
                job_id: key.id.clone(),
            },
        )) {
        Ok((request, update)) => {
            frontend.job_request = Some(JobRequest {
                endpoint: key.endpoint.clone(),
                generation,
                boot,
                request,
            });
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
    let Some(request) = frontend.job_request.as_ref() else {
        return Ok(());
    };
    if request.endpoint != result.endpoint_id
        || request.generation != result.generation
        || request.boot != result.boot_id
        || request.request != result.request_id
    {
        return Ok(());
    }
    frontend.job_request = None;
    if frontend.runtime.shell.active_endpoint_id != result.endpoint_id {
        return Ok(());
    }
    if result.result.as_ref().is_ok_and(|value| {
        value
            .get("caller_pane")
            .is_some_and(|value| value.is_string())
    }) {
        let update = frontend
            .runtime
            .activate(result.endpoint_id.clone(), None, Instant::now());
        frontend.update(update)?;
    }
    Ok(())
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend.job_request.as_ref().is_some_and(|request| {
        !frontend
            .runtime
            .endpoints
            .accepts(&request.endpoint, request.generation)
            || frontend
                .runtime
                .shell
                .endpoint(&request.endpoint)
                .and_then(|endpoint| endpoint.cache.live_snapshot(request.generation))
                .is_none_or(|snapshot| snapshot.boot_id != request.boot)
    }) {
        frontend.job_request = None;
    }
}
