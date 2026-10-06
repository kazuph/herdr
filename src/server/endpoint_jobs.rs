use crate::app::AppState;
use crate::protocol::endpoint_jobs::{EndpointJob, EndpointJobsProjection};

pub(crate) fn projection(state: &AppState, boot_id: &str, revision: u64) -> EndpointJobsProjection {
    EndpointJobsProjection {
        boot_id: boot_id.into(),
        revision,
        jobs: state
            .jobs
            .iter()
            .map(|job| {
                let workspace = state
                    .parse_pane_id(&job.caller_pane)
                    .map(|(index, _)| state.workspaces[index].id.clone());
                EndpointJob::from_record(job, workspace)
            })
            .collect(),
    }
}
