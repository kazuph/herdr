use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerLiveHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    /// Stable client-owned endpoint generation implemented by the running server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_generation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_min_generation: Option<u32>,
}

impl ServerCapabilities {
    pub(crate) fn endpoint_protocol_compatible(&self) -> bool {
        self.endpoint_protocol_generation.is_some_and(|generation| {
            let floor = self
                .endpoint_protocol_min_generation
                .unwrap_or(crate::protocol::endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION);
            floor <= generation
                && generation >= crate::protocol::endpoint::ENDPOINT_PROTOCOL_MIN_GENERATION
                && floor <= crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ServerCapabilities;

    #[test]
    fn endpoint_status_preserves_legacy_json_and_negotiates_generation_range() {
        let legacy: ServerCapabilities =
            serde_json::from_str(r#"{"live_handoff":true,"detached_server_daemon":false}"#)
                .unwrap();
        assert!(!legacy.endpoint_protocol_compatible());
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json.get("endpoint_protocol_generation").is_none());
        for (generation, floor, expected) in [
            (1, None, true),
            (1, Some(1), true),
            (2, Some(1), true),
            (2, Some(2), false),
            (0, Some(1), false),
            (1, Some(2), false),
        ] {
            let value = ServerCapabilities {
                endpoint_protocol_generation: Some(generation),
                endpoint_protocol_min_generation: floor,
                ..legacy.clone()
            };
            assert_eq!(value.endpoint_protocol_compatible(), expected, "{value:?}");
        }
    }
}

/// Runtime history persistence belongs to the server owning those terminals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerPaneHistorySetParams {
    pub enabled: bool,
}
