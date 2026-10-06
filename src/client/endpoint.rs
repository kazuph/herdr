//! Connection identities and endpoint-owned resource projections. A remote
//! resource is never inserted in the local server's AppState or PTY registry.

// These contracts are consumed by the endpoint transport and shell separately.
#![allow(dead_code)]

pub(crate) mod activation;
pub(crate) mod cache;
pub(crate) mod catalog;
pub(crate) mod chrome;
pub(crate) mod commands;
pub(crate) mod frontend;
pub(crate) mod handshake;
mod health;
pub(crate) mod jobs;
mod registry;
pub(crate) mod runtime;
pub(crate) mod shell;
pub(crate) mod sidebar;
pub(crate) mod supervisor;
pub(crate) mod transport;
pub(crate) mod writer;
pub(crate) use registry::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientEndpointStatus {
    Connecting,
    Online,
    Reconnecting,
    Attention,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FocusTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
    #[cfg(windows)]
    Notification {
        pane_id: String,
        boot_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ClientEndpointId {
    Local,
    Ssh(String),
}

impl ClientEndpointId {
    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

/// The resource ID remains opaque and is sent unchanged to its endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResourceKey {
    pub(crate) endpoint: ClientEndpointId,
    pub(crate) id: String,
}
