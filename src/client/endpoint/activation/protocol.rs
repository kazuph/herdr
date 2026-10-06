// Semantic adapters for fixed upstream 5da0a01e activation, preserving the fork CLI schema.
use serde_json::Value;

use super::super::commands::{EndpointCommandError, EndpointCommandRequest};
use super::super::shell::ClientShellState;
use super::super::{ClientEndpointId, EndpointRegistry, EndpointSendOutcome, FocusTarget};
use super::model::{ActivationEvidence, EndpointLease};
use crate::protocol::endpoint_wire::{ClientMessage, ClientSurfaceSize, PaneSurfaceFrame};

pub(super) fn focus_result_matches(focus: Option<&FocusTarget>, result: &Value) -> bool {
    let (kind, field, id_field, expected) = match focus {
        Some(FocusTarget::Workspace(id)) => ("workspace_info", "workspace", "workspace_id", id),
        Some(FocusTarget::Tab(id)) => ("tab_info", "tab", "tab_id", id),
        Some(FocusTarget::Pane(id)) => ("pane_info", "pane", "pane_id", id),
        #[cfg(windows)]
        Some(FocusTarget::Notification { pane_id, .. }) => {
            ("pane_info", "pane", "pane_id", pane_id)
        }
        None => return false,
    };
    result["type"].as_str() == Some(kind)
        && result[field][id_field].as_str() == Some(expected.as_str())
        && result[field]["focused"].as_bool() == Some(true)
}

pub(super) fn endpoint_lease(
    shell: &ClientShellState,
    endpoints: &EndpointRegistry,
    endpoint_id: &ClientEndpointId,
) -> Result<EndpointLease, String> {
    let connection = endpoints
        .connection(endpoint_id)
        .ok_or_else(|| "endpoint connection is unavailable".to_owned())?;
    let (boot_id, minimum_revision) = shell
        .endpoint_snapshot_identity(endpoint_id, connection.generation)
        .ok_or_else(|| "endpoint metadata is not ready for this connection".to_owned())?;
    Ok(EndpointLease {
        endpoint_id: endpoint_id.clone(),
        generation: connection.generation,
        boot_id: boot_id.to_owned(),
        minimum_revision,
    })
}

pub(super) fn disconnected_endpoint_lease(
    shell: &ClientShellState,
    endpoint_id: &ClientEndpointId,
) -> EndpointLease {
    EndpointLease {
        endpoint_id: endpoint_id.clone(),
        generation: 0,
        boot_id: shell
            .endpoint_boot_id(endpoint_id)
            .unwrap_or_default()
            .to_owned(),
        minimum_revision: 0,
    }
}

pub(super) fn endpoint_matches(
    lease: &EndpointLease,
    endpoint_id: &ClientEndpointId,
    generation: u64,
    boot_id: &str,
) -> bool {
    lease.endpoint_id == *endpoint_id && lease.generation == generation && lease.boot_id == boot_id
}

pub(super) fn coherent_completion_surface(
    shell: &ClientShellState,
    lease: &EndpointLease,
    evidence: &ActivationEvidence,
    acknowledgement_revision: Option<u64>,
    geometry: ClientSurfaceSize,
) -> Result<PaneSurfaceFrame, String> {
    let revision = acknowledgement_revision.ok_or_else(|| {
        "endpoint activation completed without a surface acknowledgement".to_owned()
    })?;
    let surface = evidence
        .surface
        .clone()
        .ok_or_else(|| "endpoint activation completed without a surface".to_owned())?;
    if surface.projection_revision < revision || !surface_matches_geometry(&surface, geometry) {
        return Err("endpoint activation lost its acknowledged surface evidence".into());
    }
    if !shell.endpoint_snapshot_matches(
        &lease.endpoint_id,
        lease.generation,
        &lease.boot_id,
        surface.projection_revision,
    ) || !shell.endpoint_surface_matches(&lease.endpoint_id, lease.generation, &surface)
    {
        return Err("endpoint activation lost its coherent snapshot/surface pair".into());
    }
    Ok(surface)
}

