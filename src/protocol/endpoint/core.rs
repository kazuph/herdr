//! Stable endpoint compatibility contract for client-owned shells.
//!
//! The endpoint generation is intentionally independent from the private
//! binary protocol used by same-install CLI, direct-terminal, and handoff
//! paths. Generation 1 is the compatibility floor for Local, SSH, and Cloud
//! shell endpoints and must remain available indefinitely unless retired for a
//! security reason. New JSON fields must be optional or have serde defaults;
//! new enum values need an `Unknown` fallback. Unknown named controls are
//! optional and ignored unless negotiated as part of the core.

use serde::{Deserialize, Serialize};

// Fixed upstream server/client_commands.rs stable endpoint envelope limits.
pub(crate) const MAX_ENDPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BOOT_ID_BYTES: usize = 128;
pub(crate) const MAX_ENDPOINT_REQUEST_ID_BYTES: usize = 128;
pub(crate) const ENDPOINT_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

use super::{default_min_generation, EndpointGenerationRange, EndpointHandshakeError};
#[cfg(test)]
use crate::protocol::endpoint_wire::ClientShellSnapshot;
use crate::protocol::endpoint_wire::{ClientSurfaceSize, ServerMessage};

use super::ENDPOINT_PROTOCOL_GENERATION;
pub const ENDPOINT_HELLO_KIND: &str = "endpoint.hello.v1";
pub const ENDPOINT_WELCOME_KIND: &str = "endpoint.welcome.v1";
pub const SNAPSHOT_CODEC_V1: &str = "shell.snapshot.v1";
pub const ENDPOINT_SNAPSHOT_KIND: &str = SNAPSHOT_CODEC_V1;
pub const SURFACE_CODEC_V1: &str = "shell.surface.v1";
pub const INPUT_CODEC_V1: &str = "shell.input.semantic.v1";
pub const BLOB_CODEC_V1: &str = "shell.blob.v1";
pub const SURFACE_INTEREST_CAPABILITY: &str = "surface_interest";
pub const PRESENTATION_EFFECTS_FENCE_CAPABILITY: &str = "presentation_effects_fence";
pub const PRESENTATION_EFFECTS_SYNC_KIND: &str = "endpoint.presentation.sync.v1";
pub const PRESENTATION_EFFECTS_READY_KIND: &str = "endpoint.presentation.ready.v1";
pub const HEALTH_CHECK_CAPABILITY: &str = "health_check";
pub const HEALTH_PING_KIND: &str = "endpoint.health.ping.v1";
pub const HEALTH_PONG_KIND: &str = "endpoint.health.pong.v1";
pub const AGENT_VIEW_PROJECTION_CAPABILITY: &str = "agent_view_projection";
pub const AGENT_VIEW_PROJECTION_KIND: &str = "endpoint.agent-view.v1";
pub const AGENT_COMPLETIONS_CAPABILITY: &str = "agent_completions";
pub const AGENT_COMPLETIONS_KIND: &str = "endpoint.agent-completions.v1";

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointClientHello {
    pub generation: u32,
    #[serde(default = "default_min_generation")]
    pub min_generation: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub surface_size: ClientSurfaceSize,
    pub pixel_mouse: bool,
    pub direct_graphics: bool,
    pub endpoint_keybindings: bool,
    pub mouse_capture: bool,
    #[serde(default = "default_true")]
    pub surface_active: bool,
    /// Accept the optional cell-retaining surface encoding on this connection.
    #[serde(default)]
    pub surface_reuse: bool,
    /// Accept the optional surface-delta encoding on this connection.
    #[serde(default)]
    pub surface_delta: bool,
    /// Accept the optional scroll-aware patch encoding on this connection.
    #[serde(default)]
    pub surface_scroll: bool,
    #[serde(default)]
    pub snapshot_codecs: Vec<String>,
    #[serde(default)]
    pub surface_codecs: Vec<String>,
    #[serde(default)]
    pub input_codecs: Vec<String>,
    #[serde(default)]
    pub blob_codecs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointServerWelcome {
    pub generation: u32,
    #[serde(default = "default_min_generation")]
    pub min_generation: u32,
    pub server_version: String,
    pub snapshot_codec: String,
    pub surface_codec: String,
    pub input_codec: String,
    pub blob_codec: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EndpointHandshakeError>,
}

pub fn snapshot_message(snapshot: &impl Serialize) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: ENDPOINT_SNAPSHOT_KIND.into(),
        data: serde_json::to_string(snapshot)?,
    })
}

