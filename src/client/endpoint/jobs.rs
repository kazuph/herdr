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

    /// Snapshot and job facts arrive separately. Retain the last accepted display
    /// until its replacement arrives, without reusing it across a server boot.
    pub(crate) fn for_presentation(
        &self,
        snapshot: &ClientShellSnapshot,
    ) -> Option<&EndpointJobsProjection> {
        self.projection
            .as_ref()
            .filter(|jobs| jobs.boot_id == snapshot.boot_id && jobs.revision <= snapshot.revision)
    }

    /// True while a finished job is still inside the sidebar indicator
    /// retention window, so the frontend keeps repainting until it expires.
    pub(crate) fn has_finished_indicator_pending_expiry(
        &self,
        snapshot: &ClientShellSnapshot,
        now_unix_ms: u128,
    ) -> bool {
        self.for_presentation(snapshot).is_some_and(|projection| {
            projection.jobs.iter().any(|job| {
                !matches!(job.status.as_str(), "running" | "cancelling" | "queued")
                    && crate::ui::sidebar::tokens::job_indicator_visible(
                        &job.status,
                        job.runner_alive,
                        job.finished_unix_ms,
                        now_unix_ms,
                    )
            })
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

    #[test]
    fn sidebar_jobs_remain_visible_between_snapshot_and_jobs_messages() {
        let mut cache = EndpointCache::default();
        let mut jobs = EndpointJobsCache::default();
        cache.begin_connection(1);
        jobs.begin_connection(1);
        let current = snapshot(7);
        cache.replace_snapshot(1, current.clone());
        let mut initial = projection(&current);
        initial.jobs.push(endpoint_job("running", None));
        assert!(jobs.replace(1, &cache, initial.clone()));
        let next = snapshot(8);
        cache.replace_snapshot(1, next.clone());
        assert!(jobs.for_snapshot(&next).is_none());
        assert_eq!(jobs.for_presentation(&next), Some(&initial));
        assert!(jobs.replace(1, &cache, projection(&next)));
        assert!(jobs.for_presentation(&next).unwrap().jobs.is_empty());
        let mut new_boot = next.clone();
        new_boot.boot_id = "new boot".into();
        assert!(jobs.for_presentation(&new_boot).is_none());
        jobs.begin_connection(2);
        assert!(jobs.for_presentation(&next).is_none());
    }

    fn endpoint_job(
        status: &str,
        finished_unix_ms: Option<u128>,
    ) -> crate::protocol::endpoint_jobs::EndpointJob {
        crate::protocol::endpoint_jobs::EndpointJob {
            id: status.into(),
            label: status.into(),
            command: "true".into(),
            cwd: "/remote".into(),
            caller_pane: "opaque:caller".into(),
            caller_agent: "agent".into(),
            completion: "none".into(),
            status: status.into(),
            runner_pid: None,
            exit_code: None,
            started_unix_ms: None,
            finished_unix_ms,
            log_path: String::new(),
            workspace_id: None,
            runner_alive: None,
        }
    }

    #[test]
    fn finished_indicator_pending_expiry_follows_the_retention_window() {
        let retention = crate::ui::sidebar::tokens::JOB_INDICATOR_FINISHED_RETENTION_MS;
        let now = 1_000_000u128;
        let mut cache = EndpointCache::default();
        let mut jobs = EndpointJobsCache::default();
        cache.begin_connection(1);
        jobs.begin_connection(1);
        let current = snapshot(7);
        cache.replace_snapshot(1, current.clone());
        let mut projected = projection(&current);
        projected.jobs = vec![
            endpoint_job("exited", Some(now - retention + 1)),
            endpoint_job("exited", Some(now - retention - 1)),
            endpoint_job("exited", None),
            endpoint_job("queued", None),
            endpoint_job("running", None),
        ];
        assert!(jobs.replace(1, &cache, projected));
        assert!(jobs.has_finished_indicator_pending_expiry(&current, now));
        let mut jobs = EndpointJobsCache::default();
        jobs.begin_connection(1);
        let mut projected = projection(&current);
        projected.jobs = vec![endpoint_job("exited", Some(now - retention - 1))];
        assert!(jobs.replace(1, &cache, projected));
        assert!(!jobs.has_finished_indicator_pending_expiry(&current, now));
    }
}
