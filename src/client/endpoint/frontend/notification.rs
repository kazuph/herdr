//! Notification presentation stays client-local; targets retain their server identity.
use super::*;
use crate::app::state::{ToastKind, ToastNotification};
use ratatui::layout::Rect;

#[derive(Default)]
pub(super) struct Notifications {
    viewers: std::collections::BTreeMap<ClientEndpointId, (u64, String, u64)>,
    dismissed:
        std::collections::BTreeMap<ClientEndpointId, (u64, String, wire::SemanticNotification)>,
}

fn current(
    frontend: &ClientFrontend,
) -> Option<(ClientEndpointId, u64, String, wire::SemanticNotification)> {
    if frontend.host_settings.toast != crate::config::ToastDelivery::Herdr {
        return None;
    }
    let id = &frontend.runtime.shell.active_endpoint_id;
    let endpoint = frontend.runtime.shell.endpoint(id)?;
    let generation = endpoint.generation?;
    let snapshot = endpoint.cache.live_snapshot(generation)?;
    let notification = endpoint.cache.notification(generation)?;
    if frontend
        .notifications
        .dismissed
        .get(id)
        .is_some_and(|(gen, boot, fact)| {
            *gen == generation && *boot == snapshot.boot_id && fact == notification
        })
    {
        return None;
    }
    Some((
        id.clone(),
        generation,
        snapshot.boot_id.clone(),
        notification.clone(),
    ))
}

fn toast(notification: &wire::SemanticNotification) -> ToastNotification {
    ToastNotification {
        kind: match notification.kind {
            wire::SemanticNotificationKind::NeedsAttention => ToastKind::NeedsAttention,
            wire::SemanticNotificationKind::Finished => ToastKind::Finished,
            wire::SemanticNotificationKind::UpdateInstalled
            | wire::SemanticNotificationKind::Custom => ToastKind::UpdateInstalled,
        },
        title: notification.title.clone(),
        context: notification.body.clone().unwrap_or_default(),
        position: notification.position,
        // Rendering borrows text and kind; opaque navigation is resolved separately.
        target: None,
    }
}

pub(super) fn rect(frontend: &ClientFrontend) -> Option<Rect> {
    let (_, _, _, fact) = current(frontend)?;
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    let warning = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .and_then(|endpoint| {
            endpoint
                .generation
                .and_then(|generation| endpoint.cache.live_snapshot(generation))
        })
        .is_some_and(|snapshot| snapshot.config_diagnostic.is_some());
    if frontend.cols <= frontend.chrome.settings.mobile_width_threshold {
        Some(crate::ui::mobile_toast_banner_rect(area, warning))
    } else {
        let position = fact.position?;
        Some(crate::ui::toast_notification_rect(
            area,
            &toast(&fact),
            warning,
            position,
        ))
    }
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some((_, _, _, fact)) = current(frontend) else {
        return;
    };
    if rect(frontend).is_none() {
        return;
    }
    let toast = toast(&fact);
    let palette = &frontend.chrome.settings.palette;
    // Shared renderers compute their original geometry from the full area.
    let area = frame.area();
    let warning = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .and_then(|endpoint| {
            endpoint
                .generation
                .and_then(|generation| endpoint.cache.live_snapshot(generation))
        })
        .is_some_and(|snapshot| snapshot.config_diagnostic.is_some());
    if frontend.cols <= frontend.chrome.settings.mobile_width_threshold {
        crate::ui::render_mobile_toast_banner(frame, area, &toast, warning, palette);
    } else if let Some(position) = fact.position {
        crate::ui::render_toast_notification(frame, area, &toast, warning, position, palette);
    }
}

