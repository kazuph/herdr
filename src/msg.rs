use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};

use crate::api::schema::MsgMessage;

pub(crate) const DEFAULT_ROOM: &str = "default";
pub(crate) const JOBS_ROOM: &str = "herdr-jobs";
pub(crate) const DEBOUNCE_SECONDS: i64 = 30;
#[cfg(test)]
pub(crate) const MSG_DB_PATH_ENV_VAR: &str = "HERDR_MSG_DB_PATH";

#[cfg(test)]
pub(crate) fn msg_db_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingNudge {
    pub room: String,
    pub to_agent: String,
    pub count: u32,
    pub latest_from: String,
}

pub(crate) struct MsgStore {
    conn: Connection,
}

impl MsgStore {
    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        #[cfg(test)]
        if let Ok(path) = std::env::var(MSG_DB_PATH_ENV_VAR) {
            return Self::open_at(PathBuf::from(path));
        }
        Self::open_at(crate::session::data_dir().join("msg.db"))
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
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS messages (
              id           INTEGER PRIMARY KEY AUTOINCREMENT,
              room         TEXT NOT NULL DEFAULT 'default',
              project      TEXT NOT NULL DEFAULT '',
              from_agent   TEXT NOT NULL,
              to_agent     TEXT NOT NULL,
              body         TEXT NOT NULL,
              created_at   TEXT NOT NULL,
              delivered_at TEXT,
              read_at      TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_unread ON messages(room, to_agent, read_at);
            CREATE INDEX IF NOT EXISTS idx_history ON messages(room, created_at);
            "#,
        )?;
        self.conn.execute(
            r#"
            UPDATE messages
            SET read_at = COALESCE(read_at, delivered_at)
            WHERE room = ?1
              AND read_at IS NULL
              AND delivered_at IS NOT NULL
            "#,
            params![JOBS_ROOM],
        )?;
        Ok(())
    }

    pub(crate) fn insert_messages(
        &mut self,
        room: &str,
        project: &str,
        from_agent: &str,
        recipients: &[String],
        body: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        let tx = self.conn.transaction()?;
        let mut messages = Vec::with_capacity(recipients.len());
        for to_agent in recipients {
            tx.execute(
                r#"
                INSERT INTO messages
                  (room, project, from_agent, to_agent, body, created_at)
                VALUES
                  (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                "#,
                params![room, project, from_agent, to_agent, body],
            )?;
            let id = tx.last_insert_rowid();
            let message = Self::message_by_id_in(&tx, id)?;
            messages.push(message);
        }
        tx.commit()?;
        Ok(messages)
    }

    pub(crate) fn unread_for_agent(
        &mut self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        let tx = self.conn.transaction()?;
        let messages = Self::select_messages(
            &tx,
            r#"
            SELECT id, room, project, from_agent, to_agent, body, created_at, delivered_at, read_at
            FROM messages
            WHERE room = ?1 AND to_agent = ?2 AND read_at IS NULL
            ORDER BY id ASC
            "#,
            params![room, to_agent],
        )?;
        if !messages.is_empty() {
            tx.execute(
                r#"
                UPDATE messages
                SET read_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                WHERE room = ?1 AND to_agent = ?2 AND read_at IS NULL
                "#,
                params![room, to_agent],
            )?;
        }
        tx.commit()?;
        Ok(messages)
    }

    pub(crate) fn pending_messages_for_agent(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        Self::select_messages(
            &self.conn,
            r#"
            SELECT id, room, project, from_agent, to_agent, body, created_at, delivered_at, read_at
            FROM messages
            WHERE room = ?1
              AND to_agent = ?2
              AND read_at IS NULL
              AND delivered_at IS NULL
            ORDER BY id ASC
            "#,
            params![room, to_agent],
        )
    }

    pub(crate) fn history(
        &self,
        room: &str,
        project: Option<&str>,
        limit: u32,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        let limit = i64::from(limit.max(1));
        match project {
            Some(project) => Self::select_messages(
                &self.conn,
                r#"
                SELECT id, room, project, from_agent, to_agent, body, created_at, delivered_at, read_at
                FROM messages
                WHERE room = ?1 AND project = ?2
                ORDER BY id DESC
                LIMIT ?3
                "#,
                params![room, project, limit],
            )
            .map(reverse_messages),
            None => Self::select_messages(
                &self.conn,
                r#"
                SELECT id, room, project, from_agent, to_agent, body, created_at, delivered_at, read_at
                FROM messages
                WHERE room = ?1
                ORDER BY id DESC
                LIMIT ?2
                "#,
                params![room, limit],
            )
            .map(reverse_messages),
        }
    }

    pub(crate) fn rooms(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT room
            FROM messages
            ORDER BY room ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    pub(crate) fn participants(&self, room: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT agent
            FROM (
              SELECT from_agent AS agent FROM messages WHERE room = ?1
              UNION
              SELECT to_agent AS agent FROM messages WHERE room = ?1
            )
            ORDER BY agent ASC
            "#,
        )?;
        let rows = stmt.query_map(params![room], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    pub(crate) fn pending_nudge_for(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Option<PendingNudge>> {
        if self.recent_delivered_unread_exists(room, to_agent)? {
            return Ok(None);
        }

        self.conn
            .query_row(
                r#"
                SELECT room, to_agent, COUNT(*), COALESCE(MAX(from_agent), '')
                FROM messages
                WHERE room = ?1
                  AND to_agent = ?2
                  AND read_at IS NULL
                  AND delivered_at IS NULL
                GROUP BY room, to_agent
                "#,
                params![room, to_agent],
                |row| {
                    let count: i64 = row.get(2)?;
                    Ok(PendingNudge {
                        room: row.get(0)?,
                        to_agent: row.get(1)?,
                        count: u32::try_from(count).unwrap_or(u32::MAX),
                        latest_from: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    pub(crate) fn pending_nudges_for_agent(
        &self,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<PendingNudge>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT room, to_agent, COUNT(*), COALESCE(MAX(from_agent), '')
            FROM messages
            WHERE to_agent = ?1
              AND read_at IS NULL
              AND delivered_at IS NULL
            GROUP BY room, to_agent
            ORDER BY room ASC
            "#,
        )?;
        let rows = stmt.query_map(params![to_agent], |row| {
            let room: String = row.get(0)?;
            let count: i64 = row.get(2)?;
            Ok((room, count, row.get::<_, String>(3)?))
        })?;

        let mut nudges = Vec::new();
        for row in rows {
            let (room, count, latest_from) = row?;
            if self.recent_delivered_unread_exists(&room, to_agent)? {
                continue;
            }
            nudges.push(PendingNudge {
                room,
                to_agent: to_agent.to_string(),
                count: u32::try_from(count).unwrap_or(u32::MAX),
                latest_from,
            });
        }
        Ok(nudges)
    }

    pub(crate) fn mark_delivered(&self, room: &str, to_agent: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            r#"
            UPDATE messages
            SET delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE room = ?1
              AND to_agent = ?2
              AND read_at IS NULL
              AND delivered_at IS NULL
            "#,
            params![room, to_agent],
        )
    }

    pub(crate) fn mark_read(&self, room: &str, to_agent: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            r#"
            UPDATE messages
            SET read_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE room = ?1
              AND to_agent = ?2
              AND read_at IS NULL
            "#,
            params![room, to_agent],
        )
    }

    fn recent_delivered_unread_exists(&self, room: &str, to_agent: &str) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM messages
                WHERE room = ?1
                  AND to_agent = ?2
                  AND read_at IS NULL
                  AND delivered_at IS NOT NULL
                  AND delivered_at >= strftime('%Y-%m-%dT%H:%M:%fZ','now', ?3)
                LIMIT 1
                "#,
                params![room, to_agent, format!("-{} seconds", DEBOUNCE_SECONDS)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    fn message_by_id_in(conn: &Connection, id: i64) -> rusqlite::Result<MsgMessage> {
        conn.query_row(
            r#"
            SELECT id, room, project, from_agent, to_agent, body, created_at, delivered_at, read_at
            FROM messages
            WHERE id = ?1
            "#,
            params![id],
            Self::message_from_row,
        )
    }

    fn select_messages<P>(
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> rusqlite::Result<Vec<MsgMessage>>
    where
        P: rusqlite::Params,
    {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, Self::message_from_row)?;
        rows.collect()
    }

    fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MsgMessage> {
        Ok(MsgMessage {
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
}

fn reverse_messages(mut messages: Vec<MsgMessage>) -> Vec<MsgMessage> {
    messages.reverse();
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(name: &str) -> MsgStore {
        let path = std::env::temp_dir()
            .join(format!("herdr-msg-test-{}-{name}", std::process::id()))
            .join("msg.db");
        let _ = std::fs::remove_file(&path);
        MsgStore::open_at(path).unwrap()
    }

    #[test]
    fn insert_unread_and_read_marks_messages_in_id_order() {
        let mut store = test_store("read");
        store
            .insert_messages(DEFAULT_ROOM, "/repo", "alice", &["bob".to_string()], "one")
            .unwrap();
        store
            .insert_messages(DEFAULT_ROOM, "/repo", "alice", &["bob".to_string()], "two")
            .unwrap();

        let unread = store.unread_for_agent(DEFAULT_ROOM, "bob").unwrap();
        assert_eq!(
            unread
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert!(unread.iter().all(|message| message.read_at.is_none()));

        let after = store.unread_for_agent(DEFAULT_ROOM, "bob").unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn history_filters_room_and_project() {
        let mut store = test_store("history");
        store
            .insert_messages("room-a", "/repo-a", "alice", &["bob".to_string()], "a")
            .unwrap();
        store
            .insert_messages("room-a", "/repo-b", "alice", &["bob".to_string()], "b")
            .unwrap();
        store
            .insert_messages("room-b", "/repo-a", "alice", &["bob".to_string()], "c")
            .unwrap();

        let messages = store.history("room-a", Some("/repo-b"), 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "b");
    }

    #[test]
    fn pending_nudge_debounces_recent_delivered_unread() {
        let mut store = test_store("debounce");
        store
            .insert_messages(DEFAULT_ROOM, "", "alice", &["bob".to_string()], "one")
            .unwrap();
        assert!(store
            .pending_nudge_for(DEFAULT_ROOM, "bob")
            .unwrap()
            .is_some());
        store.mark_delivered(DEFAULT_ROOM, "bob").unwrap();
        store
            .insert_messages(DEFAULT_ROOM, "", "alice", &["bob".to_string()], "two")
            .unwrap();
        assert!(store
            .pending_nudge_for(DEFAULT_ROOM, "bob")
            .unwrap()
            .is_none());
    }

    #[test]
    fn delivered_job_messages_are_marked_read_on_open() {
        let path = std::env::temp_dir()
            .join(format!("herdr-msg-test-{}-job-cleanup", std::process::id()))
            .join("msg.db");
        let _ = std::fs::remove_file(&path);
        let mut store = MsgStore::open_at(path.clone()).unwrap();
        store
            .insert_messages(JOBS_ROOM, "", "herdr-run", &["bob".to_string()], "done")
            .unwrap();
        store.mark_delivered(JOBS_ROOM, "bob").unwrap();
        drop(store);

        let mut reopened = MsgStore::open_at(path).unwrap();
        assert!(reopened
            .unread_for_agent(JOBS_ROOM, "bob")
            .unwrap()
            .is_empty());
        let history = reopened.history(JOBS_ROOM, None, 10).unwrap();
        assert!(history[0].read_at.is_some());
    }
}