pub(super) fn resize_geometry(message: &ClientMessage) -> Option<ClientSurfaceSize> {
    match message {
        ClientMessage::ClientShellResize { surface_size, .. } => Some(*surface_size),
        _ => None,
    }
}

pub(super) fn surface_matches_geometry(
    surface: &PaneSurfaceFrame,
    geometry: ClientSurfaceSize,
) -> bool {
    surface.frame.width == geometry.cols
        && surface.frame.height == geometry.rows
        && surface.frame.cells.len() == usize::from(geometry.cols) * usize::from(geometry.rows)
}

pub(super) fn send_surface_activation(
    endpoints: &mut EndpointRegistry,
    target: &EndpointLease,
    request_id: String,
    resize: &ClientMessage,
    focused: bool,
) -> Result<(), String> {
    if endpoints.send_to(&target.endpoint_id, resize) != EndpointSendOutcome::Sent {
        return Err("endpoint resize could not be sent".into());
    }
    let request = surface_interest_request(&target.boot_id, request_id, true)
        .map_err(|error| error.to_string())?;
    if endpoints.send_to(&target.endpoint_id, &request) != EndpointSendOutcome::Sent {
        return Err("endpoint activation could not be sent".into());
    }
    if endpoints.send_to(
        &target.endpoint_id,
        &ClientMessage::ClientShellFocus { focused },
    ) != EndpointSendOutcome::Sent
    {
        return Err("endpoint focus baseline could not be sent".into());
    }
    Ok(())
}

pub(super) fn decode_endpoint_response(
    request_id: &str,
    data: &[u8],
) -> Result<Value, EndpointCommandError> {
    super::super::commands::parse_response(request_id, data)
}

pub(super) fn surface_set_revision(result: &Value, expected_active: bool) -> Result<u64, String> {
    if result["type"].as_str() == Some("client_shell_surface_set")
        && result["active"].as_bool() == Some(expected_active)
    {
        if let Some(revision) = result["projection_revision"].as_u64() {
            return Ok(revision);
        }
    }
    Err("surface activation returned an invalid acknowledgement".into())
}

pub(super) fn focus_request(
    boot_id: &str,
    request_id: String,
    focus: &FocusTarget,
) -> std::io::Result<ClientMessage> {
    use crate::api::schema::{Method, PaneTarget, Request, TabTarget, WorkspaceTarget};
    let method = match focus {
        FocusTarget::Workspace(id) => Method::WorkspaceFocus(WorkspaceTarget {
            workspace_id: id.clone(),
        }),
        FocusTarget::Tab(id) => Method::TabFocus(TabTarget { tab_id: id.clone() }),
        FocusTarget::Pane(id) => Method::PaneFocus(
            PaneTarget {
                pane_id: id.clone(),
            }
            .into(),
        ),
        #[cfg(windows)]
        FocusTarget::Notification {
            pane_id,
            boot_id: expected,
        } => {
            if boot_id != expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "notification target restarted",
                ));
            }
            Method::PaneFocus(
                PaneTarget {
                    pane_id: pane_id.clone(),
                }
                .into(),
            )
        }
    };
    let request = EndpointCommandRequest::try_from(Request {
        id: request_id,
        method,
    })
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    endpoint_request(boot_id, request)
}

pub(super) fn surface_interest_request(
    boot_id: &str,
    request_id: String,
    active: bool,
) -> std::io::Result<ClientMessage> {
    endpoint_request(
        boot_id,
        EndpointCommandRequest {
            id: request_id,
            method: "client_shell.surface.set".into(),
            params: serde_json::json!({"active": active}),
        },
    )
}

fn endpoint_request(
    boot_id: &str,
    request: EndpointCommandRequest,
) -> std::io::Result<ClientMessage> {
    Ok(ClientMessage::ClientShellEndpointRequest {
        boot_id: boot_id.to_owned(),
        request: serde_json::to_string(&request)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
    })
}
