use std::path::PathBuf;

use crate::api::schema::MsgMessage;

pub(crate) const DEFAULT_ROOM: &str = "default";
pub(crate) const JOBS_ROOM: &str = "herdr-jobs";
#[cfg(test)]
pub(crate) const MSG_DB_PATH_ENV_VAR: &str = crate::dispatch::HERDR_DB_PATH_ENV_VAR;

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
    store: crate::dispatch::DispatchStore,
}

impl MsgStore {
    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        Self::open_at(crate::dispatch::DispatchStore::active_path())
    }

    pub(crate) fn open_at(path: PathBuf) -> rusqlite::Result<Self> {
        Ok(Self {
            store: crate::dispatch::DispatchStore::open_at(path)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn insert_messages(
        &mut self,
        room: &str,
        project: &str,
        from_agent: &str,
        recipients: &[String],
        body: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        let mut messages = Vec::with_capacity(recipients.len());
        for to_agent in recipients {
            messages.push(
                self.store
                    .insert_message(room, project, from_agent, to_agent, body, None, false)?
                    .into(),
            );
        }
        Ok(messages)
    }

    pub(crate) fn insert_message_with_reply(
        &mut self,
        room: &str,
        project: &str,
        from_agent: &str,
        to_agent: &str,
        body: &str,
        reply_to: Option<i64>,
    ) -> rusqlite::Result<MsgMessage> {
        self.store
            .insert_message(room, project, from_agent, to_agent, body, reply_to, false)
            .map(Into::into)
    }

    pub(crate) fn unread_for_recipients(
        &mut self,
        room: &str,
        recipients: &[String],
        excluded_ids: &std::collections::HashSet<i64>,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        self.store
            .messages_for_inbox_recipients(room, recipients, excluded_ids)
            .map(|messages| messages.into_iter().map(Into::into).collect())
    }

    pub(crate) fn pending_messages_for_agent(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        self.store
            .pending_messages(room, to_agent)
            .map(|messages| messages.into_iter().map(Into::into).collect())
    }

    pub(crate) fn pending_messages_for_agent_in_creation_order(
        &self,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        self.store
            .pending_messages_for_agent_in_creation_order(to_agent, JOBS_ROOM)
            .map(|messages| messages.into_iter().map(Into::into).collect())
    }

    pub(crate) fn history(
        &self,
        room: &str,
        project: Option<&str>,
        limit: u32,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        self.store
            .history(Some(room), project, limit)
            .map(|messages| messages.into_iter().map(Into::into).collect())
    }

    pub(crate) fn rooms(&self) -> rusqlite::Result<Vec<String>> {
        self.store.rooms()
    }

    pub(crate) fn pending_nudge_for(
        &self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Option<PendingNudge>> {
        let messages = self.store.pending_messages(room, to_agent)?;
        Ok(pending_nudge_from_messages(room, to_agent, &messages))
    }

    pub(crate) fn pending_nudges_for_agent(
        &self,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<PendingNudge>> {
        let messages = self.store.pending_messages_for_agent(to_agent)?;
        let mut nudges = Vec::new();
        let mut start = 0;
        while start < messages.len() {
            let room = &messages[start].room;
            let end = messages[start..]
                .iter()
                .position(|message| message.room != *room)
                .map_or(messages.len(), |offset| start + offset);
            if let Some(nudge) = pending_nudge_from_messages(room, to_agent, &messages[start..end])
            {
                nudges.push(nudge);
            }
            start = end;
        }
        Ok(nudges)
    }

    pub(crate) fn mark_delivered(&self, room: &str, to_agent: &str) -> rusqlite::Result<usize> {
        self.store.mark_messages_delivered(room, to_agent)
    }

    pub(crate) fn mark_message_delivered(&self, id: i64) -> rusqlite::Result<bool> {
        self.store.mark_message_delivered(id)
    }
}

impl From<crate::dispatch::MessageRecord> for MsgMessage {
    fn from(message: crate::dispatch::MessageRecord) -> Self {
        Self {
            id: message.id,
            room: message.room,
            project: message.project,
            from_agent: message.from_agent,
            to_agent: message.to_agent,
            body: message.body,
            created_at: message.created_at,
            delivered_at: message.delivered_at,
            read_at: message.read_at,
        }
    }
}

fn pending_nudge_from_messages(
    room: &str,
    to_agent: &str,
    messages: &[crate::dispatch::MessageRecord],
) -> Option<PendingNudge> {
    let latest_from = messages.last()?.from_agent.clone();
    Some(PendingNudge {
        room: room.to_string(),
        to_agent: to_agent.to_string(),
        count: u32::try_from(messages.len()).unwrap_or(u32::MAX),
        latest_from,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(name: &str) -> MsgStore {
        let path = std::env::temp_dir()
            .join(format!("herdr-msg-test-{}-{name}", std::process::id()))
            .join("herdr.db");
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

        let recipients = ["bob".to_string()];
        let unread = store
            .unread_for_recipients(DEFAULT_ROOM, &recipients, &std::collections::HashSet::new())
            .unwrap();
        assert_eq!(
            unread
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert!(unread.iter().all(|message| message.read_at.is_none()));

        let after = store
            .unread_for_recipients(DEFAULT_ROOM, &recipients, &std::collections::HashSet::new())
            .unwrap();
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
    fn delivered_messages_do_not_hide_new_pending_messages() {
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
            .is_some());
    }

    #[test]
    fn replying_to_a_read_message_preserves_its_read_at() {
        let mut store = test_store("reply-preserves-read-at");
        let original = store
            .insert_message_with_reply(DEFAULT_ROOM, "", "alice", "bob", "question", None)
            .unwrap();
        store.mark_delivered(DEFAULT_ROOM, "bob").unwrap();
        let read_at = store.history(DEFAULT_ROOM, None, 10).unwrap()[0]
            .read_at
            .clone();
        assert!(read_at.is_some());

        store
            .insert_message_with_reply(
                DEFAULT_ROOM,
                "",
                "bob",
                "alice",
                "answer",
                Some(original.id),
            )
            .unwrap();

        let original_after_reply = store
            .history(DEFAULT_ROOM, None, 10)
            .unwrap()
            .into_iter()
            .find(|message| message.id == original.id)
            .unwrap();
        assert_eq!(original_after_reply.read_at, read_at);
    }

    #[test]
    fn migration_backfills_read_at_for_existing_delivered_messages() {
        let path = std::env::temp_dir().join(format!(
            "herdr-msg-test-{}-read-at-migration.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let legacy_schema = crate::dispatch::SCHEMA_DDL.replace("  read_at       TEXT,\n", "");
        conn.execute_batch(&legacy_schema).unwrap();
        conn.execute(
            "INSERT INTO actors (id, kind, name, first_seen_at, last_seen_at) VALUES (1, 'agent', 'alice', '2026-08-08T00:00:00Z', '2026-08-08T00:00:00Z'), (2, 'agent', 'bob', '2026-08-08T00:00:00Z', '2026-08-08T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dispatches (id, kind, room, project, from_actor, to_actor, body, created_at, delivered_at, status) VALUES (1, 'message', 'default', '', 1, 2, 'old', '2026-08-08T00:00:00Z', '2026-08-08T01:00:00Z', 'delivered')",
            [],
        )
        .unwrap();
        drop(conn);

        let store = MsgStore::open_at(path.clone()).unwrap();
        let message = store.history(DEFAULT_ROOM, None, 10).unwrap().remove(0);

        assert_eq!(message.read_at, message.delivered_at);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reopening_current_schema_does_not_backfill_live_read_at() {
        let path = std::env::temp_dir()
            .join(format!(
                "herdr-msg-test-{}-read-at-migration-runs-once",
                std::process::id()
            ))
            .join("herdr.db");
        let _ = std::fs::remove_file(&path);
        let mut store = MsgStore::open_at(path.clone()).unwrap();
        let message = store
            .insert_message_with_reply(DEFAULT_ROOM, "", "alice", "bob", "old", None)
            .unwrap();
        store.mark_delivered(DEFAULT_ROOM, "bob").unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE dispatches SET status='replied', read_at=NULL WHERE id=?1",
            rusqlite::params![message.id],
        )
        .unwrap();
        drop(conn);

        let reopened = MsgStore::open_at(path.clone()).unwrap();
        let message = reopened.history(DEFAULT_ROOM, None, 10).unwrap().remove(0);

        assert!(message.delivered_at.is_some());
        assert!(message.read_at.is_none());
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pending_nudges_for_agent_groups_only_that_agents_queued_messages_by_room() {
        let mut store = test_store("pending-nudges-for-agent");
        store
            .insert_messages("room-b", "", "alice", &["bob".to_string()], "b1")
            .unwrap();
        store
            .insert_messages("room-a", "", "alice", &["bob".to_string()], "a1")
            .unwrap();
        store
            .insert_messages("room-b", "", "carol", &["bob".to_string()], "b2")
            .unwrap();
        store
            .insert_messages("room-a", "", "alice", &["carol".to_string()], "ignored")
            .unwrap();

        let nudges = store.pending_nudges_for_agent("bob").unwrap();

        assert_eq!(
            nudges,
            vec![
                PendingNudge {
                    room: "room-a".into(),
                    to_agent: "bob".into(),
                    count: 1,
                    latest_from: "alice".into(),
                },
                PendingNudge {
                    room: "room-b".into(),
                    to_agent: "bob".into(),
                    count: 2,
                    latest_from: "carol".into(),
                },
            ]
        );
    }

    #[test]
    fn delivered_job_messages_are_marked_read_on_open() {
        let path = std::env::temp_dir()
            .join(format!("herdr-msg-test-{}-job-cleanup", std::process::id()))
            .join("herdr.db");
        let _ = std::fs::remove_file(&path);
        let mut store = MsgStore::open_at(path.clone()).unwrap();
        store
            .insert_messages(JOBS_ROOM, "", "herdr-run", &["bob".to_string()], "done")
            .unwrap();
        store.mark_delivered(JOBS_ROOM, "bob").unwrap();
        drop(store);

        let mut reopened = MsgStore::open_at(path).unwrap();
        let recipients = ["bob".to_string()];
        assert!(reopened
            .unread_for_recipients(JOBS_ROOM, &recipients, &std::collections::HashSet::new())
            .unwrap()
            .is_empty());
        let history = reopened.history(JOBS_ROOM, None, 10).unwrap();
        assert!(history[0].read_at.is_some());
    }
}
