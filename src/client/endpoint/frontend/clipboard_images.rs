//! Host images follow the selected runtime owner, never a remote path read on the host.
use super::*;

/// The pane (or popup) a host clipboard paste goes to, when the screen shows
/// a saved machine's pane and nothing in this client is taking the input.
fn remote_paste_target(
    frontend: &ClientFrontend,
) -> Option<(crate::client::endpoint::ResourceKey, bool)> {
    if !frontend.runtime.input_lease_current()
        || frontend.prefix
        || frontend.ascii_realm
        || frontend.context.is_some()
        || frontend.menu.is_some()
        || frontend.machines.is_some()
        || frontend.notes.is_some()
        || frontend.settings.is_some()
        || frontend.help.is_some()
        || frontend.resize_mode.is_some()
        || frontend.worktrees.is_some()
        || frontend.modal.is_some()
        || frontend.mobile.is_some()
        || frontend.navigator.is_some()
    {
        return None;
    }
    let endpoint = &frontend.runtime.shell.active_endpoint_id;
    let remote = super::super::super::is_remote_client_process() || !endpoint.is_local();
    if !remote {
        return None;
    }
    let (key, popup) = if let Some(key) = input::popup_target(frontend) {
        (key, true)
    } else {
        (input::focused_pane(frontend)?, false)
    };
    if copy::active(frontend) && (!popup || copy::search_active(frontend)) {
        return None;
    }
    Some((key, popup))
}

pub(super) fn input(frontend: &mut ClientFrontend, bytes: &[u8]) -> bool {
    let Some((key, popup)) = remote_paste_target(frontend) else {
        return false;
    };
    let remote = true;
    let image = if super::super::super::should_bridge_clipboard_image_paste(
        bytes,
        remote,
        frontend.remote_image_paste_key,
    ) {
        crate::platform::read_clipboard_image()
    } else {
        None
    }
    .or_else(|| super::super::super::read_image_file_from_terminal_drop(bytes, remote));
    let Some(image) = image else {
        return false;
    };
    if image.bytes.len() > crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD {
        tracing::warn!(
            bytes = image.bytes.len(),
            "local clipboard image is too large to bridge"
        );
        return true;
    }
    frontend.runtime.clipboard_image(&key, popup, image);
    true
}

/// Cmd+V while a saved machine's pane is shown. With the kitty keyboard
/// "report all keys" mode on (Codex and other kitty-keyboard programs turn it
/// on), Ghostty sends Cmd+V as a key instead of pasting, and a program on the
/// other machine would read that machine's clipboard. Paste this machine's
/// clipboard instead: an image goes through the image bridge, text as a
/// bracketed paste. The key, including its release, is consumed either way.
pub(super) fn host_paste_key(
    frontend: &mut ClientFrontend,
    key: crate::input::TerminalKey,
) -> bool {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if !matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        || key.modifiers != KeyModifiers::SUPER
    {
        return false;
    }
    let Some((target, popup)) = remote_paste_target(frontend) else {
        return false;
    };
    if key.kind != KeyEventKind::Press {
        return true;
    }
    if let Some(image) = crate::platform::read_clipboard_image() {
        if image.bytes.len() > crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD {
            tracing::warn!(
                bytes = image.bytes.len(),
                "local clipboard image is too large to bridge"
            );
        } else {
            frontend.runtime.clipboard_image(&target, popup, image);
        }
        return true;
    }
    let Some(text) = crate::platform::read_clipboard_text().filter(|text| !text.is_empty()) else {
        return true;
    };
    let event = wire::ClientPaneInputEvent::Paste(text);
    if popup {
        frontend.runtime.popup_input(&target, vec![event]);
    } else {
        frontend.runtime.input(&target, vec![event]);
    }
    true
}
