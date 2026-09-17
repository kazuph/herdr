//! Stable endpoint compatibility contract (UP-MULTICLIENT PR1).
//!
//! The endpoint generation is intentionally independent from the private
//! bincode wire protocol versioned by [`super::PROTOCOL_VERSION`]. The binary
//! `ClientMessage::Hello` is not forward or backward compatible across
//! versions, so mixed-version interop is negotiated on this separate
//! generation axis instead of by widening the strict `check_client_version`
//! equality check (which stays strict until a JSON endpoint transport lands).
//!
//! Generation is a range, not a strict equality check. Each peer advertises
//! `generation` (highest generation it implements) and `min_generation`
//! (lowest generation it still speaks). A connection proceeds when the ranges
//! overlap:
//! `client.generation >= server.min_generation && server.generation >= client.min_generation`.
//!
//! Generation 1 is the compatibility floor
//! ([`ENDPOINT_PROTOCOL_MIN_GENERATION`]) and must remain available
//! indefinitely unless retired for a security reason. Additive features bump
//! `generation` but leave `min_generation` at 1 so a newer client can stay
//! connected to an older server; unsupported actions are disabled for the
//! session instead of tearing it down (C1/C2). Only a breaking change raises
//! `min_generation`. A peer older than the other side's floor never connects
//! silently; it receives an explicit one-time upgrade error (C3).
//!
//! Forward-compatibility rules for this surface: new JSON fields must be
//! optional or carry serde defaults, and new enum values need an `Unknown`
//! fallback.

// Handshake types are the PR1 contract surface. The JSON endpoint transport
// that will call them is follow-up work, so they are unused in production.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Current endpoint generation advertised by this binary.
pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;

/// Lowest endpoint generation this binary still speaks.
///
/// This is the compatibility floor. Additive generation bumps must not raise
/// it; only a breaking change (or a security retirement) may.
pub const ENDPOINT_PROTOCOL_MIN_GENERATION: u32 = 1;

/// Historical generation-1 floor used when `min_generation` is omitted.
/// Frozen at 1 so a later floor bump cannot reinterpret gen-1 hellos.
const DEFAULT_MIN_GENERATION: u32 = 1;

/// Machine-readable code for a client older than this server's floor.
pub const CODE_GENERATION_UNSUPPORTED: &str = "endpoint_generation_unsupported";
/// Machine-readable code for a server older than this client's floor.
pub const CODE_GENERATION_NEWER: &str = "endpoint_generation_newer";

fn default_min_generation() -> u32 {
    DEFAULT_MIN_GENERATION
}

/// Generation range a peer can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointGenerationRange {
    /// Highest generation this peer implements.
    pub generation: u32,
    /// Lowest generation this peer still speaks.
    pub min_generation: u32,
}

impl EndpointGenerationRange {
    /// Range advertised by this binary.
    pub const fn current() -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            min_generation: ENDPOINT_PROTOCOL_MIN_GENERATION,
        }
    }

    /// Range for a simulated peer. Used by mixed-generation tests.
    pub const fn at(generation: u32, min_generation: u32) -> Self {
        Self {
            generation,
            min_generation,
        }
    }
}

/// Client side of the endpoint handshake (JSON surface, generation axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHello {
    pub generation: u32,
    /// Lowest generation this client still speaks. Omitted gen-1 hellos default to 1.
    #[serde(default = "default_min_generation")]
    pub min_generation: u32,
    pub client_version: String,
    pub cols: u16,
    pub rows: u16,
    /// Optional capability tokens; unknown tokens are ignored by the server.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl EndpointHello {
    /// Generation range advertised by this hello.
    pub fn generation_range(&self) -> EndpointGenerationRange {
        EndpointGenerationRange {
            generation: self.generation,
            min_generation: self.min_generation,
        }
    }
}

