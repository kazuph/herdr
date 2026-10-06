//! Host images follow the selected runtime owner, never a remote path read on the host.
use super::*;

pub(super) fn input(frontend: &mut ClientFrontend, bytes: &[u8]) -> bool {
    if !frontend.runtime.input_lease_current()
        || frontend.prefix
        || frontend.ascii_realm
        || frontend.context.is_some()
        || frontend.menu.is_some()
        || frontend.notes.is_some()
        || frontend.settings.is_some()
        || frontend.help.is_some()
        || frontend.resize_mode.is_some()
        || frontend.worktrees.is_some()
        || frontend.modal.is_some()
        || frontend.mobile.is_some()
        || frontend.navigator.is_some()
    {
        return false;
    }
    let endpoint = &frontend.runtime.shell.active_endpoint_id;
    let remote = super::super::super::is_remote_client_process() || !endpoint.is_local();
    if !remote {
        return false;
    }
    let (key, popup) = if let Some(key) = input::popup_target(frontend) {
        (key, true)
    } else if let Some(key) = input::focused_pane(frontend) {
        (key, false)
    } else {
        return false;
    };
    if copy::active(frontend) && (!popup || copy::search_active(frontend)) {
        return false;
    }
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
