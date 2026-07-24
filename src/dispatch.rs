use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};

#[cfg(test)]
pub(crate) const HERDR_DB_PATH_ENV_VAR: &str = "HERDR_DB_PATH";
pub(crate) const SCHEMA_DDL: &str = r#"PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS actors (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  kind          TEXT NOT NULL CHECK (kind IN ('agent','shell','human')),
  name          TEXT NOT NULL,
  source        TEXT,
  model         TEXT,
  session_id    TEXT,
  pane_id       TEXT,
  first_seen_at TEXT NOT NULL,
  last_seen_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_actors_identity ON actors(kind, name);

CREATE TABLE IF NOT EXISTS dispatches (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  external_id   TEXT,
  kind          TEXT NOT NULL CHECK (kind IN ('message','command')),
  room          TEXT NOT NULL DEFAULT 'default',
  project       TEXT NOT NULL DEFAULT '',
  from_actor    INTEGER NOT NULL REFERENCES actors(id),
  to_actor      INTEGER NOT NULL REFERENCES actors(id),
  reply_to      INTEGER REFERENCES dispatches(id),
  expects_reply INTEGER NOT NULL DEFAULT 0,
  body          TEXT NOT NULL,
  label         TEXT,
  created_at    TEXT NOT NULL,
  delivered_at  TEXT,
  started_at    TEXT,
  finished_at   TEXT,
  exit_code     INTEGER,
  replied_at    TEXT,
  status        TEXT NOT NULL,
  completion    TEXT,
  runner_pid    INTEGER,
  log_path      TEXT
);
CREATE INDEX IF NOT EXISTS idx_dispatch_pending ON dispatches(to_actor, status);
CREATE INDEX IF NOT EXISTS idx_dispatch_room_time ON dispatches(room, created_at);
CREATE INDEX IF NOT EXISTS idx_dispatch_reply ON dispatches(reply_to);
CREATE UNIQUE INDEX IF NOT EXISTS idx_dispatch_external ON dispatches(kind, external_id);

CREATE VIEW IF NOT EXISTS v_reply_latency AS
SELECT d.id, d.room, d.project,
       fa.name AS asked_by,  fa.model AS asked_model,
       ta.name AS answered_by, ta.model AS answered_model,
       d.created_at, d.replied_at,
       CAST((julianday(d.replied_at) - julianday(d.created_at)) * 86400 AS INTEGER) AS reply_seconds
FROM dispatches d
JOIN actors fa ON fa.id = d.from_actor
JOIN actors ta ON ta.id = d.to_actor
WHERE d.kind = 'message' AND d.replied_at IS NOT NULL;

CREATE VIEW IF NOT EXISTS v_unanswered AS
SELECT d.id, d.room,
       fa.name AS asked_by, ta.name AS asked_to,
       d.body, d.created_at,
       CAST((julianday('now') - julianday(d.created_at)) * 86400 AS INTEGER) AS waiting_seconds
FROM dispatches d
JOIN actors fa ON fa.id = d.from_actor
JOIN actors ta ON ta.id = d.to_actor
WHERE d.expects_reply = 1 AND d.replied_at IS NULL;

CREATE VIEW IF NOT EXISTS v_command_stats AS
SELECT COALESCE(label, body) AS label, COUNT(*) AS runs,
       AVG(CASE WHEN exit_code = 0 THEN 1.0 ELSE 0.0 END) AS success_rate,
       CAST(AVG((julianday(finished_at) - julianday(started_at)) * 86400) AS INTEGER) AS avg_seconds,
       CAST(MAX((julianday(finished_at) - julianday(started_at)) * 86400) AS INTEGER) AS max_seconds
FROM dispatches
WHERE kind = 'command' AND finished_at IS NOT NULL
GROUP BY COALESCE(label, body);

CREATE VIEW IF NOT EXISTS v_model_matrix AS
SELECT fa.model AS asked_model, ta.model AS answered_model, COUNT(*) AS dispatches
FROM dispatches d
JOIN actors fa ON fa.id = d.from_actor
JOIN actors ta ON ta.id = d.to_actor
GROUP BY fa.model, ta.model;"#;