pub(super) fn open(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some((id, generation, boot, fact)) = current(frontend) else {
        return Ok(());
    };
    if !frontend.runtime.input_lease_current() {
        return Ok(());
    }
    let Some(snapshot) = frontend
        .runtime
        .shell
        .endpoint(&id)
        .and_then(|endpoint| endpoint.cache.live_snapshot(generation))
    else {
        return Ok(());
    };
    let Some(pane) = fact.pane_id.as_ref().and_then(|pane_id| {
        snapshot.panes.iter().find(|pane| {
            &pane.pane_id == pane_id
                && fact.workspace_id.as_ref() == Some(&pane.workspace_id)
                && fact.tab_id.as_ref() == Some(&pane.tab_id)
        })
    }) else {
        return Ok(());
    };
    let pane_id = pane.pane_id.clone();
    let update = frontend.runtime.activate(
        id.clone(),
        Some(super::super::FocusTarget::Pane(pane_id)),
        Instant::now(),
    );
    if update.error.is_none() {
        frontend
            .notifications
            .dismissed
            .insert(id, (generation, boot, fact));
        frontend.mobile = None;
        frontend.chrome.navigate_selection = None;
    }
    frontend.update(update)?;
    Ok(())
}

pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    mouse: crossterm::event::MouseEvent,
) -> io::Result<bool> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if frontend.mobile.is_some() || frontend.navigator.is_some() {
        return Ok(false);
    }
    let Some((_, _, _, fact)) = current(frontend) else {
        return Ok(false);
    };
    if fact.pane_id.is_none()
        || !rect(frontend).is_some_and(|rect| rect.contains((mouse.column, mouse.row).into()))
    {
        return Ok(false);
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            open(frontend)?;
            Ok(true)
        }
        MouseEventKind::Up(MouseButton::Left) => Ok(true),
        _ => Ok(false),
    }
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    frontend
        .notifications
        .dismissed
        .retain(|id, (generation, boot, fact)| {
            frontend.runtime.shell.endpoint(id).is_some_and(|endpoint| {
                endpoint
                    .cache
                    .live_snapshot(*generation)
                    .is_some_and(|snapshot| snapshot.boot_id == *boot)
                    && endpoint.cache.notification(*generation) == Some(fact)
            })
        });
}

fn endpoint_name(id: &ClientEndpointId) -> &str {
    match id {
        ClientEndpointId::Local => "local",
        ClientEndpointId::Ssh(id) => id,
    }
}

pub(super) fn viewer_focus(
    frontend: &mut ClientFrontend,
    effect: &crate::client::endpoint::runtime::QualifiedHostEffect,
) -> io::Result<bool> {
    let wire::ServerMessage::EndpointControl { kind, data } = &effect.message else {
        return Ok(false);
    };
    if kind != crate::protocol::endpoint_projection::VIEWER_FOCUS_KIND {
        return Ok(false);
    }
    let Ok(target) = serde_json::from_str::<crate::api::schema::PaneFocusParams>(data) else {
        return Ok(true);
    };
    let Some(viewer) = target.viewer else {
        return Ok(true);
    };
    let valid = viewer.endpoint_id == endpoint_name(&effect.endpoint_id)
        && viewer.generation == effect.generation
        && frontend.notifications.viewers.get(&effect.endpoint_id)
            == Some(&(effect.generation, viewer.boot_id.clone(), viewer.client_id))
        && frontend
            .runtime
            .endpoints
            .accepts(&effect.endpoint_id, effect.generation)
        && frontend
            .runtime
            .shell
            .endpoint(&effect.endpoint_id)
            .and_then(|endpoint| endpoint.cache.live_snapshot(effect.generation))
            .is_some_and(|snapshot| {
                snapshot.boot_id == viewer.boot_id
                    && snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == target.pane_id)
            });
    if valid {
        let update = frontend.runtime.activate(
            effect.endpoint_id.clone(),
            Some(super::super::FocusTarget::Pane(target.pane_id)),
            Instant::now(),
        );
        frontend.update(update)?;
    }
    Ok(true)
}