/// Server side of the endpoint handshake (JSON surface, generation axis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointWelcome {
    pub generation: u32,
    /// Lowest generation this server still speaks. Omitted gen-1 welcomes default to 1.
    #[serde(default = "default_min_generation")]
    pub min_generation: u32,
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
    fn compatible(
        server: EndpointGenerationRange,
        server_version: String,
        methods: Vec<String>,
    ) -> Self {
        Self {
            generation: server.generation,
            min_generation: server.min_generation,
            server_version,
            methods,
            error: None,
        }
    }

    fn incompatible(
        server: EndpointGenerationRange,
        code: &str,
        message: String,
        server_version: String,
    ) -> Self {
        Self {
            generation: server.generation,
            min_generation: server.min_generation,
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

/// Names this `server_generation` actually offers from `catalog`.
pub fn methods_for_generation(server_generation: u32, catalog: &[EndpointMethod]) -> Vec<String> {
    catalog
        .iter()
        .filter(|method| server_generation >= method.min_generation)
        .map(|method| method.name.to_string())
        .collect()
}

/// Common generation both peers can use for action gating.
pub fn available_generation(client_generation: u32, server_generation: u32) -> u32 {
    client_generation.min(server_generation)
}

/// Negotiate using this binary's generation range.
pub fn negotiate(hello: &EndpointHello, server_methods: Vec<String>) -> EndpointWelcome {
    negotiate_with(hello, EndpointGenerationRange::current(), server_methods)
}

/// Negotiate the endpoint generation axis between a client hello and a server range.
///
/// Overlapping ranges keep the connection; only an explicit floor miss is an
/// upgrade error. Additive clients (`generation` higher, `min_generation`
/// still at the floor) stay connected to older servers.
pub fn negotiate_with(
    hello: &EndpointHello,
    server: EndpointGenerationRange,
    server_methods: Vec<String>,
) -> EndpointWelcome {
    let server_version = crate::build_info::version();
    let client = hello.generation_range();

    if client.generation < server.min_generation {
        return EndpointWelcome::incompatible(
            server,
            CODE_GENERATION_UNSUPPORTED,
            format!(
                "endpoint generation {} is older than the minimum supported generation {}; perform a one-time upgrade of the client before reconnecting",
                client.generation, server.min_generation
            ),
            server_version,
        );
    }
    if server.generation < client.min_generation {
        return EndpointWelcome::incompatible(
            server,
            CODE_GENERATION_NEWER,
            format!(
                "endpoint generation {} is newer than this server's generation {}; upgrade the server before reconnecting",
                client.generation, server.generation
            ),
            server_version,
        );
    }
    EndpointWelcome::compatible(server, server_version, server_methods)
}

/// Names of `required` methods unavailable at `available_generation`.
///
/// `available_generation` is the negotiated session generation
/// (`min(client, server)`). Only the listed actions are disabled; the session
/// itself stays up.
pub fn disabled_actions(available_generation: u32, required: &[EndpointMethod]) -> Vec<String> {
    required
        .iter()
        .filter(|method| available_generation < method.min_generation)
        .map(|method| method.name.to_string())
        .collect()
}

/// C2 disable set for a mixed-generation session: methods the negotiated
/// generation cannot offer, plus methods the server did not advertise.
pub fn disabled_actions_for_session(
    client_generation: u32,
    server_generation: u32,
    advertised_methods: &[String],
    required: &[EndpointMethod],
) -> Vec<String> {
    let available = available_generation(client_generation, server_generation);
    required
        .iter()
        .filter(|method| {
            available < method.min_generation
                || !advertised_methods
                    .iter()
                    .any(|advertised| advertised == method.name)
        })
        .map(|method| method.name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(generation: u32) -> EndpointHello {
        hello_with_min(generation, DEFAULT_MIN_GENERATION)
    }

    fn hello_with_min(generation: u32, min_generation: u32) -> EndpointHello {
        EndpointHello {
            generation,
            min_generation,
            client_version: "test".to_string(),
            cols: 80,
            rows: 24,
            capabilities: Vec::new(),
        }
    }

    fn gen2_catalog() -> [EndpointMethod; 2] {
        [
            EndpointMethod {
                name: "pane.list",
                min_generation: 1,
            },
            EndpointMethod {
                name: "machine.route",
                min_generation: 2,
            },
        ]
    }

    #[test]
    fn current_generation_negotiates_compatible() {
        let welcome = negotiate(
            &hello(ENDPOINT_PROTOCOL_GENERATION),
            vec!["pane.list".into()],
        );
        assert!(welcome.is_compatible());
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.min_generation, ENDPOINT_PROTOCOL_MIN_GENERATION);
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
    fn additive_newer_client_stays_connected_to_current_server() {
        // Production gen1 `negotiate()` must accept a future additive client so
        // that C1/C2 remain reachable after generation 2 ships.
        let welcome = negotiate(
            &hello_with_min(ENDPOINT_PROTOCOL_GENERATION + 1, DEFAULT_MIN_GENERATION),
            vec!["pane.list".into()],
        );
        assert!(welcome.is_compatible());
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.methods, vec!["pane.list".to_string()]);
        assert_eq!(welcome.error, None);
    }

    #[test]
    fn generation_zero_server_is_rejected_with_upgrade_guidance() {
        let welcome = negotiate_with(&hello(1), EndpointGenerationRange::at(0, 0), vec![]);
        assert!(!welcome.is_compatible());
        let error = welcome.error.expect("pre-floor server must carry an error");
        assert_eq!(error.code, CODE_GENERATION_NEWER);
        assert!(error.message.contains("upgrade the server"));
    }

    #[test]
    fn breaking_newer_client_is_rejected_when_min_exceeds_server() {
        let welcome = negotiate(
            &hello_with_min(
                ENDPOINT_PROTOCOL_GENERATION + 1,
                ENDPOINT_PROTOCOL_GENERATION + 1,
            ),
            vec![],
        );
        assert!(!welcome.is_compatible());
        let error = welcome.error.expect("breaking client must carry an error");
        assert_eq!(error.code, CODE_GENERATION_NEWER);
        assert!(error.message.contains("upgrade the server"));
    }

    #[test]
    fn gen2_client_on_gen1_server_disables_only_additive_machine_route() {
        // C1/C2 simulation: generation 2 only adds additive `machine.route`.
        let catalog = gen2_catalog();
        let server = EndpointGenerationRange::at(1, 1);
        let client = hello_with_min(2, 1);
        let advertised = methods_for_generation(server.generation, &catalog);
        let welcome = negotiate_with(&client, server, advertised.clone());

        assert!(
            welcome.is_compatible(),
            "gen2 client + gen1 server must stay connected: {:?}",
            welcome.error
        );
        assert_eq!(welcome.generation, 1);
        assert_eq!(welcome.methods, vec!["pane.list".to_string()]);
        assert!(!welcome.methods.iter().any(|name| name == "machine.route"));

        let disabled = disabled_actions_for_session(
            client.generation,
            welcome.generation,
            &welcome.methods,
            &catalog,
        );
        assert_eq!(disabled, vec!["machine.route".to_string()]);
        assert_eq!(
            disabled_actions(
                available_generation(client.generation, welcome.generation),
                &catalog
            ),
            vec!["machine.route".to_string()]
        );
        assert_eq!(advertised, vec!["pane.list".to_string()]);
    }

    #[test]
    fn gen1_client_on_gen2_server_stays_connected_and_disables_machine_route() {
        let catalog = gen2_catalog();
        let server = EndpointGenerationRange::at(2, 1);
        let client = hello_with_min(1, 1);
        let advertised = methods_for_generation(server.generation, &catalog);
        let welcome = negotiate_with(&client, server, advertised);

        assert!(welcome.is_compatible());
        assert_eq!(welcome.generation, 2);
        assert_eq!(
            welcome.methods,
            vec!["pane.list".to_string(), "machine.route".to_string()]
        );
        assert_eq!(
            disabled_actions_for_session(
                client.generation,
                welcome.generation,
                &welcome.methods,
                &catalog
            ),
            vec!["machine.route".to_string()]
        );
    }

    #[test]
    fn disabled_actions_only_lists_gated_methods() {
        let required = gen2_catalog();
        assert_eq!(
            disabled_actions(1, &required),
            vec!["machine.route".to_string()]
        );
        assert_eq!(
            disabled_actions(0, &required),
            vec!["pane.list".to_string(), "machine.route".to_string()]
        );
        // Unknown future available generation keeps working for known methods.
        assert!(disabled_actions(99, &required).is_empty());
    }

    #[test]
    fn hello_tolerates_unknown_future_fields() {
        // Forward-compat rule: unknown fields are ignored, not rejected.
        let raw =
            r#"{"generation":1,"client_version":"x","cols":80,"rows":24,"future_field":"ok"}"#;
        let parsed: EndpointHello = serde_json::from_str(raw).expect("unknown fields ignored");
        assert_eq!(parsed.generation, 1);
        assert_eq!(parsed.min_generation, DEFAULT_MIN_GENERATION);
    }

    #[test]
    fn omitted_min_generation_defaults_to_floor_one() {
        let hello: EndpointHello =
            serde_json::from_str(include_str!("../../tests/fixtures/endpoint-hello-v1.json"))
                .expect("hello fixture parses");
        assert_eq!(hello.min_generation, DEFAULT_MIN_GENERATION);
        let welcome: EndpointWelcome = serde_json::from_str(include_str!(
            "../../tests/fixtures/endpoint-welcome-v1.json"
        ))
        .expect("welcome fixture parses");
        assert_eq!(welcome.min_generation, DEFAULT_MIN_GENERATION);
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
