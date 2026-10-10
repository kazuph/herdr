use super::cache::EndpointCache;
use crate::protocol::endpoint_decisions::EndpointDecisionsProjection;
use crate::protocol::endpoint_wire::ClientShellSnapshot;

/// Client-side cache of the server-owned pending-decision projection, keyed to
/// the endpoint's connection generation and boot id exactly like the jobs
/// projection. `None` means the server has not provided decision facts, unlike
/// a provided empty list.
#[derive(Default)]
pub(crate) struct EndpointDecisionsCache {
    generation: Option<u64>,
    projection: Option<EndpointDecisionsProjection>,
}

impl EndpointDecisionsCache {
    pub(crate) fn begin_connection(&mut self, generation: u64) -> bool {
        if self.generation.is_some_and(|current| generation <= current) {
            return false;
        }
        self.generation = Some(generation);
        self.projection = None;
        true
    }

    pub(crate) fn replace(
        &mut self,
        generation: u64,
        cache: &EndpointCache,
        projection: EndpointDecisionsProjection,
    ) -> bool {
        // The revision is borrowed from the snapshot stream: a decision update
        // does not necessarily bump the snapshot revision, so changed content
        // at the same revision is valid and only the identical projection is a
        // no-op.
        if self.generation != Some(generation)
            || !cache.accepts(generation)
            || !cache.snapshot().is_some_and(|snapshot| {
                snapshot.boot_id == projection.boot_id && snapshot.revision == projection.revision
            })
            || self
                .projection
                .as_ref()
                .is_some_and(|current| projection == *current)
        {
            return false;
        }
        self.projection = Some(projection);
        true
    }

    /// Snapshot and decision facts arrive in one batch but as separate frames.
    /// Retain the last accepted projection until its replacement arrives,
    /// without reusing it across a server boot.
    pub(crate) fn for_presentation(
        &self,
        snapshot: &ClientShellSnapshot,
    ) -> Option<&EndpointDecisionsProjection> {
        self.projection.as_ref().filter(|decisions| {
            decisions.boot_id == snapshot.boot_id && decisions.revision <= snapshot.revision
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(revision: u64) -> ClientShellSnapshot {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        snapshot.revision = revision;
        snapshot
    }

    fn projection(snapshot: &ClientShellSnapshot) -> EndpointDecisionsProjection {
        EndpointDecisionsProjection {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            decisions: Vec::new(),
        }
    }

    fn decision(id: &str) -> crate::api::schema::Decision {
        crate::api::schema::Decision {
            decision_id: id.into(),
            kind: crate::api::schema::DecisionKind::Ask,
            title: format!("title {id}"),
            body: None,
            options: Vec::new(),
            allow_text: false,
            origin: None,
            created_unix_ms: 0,
            expires_unix_ms: None,
            status: crate::api::schema::DecisionStatus::Pending,
            answer: None,
        }
    }

    #[test]
    fn endpoint_decisions_follow_generation_boot_and_revision() {
        let mut cache = EndpointCache::default();
        let mut decisions = EndpointDecisionsCache::default();
        cache.begin_connection(1);
        decisions.begin_connection(1);
        let current = snapshot(7);
        cache.replace_snapshot(1, current.clone());
        assert!(decisions.for_presentation(&current).is_none());
        assert!(decisions.replace(1, &cache, projection(&current)));
        assert!(decisions
            .for_presentation(&current)
            .unwrap()
            .decisions
            .is_empty());
        // Identical projections are no-ops; changed content at the same
        // snapshot revision is accepted because decision updates do not
        // necessarily bump the snapshot revision.
        assert!(!decisions.replace(1, &cache, projection(&current)));
        let mut updated = projection(&current);
        updated.decisions = vec![decision("d1")];
        assert!(decisions.replace(1, &cache, updated));
        assert_eq!(
            decisions
                .for_presentation(&current)
                .unwrap()
                .decisions
                .len(),
            1
        );
        let future = snapshot(8);
        assert!(!decisions.replace(1, &cache, projection(&future)));
        cache.replace_snapshot(1, future.clone());
        // The previous projection remains visible until its replacement lands.
        assert!(decisions.for_presentation(&future).is_some());
        assert!(decisions.replace(1, &cache, projection(&future)));
        let mut wrong_boot = projection(&future);
        wrong_boot.boot_id = "previous server".into();
        assert!(!decisions.replace(1, &cache, wrong_boot));
        assert!(decisions.for_presentation(&future).is_some());
        let mut new_boot = future.clone();
        new_boot.boot_id = "new boot".into();
        assert!(decisions.for_presentation(&new_boot).is_none());
        // Disconnect clears live acceptance; reconnect resets the epoch.
        cache.disconnect(1);
        assert!(!decisions.replace(1, &cache, projection(&future)));
        cache.begin_connection(2);
        decisions.begin_connection(2);
        assert!(decisions.for_presentation(&future).is_none());
        cache.replace_snapshot(2, current.clone());
        assert!(!decisions.replace(1, &cache, projection(&current)));
        assert!(decisions.replace(2, &cache, projection(&current)));
        assert!(!decisions.begin_connection(1));
    }
}
