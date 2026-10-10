use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;

use crate::api::schema::decisions::{Decision, DecisionAnswer, DecisionStatus};
use crate::dispatch::DispatchStore;

static NEXT_DECISION_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub(crate) fn new_decision_id() -> String {
    let sequence = NEXT_DECISION_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("dec-{}-{}-{sequence}", unix_ms_now(), std::process::id())
}

#[derive(Debug)]
pub(crate) enum DecisionStoreError {
    Sqlite(rusqlite::Error),
    Codec(serde_json::Error),
}

impl std::fmt::Display for DecisionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "{err}"),
            Self::Codec(err) => write!(f, "{err}"),
        }
    }
}

impl From<rusqlite::Error> for DecisionStoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<serde_json::Error> for DecisionStoreError {
    fn from(err: serde_json::Error) -> Self {
        Self::Codec(err)
    }
}

pub(crate) type DecisionStoreResult<T> = Result<T, DecisionStoreError>;

#[derive(Debug)]
pub(crate) enum AnswerOutcome {
    Answered(Box<Decision>),
    NotFound,
    AlreadyResolved,
    UnknownOption,
    TextNotAllowed,
    MissingAnswer,
}

#[derive(Debug)]
pub(crate) enum ResolveOutcome {
    Resolved(Box<Decision>),
    NotFound,
    AlreadyResolved,
}

pub(crate) struct DecisionStore {
    store: DispatchStore,
}

impl DecisionStore {
    pub(crate) fn open_active() -> rusqlite::Result<Self> {
        Self::open_at(DispatchStore::active_path())
    }

    pub(crate) fn open_at(path: std::path::PathBuf) -> rusqlite::Result<Self> {
        Ok(Self {
            store: DispatchStore::open_at(path)?,
        })
    }

