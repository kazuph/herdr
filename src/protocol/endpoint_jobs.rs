//! Optional fork job facts. This JSON control never changes generation-1 bincode types.

use serde::{Deserialize, Serialize};

pub(crate) const JOBS_PROJECTION_CAPABILITY: &str = "kazuph.job_projection.v1";
pub(crate) const JOBS_PROJECTION_KIND: &str = "kazuph.jobs.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EndpointJobsProjection {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    pub(crate) jobs: Vec<EndpointJob>,
}

/// Display facts belong to this endpoint, including the unmodified caller ID.
/// Paths are descriptive remote strings, never local file-open instructions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EndpointJob {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) command: String,
    pub(crate) cwd: String,
    pub(crate) caller_pane: String,
    pub(crate) caller_agent: String,
    pub(crate) completion: String,
    pub(crate) status: String,
    pub(crate) runner_pid: Option<u32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) started_unix_ms: Option<u128>,
    pub(crate) finished_unix_ms: Option<u128>,
    pub(crate) log_path: String,
    /// The owning server resolves this with its existing caller resolver.
    pub(crate) workspace_id: Option<String>,
    /// Server-verified liveness of `runner_pid` for `running`/`cancelling`
    /// jobs. `None` when the server did not verify (finished or queued job, no
    /// runner pid, or a server that predates this fact). Runner pids live in
    /// the server's namespace, so only the server can report this.
    #[serde(default)]
    pub(crate) runner_alive: Option<bool>,
}

impl EndpointJob {
    pub(crate) fn from_record(
        job: &crate::job::JobRecord,
        workspace_id: Option<String>,
        runner_alive: Option<bool>,
    ) -> Self {
        Self {
            id: job.id.clone(),
            label: job.label.clone(),
            command: job.command.clone(),
            cwd: job.cwd.clone(),
            caller_pane: job.caller_pane.clone(),
            caller_agent: job.caller_agent.clone(),
            completion: job.completion.clone(),
            status: job.status.clone(),
            runner_pid: job.runner_pid,
            exit_code: job.exit_code,
            started_unix_ms: job.started_unix_ms,
            finished_unix_ms: job.finished_unix_ms,
            log_path: job.log_path.clone(),
            workspace_id,
            runner_alive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_jobs_json_preserves_unknown_status_and_opaque_owner() {
        let projection = EndpointJobsProjection {
            boot_id: "server boot".into(),
            revision: 7,
            jobs: vec![EndpointJob {
                id: "job-opaque".into(),
                label: "日本語 👩🏽‍💻".into(),
                command: "printf '%s' \"$HOME\"".into(),
                cwd: "/remote project".into(),
                caller_pane: "opaque:pane".into(),
                caller_agent: "codex".into(),
                completion: "summary".into(),
                status: "future-state".into(),
                runner_pid: Some(123),
                exit_code: None,
                started_unix_ms: Some(1),
                finished_unix_ms: None,
                log_path: "/remote log".into(),
                workspace_id: Some("opaque:workspace".into()),
                runner_alive: Some(true),
            }],
        };
        let json = serde_json::to_string(&projection).unwrap();
        let decoded: EndpointJobsProjection = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, projection);
        assert_eq!(decoded.jobs[0].caller_pane, "opaque:pane");
    }
}