impl EndpointClientHello {
    pub fn generation_range(&self) -> EndpointGenerationRange {
        EndpointGenerationRange::at(self.generation, self.min_generation)
    }

    pub fn supports_required_codecs(&self) -> bool {
        self.snapshot_codecs
            .iter()
            .any(|codec| codec == SNAPSHOT_CODEC_V1)
            && self
                .surface_codecs
                .iter()
                .any(|codec| codec == SURFACE_CODEC_V1)
            && self
                .input_codecs
                .iter()
                .any(|codec| codec == INPUT_CODEC_V1)
            && self.blob_codecs.iter().any(|codec| codec == BLOB_CODEC_V1)
    }
}

impl EndpointServerWelcome {
    pub fn compatible(methods: Vec<String>, capabilities: Vec<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            min_generation: super::ENDPOINT_PROTOCOL_MIN_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods,
            capabilities,
            error: None,
        }
    }

    pub fn incompatible(code: &str, message: impl Into<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            min_generation: super::ENDPOINT_PROTOCOL_MIN_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods: Vec::new(),
            capabilities: Vec::new(),
            error: Some(EndpointHandshakeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

/// Preserve the fork range and method gating contract on the production JSON core.
pub fn negotiate_core(
    hello: &EndpointClientHello,
    methods: Vec<String>,
    capabilities: Vec<String>,
) -> EndpointServerWelcome {
    let contract = super::EndpointHello {
        generation: hello.generation,
        min_generation: hello.min_generation,
        client_version: String::new(),
        cols: hello.surface_size.cols,
        rows: hello.surface_size.rows,
        capabilities: Vec::new(),
    };
    let negotiated = super::negotiate(&contract, methods);
    if let Some(error) = negotiated.error {
        return EndpointServerWelcome::incompatible(&error.code, error.message);
    }
    if !hello.supports_required_codecs() {
        return EndpointServerWelcome::incompatible(
            "no_common_core",
            "client and server have no compatible endpoint core codecs",
        );
    }
    EndpointServerWelcome::compatible(negotiated.methods, capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> EndpointClientHello {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-hello-v1.json"
        )))
        .expect("fixed upstream hello")
    }

    #[test]
    fn fixed_upstream_json_core_decodes_without_fork_range_extension() {
        let hello = hello();
        assert_eq!(hello.min_generation, 1);
        assert!(hello.supports_required_codecs());
        let welcome: EndpointServerWelcome = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-welcome-v1.json"
        )))
        .expect("fixed upstream welcome");
        assert_eq!(welcome.min_generation, 1);
        assert_eq!(welcome.snapshot_codec, SNAPSHOT_CODEC_V1);
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .expect("fixed upstream snapshot");
        assert_eq!(snapshot.boot_id, "boot-v1");
        assert_eq!(
            snapshot.workspaces[0].agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
    }

    #[test]
    fn json_core_keeps_additive_range_and_per_method_disable_contract() {
        let mut client = hello();
        client.generation = 2;
        let welcome = negotiate_core(&client, vec!["pane.list".into()], Vec::new());
        assert!(welcome.error.is_none());
        let disabled = super::super::disabled_actions_for_session(
            client.generation,
            welcome.generation,
            &welcome.methods,
            &[
                super::super::EndpointMethod {
                    name: "pane.list",
                    min_generation: 1,
                },
                super::super::EndpointMethod {
                    name: "pane.close",
                    min_generation: 1,
                },
                super::super::EndpointMethod {
                    name: "machine.route",
                    min_generation: 2,
                },
            ],
        );
        assert_eq!(disabled, ["pane.close", "machine.route"]);
        client.min_generation = 2;
        let welcome = negotiate_core(&client, vec!["pane.list".into()], Vec::new());
        assert_eq!(
            welcome.error.expect("incompatible floor").code,
            super::super::CODE_GENERATION_NEWER
        );
        assert!(welcome.methods.is_empty());
    }

    #[test]
    fn no_common_core_is_a_connection_error_not_a_private_wire_retry() {
        let mut client = hello();
        client.surface_codecs.clear();
        let welcome = negotiate_core(
            &client,
            vec!["pane.list".into()],
            vec![HEALTH_CHECK_CAPABILITY.into()],
        );
        assert_eq!(welcome.error.expect("missing codec").code, "no_common_core");
        assert!(welcome.methods.is_empty());
        assert!(welcome.capabilities.is_empty());
    }
}