    pub(crate) fn insert(&mut self, decision: &Decision) -> DecisionStoreResult<()> {
        let record = serde_json::to_string(decision)?;
        self.store.conn_mut().execute(
            "INSERT INTO decisions (decision_id, status, expires_unix_ms, created_unix_ms, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                decision.decision_id,
                status_name(decision.status),
                decision.expires_unix_ms.map(|value| value as i64),
                decision.created_unix_ms as i64,
                record,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn get(&self, decision_id: &str) -> DecisionStoreResult<Option<Decision>> {
        let mut stmt = self
            .store
            .conn()
            .prepare("SELECT record_json FROM decisions WHERE decision_id = ?1")?;
        let record = stmt
            .query_row([decision_id], |row| row.get::<_, String>(0))
            .optional()?;
        record
            .map(|json| serde_json::from_str(&json).map_err(DecisionStoreError::from))
            .transpose()
    }

    pub(crate) fn list(
        &self,
        status: Option<DecisionStatus>,
    ) -> DecisionStoreResult<Vec<Decision>> {
        let mut stmt = self.store.conn().prepare(
            "SELECT record_json FROM decisions
             WHERE (?1 IS NULL OR status = ?1)
             ORDER BY created_unix_ms, decision_id",
        )?;
        let status_filter = status.map(status_name);
        let rows = stmt.query_map([status_filter], |row| row.get::<_, String>(0))?;
        let mut decisions = Vec::new();
        for row in rows {
            decisions.push(serde_json::from_str(&row?)?);
        }
        Ok(decisions)
    }

    /// Atomically resolve a pending decision as answered. The UPDATE is
    /// conditional on `status = 'pending'`, so the first answer wins even if
    /// two requests race.
    pub(crate) fn answer(
        &mut self,
        decision_id: &str,
        option_id: Option<String>,
        text: Option<String>,
        responder: Option<String>,
        now_unix_ms: u64,
    ) -> DecisionStoreResult<AnswerOutcome> {
        let tx = self.store.conn_mut().unchecked_transaction()?;
        let Some(mut decision) = load_decision(&tx, decision_id)? else {
            tx.commit()?;
            return Ok(AnswerOutcome::NotFound);
        };
        if decision.status != DecisionStatus::Pending {
            tx.commit()?;
            return Ok(AnswerOutcome::AlreadyResolved);
        }
        let option_id = option_id.filter(|value| !value.is_empty());
        let text = text.filter(|value| !value.is_empty());
        if option_id.is_none() && text.is_none() {
            tx.commit()?;
            return Ok(AnswerOutcome::MissingAnswer);
        }
        if let Some(option) = &option_id {
            if !decision
                .options
                .iter()
                .any(|candidate| &candidate.id == option)
            {
                tx.commit()?;
                return Ok(AnswerOutcome::UnknownOption);
            }
        }
        if text.is_some() && !decision.allow_text {
            tx.commit()?;
            return Ok(AnswerOutcome::TextNotAllowed);
        }
        decision.status = DecisionStatus::Answered;
        decision.answer = Some(DecisionAnswer {
            option_id,
            text,
            responder,
            answered_unix_ms: now_unix_ms,
        });
        if !update_if_pending(&tx, &decision)? {
            tx.commit()?;
            return Ok(AnswerOutcome::AlreadyResolved);
        }
        tx.commit()?;
        Ok(AnswerOutcome::Answered(Box::new(decision)))
    }

    pub(crate) fn cancel(&mut self, decision_id: &str) -> DecisionStoreResult<ResolveOutcome> {
        let tx = self.store.conn_mut().unchecked_transaction()?;
        let Some(mut decision) = load_decision(&tx, decision_id)? else {
            tx.commit()?;
            return Ok(ResolveOutcome::NotFound);
        };
        if decision.status != DecisionStatus::Pending {
            tx.commit()?;
            return Ok(ResolveOutcome::AlreadyResolved);
        }
        decision.status = DecisionStatus::Cancelled;
        if !update_if_pending(&tx, &decision)? {
            tx.commit()?;
            return Ok(ResolveOutcome::AlreadyResolved);
        }
        tx.commit()?;
        Ok(ResolveOutcome::Resolved(Box::new(decision)))
    }

    /// Mark every pending decision whose deadline passed as expired and return
    /// them so callers can emit `decision.resolved`.
    pub(crate) fn expire_pending(
        &mut self,
        now_unix_ms: u64,
    ) -> DecisionStoreResult<Vec<Decision>> {
        let tx = self.store.conn_mut().unchecked_transaction()?;
        let due: Vec<Decision> = {
            let mut stmt = tx.prepare(
                "SELECT record_json FROM decisions
                 WHERE status = 'pending' AND expires_unix_ms IS NOT NULL AND expires_unix_ms <= ?1
                 ORDER BY expires_unix_ms, decision_id",
            )?;
            let rows = stmt.query_map([now_unix_ms as i64], |row| row.get::<_, String>(0))?;
            let mut due = Vec::new();
            for row in rows {
                due.push(serde_json::from_str::<Decision>(&row?)?);
            }
            due
        };
        let mut expired = Vec::new();
        for mut decision in due {
            decision.status = DecisionStatus::Expired;
            if update_if_pending(&tx, &decision)? {
                expired.push(decision);
            }
        }
        tx.commit()?;
        Ok(expired)
    }

    /// Earliest expiry deadline among pending decisions; used to schedule the
    /// next expiry tick.
    pub(crate) fn next_pending_expiry_unix_ms(&self) -> DecisionStoreResult<Option<u64>> {
        let value = self.store.conn().query_row(
            "SELECT MIN(expires_unix_ms) FROM decisions
             WHERE status = 'pending' AND expires_unix_ms IS NOT NULL",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(value.map(|ms| ms.max(0) as u64))
    }
}

fn status_name(status: DecisionStatus) -> &'static str {
    match status {
        DecisionStatus::Pending => "pending",
        DecisionStatus::Answered => "answered",
        DecisionStatus::Expired => "expired",
        DecisionStatus::Cancelled => "cancelled",
    }
}

fn load_decision(
    conn: &rusqlite::Connection,
    decision_id: &str,
) -> DecisionStoreResult<Option<Decision>> {
    let mut stmt = conn.prepare("SELECT record_json FROM decisions WHERE decision_id = ?1")?;
    let record = stmt
        .query_row([decision_id], |row| row.get::<_, String>(0))
        .optional()?;
    record
        .map(|json| serde_json::from_str(&json).map_err(DecisionStoreError::from))
        .transpose()
}

fn update_if_pending(
    conn: &rusqlite::Connection,
    decision: &Decision,
) -> DecisionStoreResult<bool> {
    let record = serde_json::to_string(decision)?;
    let changed = conn.execute(
        "UPDATE decisions SET status = ?2, record_json = ?3
         WHERE decision_id = ?1 AND status = 'pending'",
        rusqlite::params![decision.decision_id, status_name(decision.status), record],
    )?;
    Ok(changed == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::decisions::{
        DecisionKind, DecisionOption, DecisionOptionRole, DecisionOrigin,
    };

    fn temp_store(tag: &str) -> (DecisionStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("herdr-decision-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (DecisionStore::open_at(dir.join("herdr.db")).unwrap(), dir)
    }

    fn pending_decision(decision_id: &str, expires_unix_ms: Option<u64>) -> Decision {
        Decision {
            decision_id: decision_id.into(),
            kind: DecisionKind::Ask,
            title: "continue?".into(),
            body: Some("body".into()),
            options: vec![
                DecisionOption {
                    id: "yes".into(),
                    label: "Yes".into(),
                    role: DecisionOptionRole::Approve,
                },
                DecisionOption {
                    id: "no".into(),
                    label: "No".into(),
                    role: DecisionOptionRole::Reject,
                },
            ],
            allow_text: false,
            origin: Some(DecisionOrigin {
                pane_id: Some("p_1".into()),
                ..DecisionOrigin::default()
            }),
            created_unix_ms: 1_000,
            expires_unix_ms,
            status: DecisionStatus::Pending,
            answer: None,
        }
    }

    #[test]
    fn decision_ids_have_prefix_and_are_unique() {
        let first = new_decision_id();
        let second = new_decision_id();
        assert!(first.starts_with("dec-"));
        assert!(second.starts_with("dec-"));
        assert_ne!(first, second);
    }

    #[test]
    fn insert_get_and_list_round_trip_the_full_record() {
        let (mut store, dir) = temp_store("roundtrip");
        let decision = pending_decision("dec-rt-1", Some(5_000));
        store.insert(&decision).unwrap();

        let loaded = store.get("dec-rt-1").unwrap().unwrap();
        assert_eq!(loaded, decision);
        assert_eq!(store.list(None).unwrap(), vec![decision.clone()]);
        assert_eq!(
            store.list(Some(DecisionStatus::Pending)).unwrap(),
            vec![decision]
        );
        assert!(store
            .list(Some(DecisionStatus::Answered))
            .unwrap()
            .is_empty());
        assert!(store.get("dec-missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn records_survive_reopening_the_database() {
        let (mut store, dir) = temp_store("reopen");
        store.insert(&pending_decision("dec-p-1", None)).unwrap();
        store
            .insert(&pending_decision("dec-p-2", Some(9_000)))
            .unwrap();
        drop(store);

        let reopened = DecisionStore::open_at(dir.join("herdr.db")).unwrap();
        assert_eq!(reopened.list(None).unwrap().len(), 2);
        assert_eq!(
            reopened.get("dec-p-2").unwrap().unwrap().expires_unix_ms,
            Some(9_000)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn first_answer_wins_and_second_answer_is_already_resolved() {
        let (mut store, dir) = temp_store("firstwins");
        store.insert(&pending_decision("dec-fw-1", None)).unwrap();

        let outcome = store
            .answer(
                "dec-fw-1",
                Some("yes".into()),
                None,
                Some("cli".into()),
                2_000,
            )
            .unwrap();
        let AnswerOutcome::Answered(decision) = outcome else {
            panic!("expected answered outcome");
        };
        assert_eq!(decision.status, DecisionStatus::Answered);
        let answer = decision.answer.unwrap();
        assert_eq!(answer.option_id.as_deref(), Some("yes"));
        assert_eq!(answer.responder.as_deref(), Some("cli"));
        assert_eq!(answer.answered_unix_ms, 2_000);

        let second = store
            .answer("dec-fw-1", Some("no".into()), None, None, 3_000)
            .unwrap();
        assert!(matches!(second, AnswerOutcome::AlreadyResolved));
        // The stored record still carries the first answer.
        let stored = store.get("dec-fw-1").unwrap().unwrap();
        assert_eq!(stored.answer.unwrap().option_id.as_deref(), Some("yes"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn answer_rejects_unknown_option_text_and_missing_answer() {
        let (mut store, dir) = temp_store("reject");
        store.insert(&pending_decision("dec-rj-1", None)).unwrap();
        assert!(matches!(
            store
                .answer("dec-rj-1", Some("maybe".into()), None, None, 1)
                .unwrap(),
            AnswerOutcome::UnknownOption
        ));
        assert!(matches!(
            store
                .answer("dec-rj-1", Some("yes".into()), Some("note".into()), None, 1)
                .unwrap(),
            AnswerOutcome::TextNotAllowed
        ));
        assert!(matches!(
            store.answer("dec-rj-1", None, None, None, 1).unwrap(),
            AnswerOutcome::MissingAnswer
        ));
        assert!(matches!(
            store
                .answer("dec-absent", Some("yes".into()), None, None, 1)
                .unwrap(),
            AnswerOutcome::NotFound
        ));
        // None of the rejections resolved the decision.
        assert_eq!(
            store.get("dec-rj-1").unwrap().unwrap().status,
            DecisionStatus::Pending
        );

        // allow_text=true accepts a text-only answer.
        let mut texty = pending_decision("dec-rj-2", None);
        texty.allow_text = true;
        store.insert(&texty).unwrap();
        let outcome = store
            .answer("dec-rj-2", None, Some("free text".into()), None, 1)
            .unwrap();
        assert!(matches!(outcome, AnswerOutcome::Answered(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancel_resolves_once() {
        let (mut store, dir) = temp_store("cancel");
        store.insert(&pending_decision("dec-c-1", None)).unwrap();
        let outcome = store.cancel("dec-c-1").unwrap();
        let ResolveOutcome::Resolved(decision) = outcome else {
            panic!("expected resolved outcome");
        };
        assert_eq!(decision.status, DecisionStatus::Cancelled);
        assert!(matches!(
            store.cancel("dec-c-1").unwrap(),
            ResolveOutcome::AlreadyResolved
        ));
        assert!(matches!(
            store
                .answer("dec-c-1", Some("yes".into()), None, None, 1)
                .unwrap(),
            AnswerOutcome::AlreadyResolved
        ));
        assert!(matches!(
            store.cancel("dec-absent").unwrap(),
            ResolveOutcome::NotFound
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expire_pending_only_expires_due_pending_rows() {
        let (mut store, dir) = temp_store("expire");
        store
            .insert(&pending_decision("dec-e-1", Some(1_000)))
            .unwrap();
        store
            .insert(&pending_decision("dec-e-2", Some(10_000)))
            .unwrap();
        store.insert(&pending_decision("dec-e-3", None)).unwrap();
        let mut answered = pending_decision("dec-e-4", Some(500));
        store.insert(&answered).unwrap();
        store
            .answer("dec-e-4", Some("yes".into()), None, None, 600)
            .unwrap();
        answered.status = DecisionStatus::Answered;

        let expired = store.expire_pending(2_000).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].decision_id, "dec-e-1");
        assert_eq!(expired[0].status, DecisionStatus::Expired);

        assert_eq!(
            store.get("dec-e-1").unwrap().unwrap().status,
            DecisionStatus::Expired
        );
        assert_eq!(
            store.get("dec-e-2").unwrap().unwrap().status,
            DecisionStatus::Pending
        );
        assert_eq!(
            store.get("dec-e-3").unwrap().unwrap().status,
            DecisionStatus::Pending
        );
        assert_eq!(
            store.get("dec-e-4").unwrap().unwrap().status,
            DecisionStatus::Answered
        );

        // Re-running expiry at the same instant is a no-op.
        assert!(store.expire_pending(2_000).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expiry_works_after_reopen_and_next_pending_expiry_tracks_min() {
        let (mut store, dir) = temp_store("expirereopen");
        store
            .insert(&pending_decision("dec-x-1", Some(3_000)))
            .unwrap();
        store
            .insert(&pending_decision("dec-x-2", Some(1_500)))
            .unwrap();
        assert_eq!(store.next_pending_expiry_unix_ms().unwrap(), Some(1_500));
        drop(store);

        // Simulates a server restart: a fresh store sees and expires the due row.
        let mut reopened = DecisionStore::open_at(dir.join("herdr.db")).unwrap();
        let expired = reopened.expire_pending(2_000).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].decision_id, "dec-x-2");
        assert_eq!(reopened.next_pending_expiry_unix_ms().unwrap(), Some(3_000));
        let _ = std::fs::remove_dir_all(dir);
    }
}
