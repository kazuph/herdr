//! Stable endpoint compatibility contract (UP-MULTICLIENT PR1).
//!
//! The endpoint generation is intentionally independent from the private
//! bincode wire protocol versioned by [`super::PROTOCOL_VERSION`]. The binary
//! `ClientMessage::Hello` is not forward or backward compatible across
//! versions, so mixed-version interop is negotiated on this separate
//! generation axis instead of by widening the strict `check_client_version`
//! equality check (which stays strict until a JSON endpoint transport lands).
//!
//! Generation 1 is the compatibility floor and must remain available
//! indefinitely unless retired for a security reason. Forward-compatibility
//! rules for this surface: new JSON fields must be optional or carry serde
//! defaults, and new enum values need an `Unknown` fallback. A generation
//! older than 1 never connects silently; it receives an explicit one-time
//! upgrade error (fail-closed, no silent fallback to legacy behavior).

use serde::{Deserialize, Serialize};

/// Current endpoint generation advertised by this binary.
pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;

/// Machine-readable code for a client older than generation 1.
pub const CODE_GENERATION_UNSUPPORTED: &str = "endpoint_generation_unsupported";
/// Machine-readable code for a client newer than this server's generation.
pub const CODE_GENERATION_NEWER: &str = "endpoint_generation_newer";

/// Client side of the endpoint handshake (JSON surface, generation axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHello {
    pub generation: u32,
    pub client_version: String,
    pub cols: u16,
    pub rows: u16,
    /// Optional capability tokens; unknown tokens are ignored by the server.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Server side of the endpoint handshake (JSON surface, generation axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointWelcome {
    pub generation: u32,
    pub server_version: String,
    /// Method names this server generation offers.
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EndpointHandshakeError>,
}

/// Structured handshake failure; the connection must not proceed silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHandshakeError {
    pub code: String,
    pub message: String,
}

/// A server method with the minimum endpoint generation that offers it.
/// Used to compute per-action availability without touching the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointMethod {
    pub name: &'static str,
    pub min_generation: u32,
}

impl EndpointWelcome {
    fn compatible(server_version: String, methods: Vec<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version,
            methods,
            error: None,
        }
    }

    fn incompatible(code: &str, message: String, server_version: String) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version,
            methods: Vec::new(),
            error: Some(EndpointHandshakeError {
                code: code.into(),
                message,
            }),
        }
    }

    /// True when the handshake allows the session to proceed.
    pub fn is_compatible(&self) -> bool {
        self.error.is_none()
    }
}

/// Negotiate the endpoint generation axis. Never falls back silently:
/// out-of-range generations receive an explicit upgrade error.
pub fn negotiate(hello: &EndpointHello, server_methods: Vec<String>) -> EndpointWelcome {
    let server_version = crate::build_info::version();
    if hello.generation < ENDPOINT_PROTOCOL_GENERATION {
        return EndpointWelcome::incompatible(
            CODE_GENERATION_UNSUPPORTED,
            format!(
                "endpoint generation {} is older than the minimum supported generation {}; perform a one-time upgrade of the client before reconnecting",
                hello.generation, ENDPOINT_PROTOCOL_GENERATION
            ),
            server_version,
        );
    }
    if hello.generation > ENDPOINT_PROTOCOL_GENERATION {
        return EndpointWelcome::incompatible(
            CODE_GENERATION_NEWER,
            format!(
                "endpoint generation {} is newer than this server's generation {}; upgrade the server before reconnecting",
                hello.generation, ENDPOINT_PROTOCOL_GENERATION
            ),
            server_version,
        );
    }
    EndpointWelcome::compatible(server_version, server_methods)
}

/// Names of `required` methods unavailable to a client on `client_generation`.
/// Only the listed actions are disabled; the session itself stays up.
pub fn disabled_actions(client_generation: u32, required: &[EndpointMethod]) -> Vec<String> {
    required
        .iter()
        .filter(|method| client_generation < method.min_generation)
        .map(|method| method.name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(generation: u32) -> EndpointHello {
        EndpointHello {
            generation,
            client_version: "test".to_string(),
            cols: 80,
            rows: 24,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn current_generation_negotiates_compatible() {
        let welcome = negotiate(
            &hello(ENDPOINT_PROTOCOL_GENERATION),
            vec!["pane.list".into()],
        );
        assert!(welcome.is_compatible());
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.methods, vec!["pane.list".to_string()]);
        assert_eq!(welcome.error, None);
    }

    #[test]
    fn older_generation_is_rejected_with_upgrade_guidance() {
        let welcome = negotiate(&hello(0), vec![]);
        assert!(!welcome.is_compatible());
        let error = welcome.error.expect("older generation must carry an error");
        assert_eq!(error.code, CODE_GENERATION_UNSUPPORTED);
        assert!(error.message.contains("one-time upgrade"));
        assert!(welcome.methods.is_empty());
    }

    #[test]
    fn newer_generation_is_rejected_with_server_upgrade_guidance() {
        let welcome = negotiate(&hello(ENDPOINT_PROTOCOL_GENERATION + 1), vec![]);
        assert!(!welcome.is_compatible());
        let error = welcome.error.expect("newer generation must carry an error");
        assert_eq!(error.code, CODE_GENERATION_NEWER);
        assert!(error.message.contains("upgrade the server"));
    }

    #[test]
    fn disabled_actions_only_lists_gated_methods() {
        let required = [
            EndpointMethod {
                name: "pane.list",
                min_generation: 1,
            },
            EndpointMethod {
                name: "machine.route",
                min_generation: 2,
            },
        ];
        assert!(disabled_actions(1, &required).is_empty());
        assert_eq!(
            disabled_actions(0, &required),
            vec!["pane.list".to_string(), "machine.route".to_string()]
        );
        // Unknown future generation keeps working for known methods.
        assert!(disabled_actions(99, &required).is_empty());
    }

    #[test]
    fn hello_tolerates_unknown_future_fields() {
        // Forward-compat rule: unknown fields are ignored, not rejected.
        let raw =
            r#"{"generation":1,"client_version":"x","cols":80,"rows":24,"future_field":"ok"}"#;
        let parsed: EndpointHello = serde_json::from_str(raw).expect("unknown fields ignored");
        assert_eq!(parsed.generation, 1);
    }

    #[test]
    fn v1_fixtures_parse() {
        let hello: EndpointHello =
            serde_json::from_str(include_str!("../../tests/fixtures/endpoint-hello-v1.json"))
                .expect("hello fixture parses");
        assert_eq!(hello.generation, ENDPOINT_PROTOCOL_GENERATION);
        let welcome: EndpointWelcome = serde_json::from_str(include_str!(
            "../../tests/fixtures/endpoint-welcome-v1.json"
        ))
        .expect("welcome fixture parses");
        assert!(welcome.is_compatible());
    }
}
