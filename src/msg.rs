use std::path::PathBuf;

use crate::api::schema::MsgMessage;

pub(crate) const DEFAULT_ROOM: &str = "default";
pub(crate) const JOBS_ROOM: &str = "herdr-jobs";
pub(crate) const FALLBACK_MAX_MESSAGES: usize = 5;
pub(crate) const FALLBACK_MAX_BYTES: usize = 4 * 1024;
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

    pub(crate) fn unread_for_agent(
        &mut self,
        room: &str,
        to_agent: &str,
    ) -> rusqlite::Result<Vec<MsgMessage>> {
        self.store
            .messages_for_inbox(room, to_agent)
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

    pub(crate) fn participants(&self, room: &str) -> rusqlite::Result<Vec<String>> {
        self.store.participants(room)
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
        assert!(reopened
            .unread_for_agent(JOBS_ROOM, "bob")
            .unwrap()
            .is_empty());
        let history = reopened.history(JOBS_ROOM, None, 10).unwrap();
        assert!(history[0].read_at.is_some());
    }
}
