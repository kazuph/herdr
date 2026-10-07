//! Host input-source transitions reuse the fork's existing presentation-mode policy.
use super::*;
use crate::app::Mode;
use crate::platform::PrefixInputSource;

fn mode(frontend: &ClientFrontend) -> Mode {
    if frontend.machines.is_some() {
        return Mode::RenameTab;
    }
    if let Some(modal) = &frontend.modal {
        return match modal {
            modal::Modal::Confirm(_) => Mode::ConfirmClose,
            modal::Modal::Rename(_) | modal::Modal::NewTabQuery { .. } => Mode::RenameTab,
        };
    }
    if let Some(menu) = &frontend.menu {
        return menu.mode();
    }
    if frontend.context.is_some() {
        return Mode::ContextMenu;
    }
    if frontend.notes.is_some() {
        return Mode::ReleaseNotes;
    }
    if frontend.settings.is_some() {
        return Mode::Settings;
    }
    if frontend.help.is_some() {
        return Mode::KeybindHelp;
    }
    if frontend.resize_mode.is_some() {
        return Mode::Resize;
    }
    if frontend.navigator.is_some() {
        return Mode::Navigator;
    }
    if frontend.mobile.is_some() {
        return Mode::Navigate;
    }
    if copy::active(frontend) {
        return Mode::Copy;
    }
    if frontend.prefix {
        return Mode::Prefix;
    }
    Mode::Terminal
}

pub(super) fn sync(frontend: &mut ClientFrontend) {
    let realm = mode(frontend).wants_ascii_input()
        && frontend.runtime.shell.outer_focused != Some(false)
        && frontend.runtime.input_lease_current()
        && !frontend.detach_requested;
    match (frontend.ascii_realm, realm) {
        (false, true) if frontend.host_settings.prefix_ascii => {
            frontend.prefix_input_source.switch_to_ascii();
        }
        // Always restore on realm exit, including a flag disabled during the interaction.
        (true, false) => frontend.prefix_input_source.restore(),
        _ => {}
    }
    frontend.ascii_realm = realm;
}
