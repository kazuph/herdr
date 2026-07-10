use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
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
    conn: Connection,
}

impl JobStore {
    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        Self::open_at(crate::session::data_dir().join("jobs.db"))
    }

    fn open_at(path: PathBuf) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS jobs (
              id               TEXT PRIMARY KEY,
              label            TEXT NOT NULL,
              command          TEXT NOT NULL,
              cwd              TEXT NOT NULL,
              caller_pane      TEXT NOT NULL,
              caller_agent     TEXT NOT NULL,
              completion       TEXT NOT NULL,
              status           TEXT NOT NULL,
              runner_pid       INTEGER,
              exit_code        INTEGER,
              started_unix_ms  TEXT,
              finished_unix_ms TEXT,
              log_path         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_started ON jobs(started_unix_ms DESC, id DESC);
            "#,
        )?;
        Ok(store)
    }

    pub(crate) fn insert(&self, job: &JobRecord) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"INSERT INTO jobs
              (id, label, command, cwd, caller_pane, caller_agent, completion, status, log_path)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                job.id,
                job.label,
                job.command,
                job.cwd,
                job.caller_pane,
                job.caller_agent,
                job.completion,
                job.status,
                job.log_path,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn mark_running(
        &self,
        id: &str,
        runner_pid: u32,
        started_unix_ms: u128,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status='running', runner_pid=?2, started_unix_ms=?3 WHERE id=?1",
            params![id, runner_pid, started_unix_ms.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn mark_start_failed(
        &self,
        id: &str,
        exit_code: i32,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status='exited', exit_code=?2, finished_unix_ms=?3 WHERE id=?1 AND status='queued'",
            params![id, exit_code, finished_unix_ms.to_string()],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn mark_finished(
        &self,
        id: &str,
        exit_code: Option<i32>,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status='exited', exit_code=?2, finished_unix_ms=?3 WHERE id=?1 AND status='running'",
            params![id, exit_code, finished_unix_ms.to_string()],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn mark_cancelling(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status='cancelling' WHERE id=?1 AND status='running'",
            params![id],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn mark_cancelled(
        &self,
        id: &str,
        finished_unix_ms: u128,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status='cancelled', finished_unix_ms=?2 WHERE id=?1 AND status='cancelling'",
            params![id, finished_unix_ms.to_string()],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn get(&self, id: &str) -> rusqlite::Result<Option<JobRecord>> {
        self.conn
            .query_row(
                "SELECT id,label,command,cwd,caller_pane,caller_agent,completion,status,runner_pid,exit_code,started_unix_ms,finished_unix_ms,log_path FROM jobs WHERE id=?1",
                params![id],
                record_from_row,
            )
            .optional()
    }

    pub(crate) fn list(&self) -> rusqlite::Result<Vec<JobRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,label,command,cwd,caller_pane,caller_agent,completion,status,runner_pid,exit_code,started_unix_ms,finished_unix_ms,log_path FROM jobs ORDER BY COALESCE(started_unix_ms, '0') DESC, id DESC",
        )?;
        let jobs = stmt.query_map([], record_from_row)?.collect();
        jobs
    }
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    let started: Option<String> = row.get(10)?;
    let finished: Option<String> = row.get(11)?;
    Ok(JobRecord {
        id: row.get(0)?,
        label: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        caller_pane: row.get(4)?,
        caller_agent: row.get(5)?,
        completion: row.get(6)?,
        status: row.get(7)?,
        runner_pid: row.get(8)?,
        exit_code: row.get(9)?,
        started_unix_ms: started.and_then(|value| value.parse().ok()),
        finished_unix_ms: finished.and_then(|value| value.parse().ok()),
        log_path: row.get(12)?,
    })
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
            log_path: "/tmp/job.log".into(),
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
        drop(store);

        let reopened = JobStore::open_at(path).unwrap();
        let running = reopened.get("job-one").unwrap().unwrap();
        assert_eq!(running.status, "running");
        assert_eq!(running.runner_pid, Some(1234));
        assert_eq!(running.started_unix_ms, Some(100));
        assert!(reopened.mark_finished("job-one", Some(7), 200).unwrap());
        let exited = reopened.get("job-one").unwrap().unwrap();
        assert_eq!(exited.status, "exited");
        assert_eq!(exited.exit_code, Some(7));
        assert_eq!(exited.finished_unix_ms, Some(200));
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
        assert_eq!(cancelled.finished_unix_ms, Some(20));
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