pub(crate) struct DispatchStore {
    conn: Connection,
}

impl DispatchStore {
    pub(crate) fn active_path() -> PathBuf {
        #[cfg(test)]
        if let Ok(path) = std::env::var(HERDR_DB_PATH_ENV_VAR) {
            return PathBuf::from(path);
        }
        crate::session::data_dir().join("herdr.db")
    }

    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        Self::open_at(Self::active_path())
    }

    pub(crate) fn open_at(path: PathBuf) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(SCHEMA_DDL)?;
        self.ensure_dispatch_column("external_id", "TEXT")?;
        self.ensure_dispatch_column("completion", "TEXT")?;
        self.ensure_dispatch_column("runner_pid", "INTEGER")?;
        self.conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_dispatch_external ON dispatches(kind, external_id)",
            [],
        )?;
        Ok(())
    }

    fn ensure_dispatch_column(&self, name: &str, definition: &str) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(dispatches)")?;
        let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for column in columns {
            if column? == name {
                return Ok(());
            }
        }
        self.conn.execute(
            &format!("ALTER TABLE dispatches ADD COLUMN {name} {definition}"),
            [],
        )?;
        Ok(())
    }

    pub(crate) fn schema() -> &'static str {
        SCHEMA_DDL
    }

    pub(crate) fn upsert_actor(
        &self,
        kind: &str,
        name: &str,
        source: Option<&str>,
        model: Option<&str>,
        session_id: Option<&str>,
        pane_id: Option<&str>,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO actors (kind, name, source, model, session_id, pane_id, first_seen_at, last_seen_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            ON CONFLICT(kind, name) DO UPDATE SET
              source = COALESCE(excluded.source, actors.source),
              model = COALESCE(excluded.model, actors.model),
              session_id = COALESCE(excluded.session_id, actors.session_id),
              pane_id = COALESCE(excluded.pane_id, actors.pane_id),
              last_seen_at = excluded.last_seen_at
            "#,
            params![kind, name, source, model, session_id, pane_id],
        )?;
        self.actor_id(kind, name)
    }

    fn actor_id(&self, kind: &str, name: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT id FROM actors WHERE kind=?1 AND name=?2",
            params![kind, name],
            |row| row.get(0),
        )
    }

    pub(crate) fn insert_message(
        &self,
        room: &str,
        project: &str,
        from: &str,
        to: &str,
        body: &str,
        reply_to: Option<i64>,
        expects_reply: bool,
    ) -> rusqlite::Result<MessageRecord> {
        let from_actor = self.upsert_actor("agent", from, None, None, None, None)?;
        let to_actor = self.upsert_actor("agent", to, None, None, None, None)?;
        self.conn.execute(
            r#"
            INSERT INTO dispatches
              (kind, room, project, from_actor, to_actor, reply_to, expects_reply, body, created_at, status)
            VALUES
              ('message', ?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'queued')
            "#,
            params![room, project, from_actor, to_actor, reply_to, i64::from(expects_reply), body],
        )?;
        let id = self.conn.last_insert_rowid();
        if let Some(reply_to) = reply_to {
            self.conn.execute(
                r#"
                UPDATE dispatches
                SET replied_at = COALESCE(replied_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    status = CASE WHEN kind='message' THEN 'replied' ELSE status END
                WHERE id = ?1 AND replied_at IS NULL
                "#,
                params![reply_to],
            )?;
        }
        self.message_by_id(id)
    }

    pub(crate) fn insert_command(
        &self,
        id: &str,
        label: &str,
        command: &str,
        cwd: &str,
        caller_pane: &str,
        caller_agent: &str,
        completion: &str,
        log_path: &str,
    ) -> rusqlite::Result<()> {
        let from_actor =
            self.upsert_actor("agent", caller_agent, None, None, None, Some(caller_pane))?;
        let to_actor = self.upsert_actor("shell", "herdr-run", None, None, None, None)?;
        self.conn.execute(
            r#"
            INSERT INTO dispatches
              (external_id, kind, room, project, from_actor, to_actor, body, label, created_at, status, completion, log_path)
            VALUES
              (?1, 'command', 'default', ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'queued', ?7, ?8)
            "#,
            params![id, cwd, from_actor, to_actor, command, label, completion, log_path],
        )?;
        Ok(())
    }

    pub(crate) fn messages_for_inbox(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MessageRecord>> {
        let messages = self.select_messages(
            r#"
            SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.kind='message'
              AND d.room=?1
              AND d.to_actor=(SELECT id FROM actors WHERE kind='agent' AND name=?2)
              AND d.status='queued'
            ORDER BY d.id ASC
            "#,
            params![room, to_agent],
        )?;
        if !messages.is_empty() {
            self.mark_messages_delivered(room, to_agent)?;
        }
        Ok(messages)
    }

    pub(crate) fn pending_messages(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MessageRecord>> {
        self.select_messages(
            r#"
            SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.kind='message'
              AND d.room=?1
              AND d.to_actor=(SELECT id FROM actors WHERE kind='agent' AND name=?2)
              AND d.status='queued'
            ORDER BY d.id ASC
            "#,
            params![room, to_agent],
        )
    }

    pub(crate) fn pending_messages_for_agent(
        &self,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MessageRecord>> {
        self.select_messages(
            r#"
            SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.kind='message'
              AND d.to_actor=(SELECT id FROM actors WHERE kind='agent' AND name=?1)
              AND d.status='queued'
            ORDER BY d.room ASC, d.id ASC
            "#,
            params![to_agent],
        )
    }

    pub(crate) fn history(
        &self,
        room: Option<&str>,
        project: Option<&str>,
        limit: u32,
    ) -> rusqlite::Result<Vec<MessageRecord>> {
        let limit = i64::from(limit.max(1));
        let (sql, values): (&str, Vec<String>) = match (room, project) {
            (Some(room), Some(project)) => (
                r#"
                SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
                FROM dispatches d
                JOIN actors fa ON fa.id=d.from_actor
                JOIN actors ta ON ta.id=d.to_actor
                WHERE d.kind='message' AND d.room=?1 AND d.project=?2
                ORDER BY d.id DESC LIMIT ?3
                "#,
                vec![room.to_string(), project.to_string(), limit.to_string()],
            ),
            (Some(room), None) => (
                r#"
                SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
                FROM dispatches d
                JOIN actors fa ON fa.id=d.from_actor
                JOIN actors ta ON ta.id=d.to_actor
                WHERE d.kind='message' AND d.room=?1
                ORDER BY d.id DESC LIMIT ?2
                "#,
                vec![room.to_string(), limit.to_string()],
            ),
            (None, _) => (
                r#"
                SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
                FROM dispatches d
                JOIN actors fa ON fa.id=d.from_actor
                JOIN actors ta ON ta.id=d.to_actor
                WHERE d.kind='message'
                ORDER BY d.id DESC LIMIT ?1
                "#,
                vec![limit.to_string()],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match values.len() {
            1 => stmt.query_map(
                params![values[0].parse::<i64>().unwrap_or(50)],
                message_from_row,
            )?,
            2 => stmt.query_map(
                params![values[0], values[1].parse::<i64>().unwrap_or(50)],
                message_from_row,
            )?,
            _ => stmt.query_map(
                params![values[0], values[1], values[2].parse::<i64>().unwrap_or(50)],
                message_from_row,
            )?,
        };
        let mut messages: Vec<_> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub(crate) fn rooms(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT room FROM dispatches ORDER BY room ASC")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    pub(crate) fn participants(&self, room: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT name FROM (
              SELECT fa.name AS name FROM dispatches d JOIN actors fa ON fa.id=d.from_actor WHERE d.room=?1
              UNION
              SELECT ta.name AS name FROM dispatches d JOIN actors ta ON ta.id=d.to_actor WHERE d.room=?1
            )
            ORDER BY name ASC
            "#,
        )?;
        let rows = stmt.query_map(params![room], |row| row.get(0))?;
        rows.collect()
    }

    pub(crate) fn mark_messages_delivered(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<usize> {
        self.conn.execute(
            r#"
            UPDATE dispatches
            SET delivered_at = COALESCE(delivered_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                status = 'delivered'
            WHERE id IN (
              SELECT d.id
              FROM dispatches d
              JOIN actors ta ON ta.id=d.to_actor
              WHERE d.kind='message' AND d.room=?1 AND ta.name=?2 AND d.status='queued'
            )
            "#,
            params![room, to_agent],
        )
    }

    pub(crate) fn command_rows(&self) -> rusqlite::Result<Vec<crate::job::JobRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT d.external_id, d.label, d.body, d.project, d.status, d.exit_code, d.started_at,
                   d.finished_at, d.log_path, fa.name, fa.pane_id, d.completion, d.runner_pid
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            WHERE d.kind='command'
            ORDER BY d.id DESC
            "#,
        )?;
        let rows = stmt.query_map([], job_from_row)?;
        rows.collect()
    }

    pub(crate) fn command_row(&self, id: &str) -> rusqlite::Result<Option<crate::job::JobRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT d.external_id, d.label, d.body, d.project, d.status, d.exit_code, d.started_at,
                       d.finished_at, d.log_path, fa.name, fa.pane_id, d.completion, d.runner_pid
                FROM dispatches d
                JOIN actors fa ON fa.id=d.from_actor
                WHERE d.kind='command' AND (d.external_id=?1 OR d.log_path LIKE '%' || ?1 || '.log')
                "#,
                params![id],
                job_from_row,
            )
            .optional()
    }

    pub(crate) fn command_dispatch_id(&self, id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT d.id FROM dispatches d WHERE d.kind='command' AND (d.external_id=?1 OR d.log_path LIKE '%' || ?1 || '.log')",
                params![id],
                |row| row.get(0),
            )
            .optional()
    }

    pub(crate) fn mark_command_running(&self, id: &str, runner_pid: u32) -> rusqlite::Result<()> {
        self.conn.execute(
            r#"
            UPDATE dispatches
            SET status='running', started_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), runner_pid=?2
            WHERE kind='command' AND (external_id=?1 OR log_path LIKE '%' || ?1 || '.log')
            "#,
            params![id, runner_pid],
        )?;
        Ok(())
    }

    pub(crate) fn mark_command_finished(
        &self,
        id: &str,
        exit_code: Option<i32>,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            r#"
            UPDATE dispatches
            SET status='exited', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), exit_code=?2
            WHERE kind='command' AND (external_id=?1 OR log_path LIKE '%' || ?1 || '.log') AND status='running'
            "#,
            params![id, exit_code],
        )?;
        Ok(changed == 1)
    }

    #[cfg(any(unix, test))]
    pub(crate) fn mark_command_cancelled(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            r#"
            UPDATE dispatches
            SET status='cancelled', finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE kind='command' AND (external_id=?1 OR log_path LIKE '%' || ?1 || '.log') AND status IN ('queued','running','cancelling')
            "#,
            params![id],
        )?;
        Ok(changed == 1)
    }

    #[cfg(any(unix, test))]
    pub(crate) fn mark_command_cancelling(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            r#"
            UPDATE dispatches
            SET status='cancelling'
            WHERE kind='command' AND (external_id=?1 OR log_path LIKE '%' || ?1 || '.log') AND status='running'
            "#,
            params![id],
        )?;
        Ok(changed == 1)
    }

    fn message_by_id(&self, id: i64) -> rusqlite::Result<MessageRecord> {
        self.conn.query_row(
            r#"
            SELECT d.id, d.room, d.project, fa.name, ta.name, d.body, d.created_at, d.delivered_at, d.delivered_at
            FROM dispatches d
            JOIN actors fa ON fa.id=d.from_actor
            JOIN actors ta ON ta.id=d.to_actor
            WHERE d.id=?1
            "#,
            params![id],
            message_from_row,
        )
    }

    fn select_messages<P>(&self, sql: &str, params: P) -> rusqlite::Result<Vec<MessageRecord>>
    where
        P: rusqlite::Params,
    {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, message_from_row)?;
        rows.collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageRecord {
    pub id: i64,
    pub room: String,
    pub project: String,
    pub from_agent: String,
    pub to_agent: String,
    pub body: String,
    pub created_at: String,
    pub delivered_at: Option<String>,
    pub read_at: Option<String>,
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRecord> {
    Ok(MessageRecord {
        id: row.get(0)?,
        room: row.get(1)?,
        project: row.get(2)?,
        from_agent: row.get(3)?,
        to_agent: row.get(4)?,
        body: row.get(5)?,
        created_at: row.get(6)?,
        delivered_at: row.get(7)?,
        read_at: row.get(8)?,
    })
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::job::JobRecord> {
    let log_path: String = row.get(8)?;
    let metadata = job_log_metadata(&log_path);
    let id = row.get::<_, Option<String>>(0)?.unwrap_or_else(|| {
        std::path::Path::new(&log_path)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string()
    });
    let started_at: Option<String> = row.get(6)?;
    let finished_at: Option<String> = row.get(7)?;
    Ok(crate::job::JobRecord {
        id,
        label: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        caller_pane: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
        caller_agent: row.get(9)?,
        completion: row
            .get::<_, Option<String>>(11)?
            .unwrap_or_else(|| "summary".into()),
        status: row.get(4)?,
        runner_pid: row.get::<_, Option<u32>>(12)?.or(metadata.runner_pid),
        exit_code: row.get(5)?,
        started_unix_ms: started_at.as_deref().and_then(isoish_to_epoch_hint),
        finished_unix_ms: finished_at.as_deref().and_then(isoish_to_epoch_hint),
        log_path,
    })
}

#[derive(Default)]
struct JobLogMetadata {
    runner_pid: Option<u32>,
}

fn job_log_metadata(log_path: &str) -> JobLogMetadata {
    let Ok(file) = std::fs::File::open(log_path) else {
        return JobLogMetadata::default();
    };
    let reader = std::io::BufReader::new(file);
    let mut metadata = JobLogMetadata::default();
    for line in std::io::BufRead::lines(reader)
        .map_while(Result::ok)
        .take(16)
    {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(pid) = line
            .strip_prefix("runner_pid: ")
            .and_then(|value| value.parse::<u32>().ok())
        {
            metadata.runner_pid = Some(pid);
        }
    }
    metadata
}

fn isoish_to_epoch_hint(value: &str) -> Option<u128> {
    (!value.is_empty()).then_some(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_to_command_sets_replied_at_without_overwriting_terminal_status() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-dispatch-reply-command-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = DispatchStore::open_at(dir.join("herdr.db")).unwrap();
        store
            .insert_command(
                "job-reply-command",
                "tests",
                "sleep 1",
                "/repo",
                "p_1",
                "alpha",
                "summary",
                "/tmp/job-reply-command.log",
            )
            .unwrap();
        store
            .mark_command_running("job-reply-command", 123)
            .unwrap();
        assert!(store
            .mark_command_finished("job-reply-command", Some(0))
            .unwrap());
        let command_id = store
            .command_dispatch_id("job-reply-command")
            .unwrap()
            .unwrap();
        store
            .insert_message(
                crate::msg::JOBS_ROOM,
                "/repo",
                "herdr-run",
                "alpha",
                "done",
                Some(command_id),
                false,
            )
            .unwrap();
        let command = store.command_row("job-reply-command").unwrap().unwrap();
        assert_eq!(command.status, "exited");
        let replied_at: Option<String> = store
            .conn
            .query_row(
                "SELECT replied_at FROM dispatches WHERE id=?1",
                params![command_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(replied_at.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
