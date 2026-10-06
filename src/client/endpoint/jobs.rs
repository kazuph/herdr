use super::cache::EndpointCache;
use crate::protocol::endpoint_jobs::EndpointJobsProjection;
use crate::protocol::endpoint_wire::ClientShellSnapshot;

/// None means the server has not provided job facts, unlike a provided empty list.
#[derive(Default)]
pub(crate) struct EndpointJobsCache {
    generation: Option<u64>,
    projection: Option<EndpointJobsProjection>,
}

impl EndpointJobsCache {
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
        projection: EndpointJobsProjection,
    ) -> bool {
        if self.generation != Some(generation)
            || !cache.accepts(generation)
            || !cache.snapshot().is_some_and(|snapshot| {
                snapshot.boot_id == projection.boot_id && snapshot.revision == projection.revision
            })
            || self.projection.as_ref().is_some_and(|current| {
                projection.revision < current.revision
                    || (projection.revision == current.revision && projection != *current)
            })
        {
            return false;
        }
        self.projection = Some(projection);
        true
    }

    pub(crate) fn for_snapshot(
        &self,
        snapshot: &ClientShellSnapshot,
    ) -> Option<&EndpointJobsProjection> {
        self.projection
            .as_ref()
            .filter(|jobs| jobs.boot_id == snapshot.boot_id && jobs.revision == snapshot.revision)
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

    fn projection(snapshot: &ClientShellSnapshot) -> EndpointJobsProjection {
        EndpointJobsProjection {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            jobs: Vec::new(),
        }
    }

    #[test]
    fn endpoint_jobs_absence_empty_and_epoch_boot_revision_are_distinct() {
        let mut cache = EndpointCache::default();
        let mut jobs = EndpointJobsCache::default();
        cache.begin_connection(1);
        jobs.begin_connection(1);
        let current = snapshot(7);
        cache.replace_snapshot(1, current.clone());
        assert!(jobs.for_snapshot(&current).is_none());
        assert!(jobs.replace(1, &cache, projection(&current)));
        assert!(jobs.for_snapshot(&current).unwrap().jobs.is_empty());
        let future = snapshot(8);
        assert!(!jobs.replace(1, &cache, projection(&future)));
        cache.replace_snapshot(1, future.clone());
        assert!(jobs.for_snapshot(&future).is_none());
        assert!(!jobs.replace(1, &cache, projection(&current)));
        assert!(jobs.replace(1, &cache, projection(&future)));
        let mut wrong_boot = projection(&future);
        wrong_boot.boot_id = "previous server".into();
        assert!(!jobs.replace(1, &cache, wrong_boot));
        cache.disconnect(1);
        assert!(!jobs.replace(1, &cache, projection(&future)));
        cache.begin_connection(2);
        jobs.begin_connection(2);
        cache.replace_snapshot(2, current.clone());
        assert!(!jobs.replace(1, &cache, projection(&current)));
        assert!(jobs.for_snapshot(&current).is_none());
        assert!(jobs.replace(2, &cache, projection(&current)));
        assert!(!jobs.begin_connection(1));
    }
}
