use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobRecord {
    pub id: String,
    pub label: String,
    pub command: String,
    pub cwd: String,
    pub caller_pane: String,
    pub caller_agent: String,
    pub completion: String,
    pub status: String,
    pub runner_pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub started_unix_ms: Option<u128>,
    pub finished_unix_ms: Option<u128>,
    pub log_path: String,
}

pub(crate) struct JobStore {
    store: crate::dispatch::DispatchStore,
}

impl JobStore {
    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        Self::open_at(crate::dispatch::DispatchStore::active_path())
    }

    pub(crate) fn open_at(path: std::path::PathBuf) -> rusqlite::Result<Self> {
        Ok(Self {
            store: crate::dispatch::DispatchStore::open_at(path)?,
        })
    }

    pub(crate) fn insert(&self, job: &JobRecord) -> rusqlite::Result<()> {
        self.store.insert_command(
            &job.id,
            &job.label,
            &job.command,
            &job.cwd,
            &job.caller_pane,
            &job.caller_agent,
            &job.completion,
            &job.log_path,
        )
    }

    pub(crate) fn mark_running(
        &self,
        id: &str,
        runner_pid: u32,
        started_unix_ms: u128,
    ) -> rusqlite::Result<()> {
        let _ = started_unix_ms;
        self.store.mark_command_running(id, runner_pid)
    }

    pub(crate) fn mark_start_failed(
        &self,
        id: &str,
        exit_code: i32,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        self.store.mark_command_running(id, 0)?;
        let _ = finished_unix_ms;
        self.store.mark_command_finished(id, Some(exit_code))
    }

    pub(crate) fn mark_finished(
        &self,
        id: &str,
        exit_code: Option<i32>,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        let _ = finished_unix_ms;
        self.store.mark_command_finished(id, exit_code)
    }

    pub(crate) fn mark_cancelling(&self, id: &str) -> rusqlite::Result<bool> {
        self.store.mark_command_cancelling(id)
    }

    pub(crate) fn mark_cancelled(
        &self,
        id: &str,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        let _ = finished_unix_ms;
        self.store.mark_command_cancelled(id)
    }

    pub(crate) fn get(&self, id: &str) -> rusqlite::Result<Option<JobRecord>> {
        self.store.command_row(id)
    }

    pub(crate) fn list(&self) -> rusqlite::Result<Vec<JobRecord>> {
        self.store.command_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> JobRecord {
        JobRecord {
            id: id.into(),
            label: "tests".into(),
            command: "cargo test".into(),
            cwd: "/repo".into(),
            caller_pane: "p_1".into(),
            caller_agent: "codex-a".into(),
            completion: "summary".into(),
            status: "queued".into(),
            runner_pid: None,
            exit_code: None,
            started_unix_ms: None,
            finished_unix_ms: None,
            log_path: format!("/tmp/{id}.log"),
        }
    }

    fn nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }

    #[test]
    fn persists_job_lifecycle_across_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-job-store-{}-{}",
            std::process::id(),
            nonce()
        ));
        let path = dir.join("jobs.db");
        let store = JobStore::open_at(path.clone()).unwrap();
        store.insert(&record("job-one")).unwrap();
        store.mark_running("job-one", 1234, 100).unwrap();
        let _ = std::fs::remove_file("/tmp/job-one.log");
        std::fs::write("/tmp/job-one.log", "job_id: job-one\nrunner_pid: 1234\n\n").unwrap();
        drop(store);

        let reopened = JobStore::open_at(path).unwrap();
        let running = reopened.get("job-one").unwrap().unwrap();
        assert_eq!(running.status, "running");
        assert_eq!(running.runner_pid, Some(1234));
        assert_eq!(running.started_unix_ms, Some(1));
        assert!(reopened.mark_finished("job-one", Some(7), 200).unwrap());
        let exited = reopened.get("job-one").unwrap().unwrap();
        assert_eq!(exited.status, "exited");
        assert_eq!(exited.exit_code, Some(7));
        assert_eq!(exited.finished_unix_ms, Some(1));
        let _ = std::fs::remove_file("/tmp/job-one.log");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancelled_job_is_durable() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-job-cancel-{}-{}",
            std::process::id(),
            nonce()
        ));
        let store = JobStore::open_at(dir.join("jobs.db")).unwrap();
        store.insert(&record("job-cancel")).unwrap();
        store.mark_running("job-cancel", 99, 10).unwrap();
        assert!(store.mark_cancelling("job-cancel").unwrap());
        assert!(store.mark_cancelled("job-cancel", 20).unwrap());
        let cancelled = store.get("job-cancel").unwrap().unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.finished_unix_ms, Some(1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn terminal_transition_has_single_owner() {
        let dir =
            std::env::temp_dir().join(format!("herdr-job-race-{}-{}", std::process::id(), nonce()));
        let store = JobStore::open_at(dir.join("jobs.db")).unwrap();

        store.insert(&record("cancel-wins")).unwrap();
        store.mark_running("cancel-wins", 10, 1).unwrap();
        assert!(store.mark_cancelling("cancel-wins").unwrap());
        assert!(!store.mark_finished("cancel-wins", Some(0), 2).unwrap());
        assert!(store.mark_cancelled("cancel-wins", 3).unwrap());
        assert!(!store.mark_finished("cancel-wins", Some(0), 4).unwrap());

        store.insert(&record("finish-wins")).unwrap();
        store.mark_running("finish-wins", 11, 1).unwrap();
        assert!(store.mark_finished("finish-wins", Some(0), 2).unwrap());
        assert!(!store.mark_cancelling("finish-wins").unwrap());
        assert!(!store.mark_cancelled("finish-wins", 3).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }
}