fn viewer_focus_command(
    effect: &crate::client::endpoint::runtime::QualifiedHostEffect,
    boot: &str,
    viewer_id: u64,
    pane_id: &str,
) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let quote = super::super::super::shell_quote;
    let mut argv = vec![exe.to_string_lossy().into_owned()];
    if let ClientEndpointId::Ssh(profile) = &effect.endpoint_id {
        argv.extend(["--machine".into(), profile.clone()]);
    }
    argv.extend([
        "pane".into(),
        "focus".into(),
        pane_id.into(),
        "--viewer".into(),
        viewer_id.to_string(),
        "--boot".into(),
        boot.into(),
        "--endpoint".into(),
        endpoint_name(&effect.endpoint_id).into(),
        "--generation".into(),
        effect.generation.to_string(),
    ]);
    let socket = crate::api::socket_path();
    let socket = if socket.is_absolute() {
        socket
    } else {
        std::env::current_dir().ok()?.join(socket)
    };
    let mut env = vec![
        format!("HERDR_SOCKET_PATH={}", socket.to_string_lossy()),
        "HERDR_SOCKET_PATH_EXPLICIT=1".into(),
    ];
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        env.push(format!("XDG_CONFIG_HOME={}", config.to_string_lossy()));
    }
    if let Some(binary) = std::env::var_os("HERDR_MACHINE_REMOTE_BINARY") {
        env.push(format!(
            "HERDR_MACHINE_REMOTE_BINARY={}",
            binary.to_string_lossy()
        ));
    }
    Some(format!(
        "env {} {} >/dev/null 2>&1",
        env.iter().map(|v| quote(v)).collect::<Vec<_>>().join(" "),
        argv.iter().map(|v| quote(v)).collect::<Vec<_>>().join(" ")
    ))
}

pub(super) fn forwarded_effect(
    frontend: &mut ClientFrontend,
    effect: &crate::client::endpoint::runtime::QualifiedHostEffect,
) -> bool {
    let wire::ServerMessage::EndpointControl { kind, data } = &effect.message else {
        return false;
    };
    if kind != crate::protocol::endpoint_projection::FORWARDED_NOTIFICATION_KIND {
        return false;
    }
    let Ok(payload) =
        serde_json::from_str::<crate::protocol::endpoint_projection::ForwardedNotification>(data)
    else {
        return true;
    };
    let current = frontend.runtime.endpoints.active_id() == &effect.endpoint_id
        && frontend
            .runtime
            .endpoints
            .accepts(&effect.endpoint_id, effect.generation)
        && frontend
            .runtime
            .shell
            .endpoint(&effect.endpoint_id)
            .and_then(|endpoint| endpoint.cache.live_snapshot(effect.generation))
            .is_some_and(|snapshot| snapshot.boot_id == payload.boot_id);
    if current {
        if let Some(viewer) = payload.viewer_id {
            frontend.notifications.viewers.insert(
                effect.endpoint_id.clone(),
                (effect.generation, payload.boot_id.clone(), viewer),
            );
        }
        if let crate::protocol::ServerMessage::Notify {
            kind,
            message,
            body,
            target_pane_id,
        } = payload.notification
        {
            if kind == crate::protocol::NotifyKind::SystemToast && target_pane_id.is_some() {
                let command =
                    payload
                        .viewer_id
                        .zip(target_pane_id.as_deref())
                        .and_then(|(viewer, pane)| {
                            viewer_focus_command(effect, &payload.boot_id, viewer, pane)
                        });
                if let Some(command) = command {
                    if let Err(error) = crate::platform::show_desktop_notification_with_action(
                        &message,
                        body.as_deref(),
                        Some(&command),
                    ) {
                        tracing::warn!(%error, "failed to emit viewer notification");
                    }
                }
                return true;
            }
            let supported = matches!(
                kind,
                crate::protocol::NotifyKind::Toast | crate::protocol::NotifyKind::Sound
            ) || (kind == crate::protocol::NotifyKind::SystemToast
                && target_pane_id.is_none());
            if supported {
                let mut sound_config = frontend.host_settings.sound_config.clone();
                sound_config.enabled = frontend.host_settings.sound;
                super::super::super::handle_notify(
                    kind,
                    &message,
                    body.as_deref(),
                    None,
                    &sound_config,
                );
            }
        }
    }
    true
}
