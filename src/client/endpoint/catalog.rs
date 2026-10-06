//! Watch the existing fork array catalog without changing its persistence or CLI contracts.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::machine::MachineProfile;

// Fixed upstream client/catalog_reload.rs watch interval; no upstream catalog limits imported.
const CATALOG_WATCH_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) type CatalogUpdate = Result<Vec<MachineProfile>, String>;

/// Failed reads retain the last accepted profiles; a valid empty array explicitly removes them.
#[derive(Default)]
pub(crate) struct LiveCatalog {
    profiles: Vec<MachineProfile>,
    error: Option<String>,
}

impl LiveCatalog {
    pub(crate) fn apply(&mut self, update: CatalogUpdate) -> bool {
        match update {
            Ok(profiles) => {
                let changed = profiles != self.profiles;
                self.profiles = profiles;
                self.error = None;
                changed
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    pub(crate) fn profiles(&self) -> &[MachineProfile] {
        &self.profiles
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

pub(crate) fn watch(
    path: PathBuf,
    events: tokio::sync::mpsc::Sender<CatalogUpdate>,
    stopped: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("endpoint-catalog".into())
        .spawn(move || {
            let mut previous = None;
            while !stopped.load(Ordering::Acquire) {
                let update =
                    crate::machine::load_profiles_result(&path).map_err(|error| error.to_string());
                if previous.as_ref() != Some(&update) {
                    previous = Some(update.clone());
                    if events.blocking_send(update).is_err() {
                        break;
                    }
                }
                // Shutdown can unpark the owned thread instead of waiting for the next catalog tick.
                std::thread::park_timeout(CATALOG_WATCH_INTERVAL);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn receive(receiver: &mut tokio::sync::mpsc::Receiver<CatalogUpdate>) -> CatalogUpdate {
        // Same four-second owned-test bound used by the socket integration tests.
        tokio::time::timeout(Duration::from_secs(4), receiver.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn endpoint_catalog_actual_file_corruption_retains_profiles_and_cli_empty_contract() {
        let path = std::env::temp_dir().join(format!(
            "herdr-catalog-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(crate::machine::load_profiles_result(&path)
            .unwrap()
            .is_empty());
        let profile = MachineProfile {
            id: "m-existing-id".into(),
            label: "Build".into(),
            target: "uncontacted-host".into(),
            session: "explicit-session".into(),
            enabled: false,
        };
        let mut catalog = crate::machine::MachineCatalog {
            profiles: vec![profile.clone()],
        };
        crate::machine::save_to_path(&path, &catalog).unwrap();
        let (events, mut receiver) = tokio::sync::mpsc::channel(8);
        let stopped = Arc::new(AtomicBool::new(false));
        let watcher = watch(path.clone(), events, stopped.clone()).unwrap();
        let mut live = LiveCatalog::default();
        assert!(live.apply(receive(&mut receiver).await));
        assert_eq!(live.profiles(), std::slice::from_ref(&profile));
        std::fs::write(&path, "{broken").unwrap();
        watcher.thread().unpark();
        assert!(!live.apply(receive(&mut receiver).await));
        assert!(live.error().is_some());
        assert_eq!(live.profiles(), std::slice::from_ref(&profile));
        assert!(crate::machine::load_from_path(&path).profiles.is_empty());
        assert_eq!(
            crate::machine::load_profiles_result(&path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        catalog.profiles[0].label = "Renamed".into();
        crate::machine::save_to_path(&path, &catalog).unwrap();
        watcher.thread().unpark();
        assert!(live.apply(receive(&mut receiver).await));
        assert_eq!(live.profiles()[0].id, profile.id);
        assert_eq!(live.profiles()[0].session, profile.session);
        assert_eq!(live.profiles()[0].label, "Renamed");
        assert!(!live.profiles()[0].enabled);
        assert!(live.error().is_none());
        std::fs::write(&path, "[]").unwrap();
        watcher.thread().unpark();
        assert!(live.apply(receive(&mut receiver).await));
        assert!(live.profiles().is_empty());
        stopped.store(true, Ordering::Release);
        watcher.thread().unpark();
        watcher.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
