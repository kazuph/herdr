pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub use server::{start_server, start_server_with_capabilities, ServerHandle};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::{Method, Request};

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";
pub const SOCKET_PATH_EXPLICIT_ENV_VAR: &str = "HERDR_SOCKET_PATH_EXPLICIT";

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    matches!(
        &request.method,
        Method::ServerPaneHistorySet(_)
            | Method::ServerReloadConfig(_)
            | Method::PaneScroll(_)
            | Method::PopupScroll(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::NotificationShow(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceDuplicate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceSetSection(_)
            | Method::WorkspaceMove(_)
            | Method::WorkspaceReportMetadata(_)
            | Method::WorkspaceClose(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabMove(_)
            | Method::TabClose(_)
            | Method::LayoutApply(_)
            | Method::LayoutSetSplitRatio(_)
            | Method::AgentRename(_)
            | Method::AgentFocus(_)
            | Method::AgentStart(_)
            | Method::AgentRestore(_)
            | Method::MsgSend(_)
            | Method::RunStart(_)
            | Method::RunLogOpen(_)
            | Method::PaneScrollbackEdit(_)
            | Method::PaneCommandExecute(_)
            | Method::PaneAgentStart(_)
            | Method::PaneSplit(_)
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneArrange(_)
            | Method::PaneZoom(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneFocus(_)
            | Method::PaneRename(_)
            | Method::PaneGraphicsSet(_)
            | Method::PaneGraphicsClear(_)
            | Method::PaneGraphicsStream(_)
            | Method::PaneGraphicsStreamSet(_)
            | Method::PaneGraphicsStreamDirect(_)
            | Method::PaneGraphicsStreamOpen(_)
            | Method::PaneGraphicsStreamClose(_)
            | Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
            | Method::PopupClose(_)
            | Method::PluginActionInvoke(_)
            | Method::PluginPaneOpen(_)
            | Method::PluginPaneFocus(_)
            | Method::PluginPaneClose(_)
    )
}

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    pub response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    pub stream_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}

/// The binary path handed to plugins as `HERDR_BIN_PATH`. A Homebrew
/// install runs from a versioned Cellar path (`…/Cellar/herdr/<ver>/bin/herdr`)
/// that `brew upgrade` deletes, which would break every plugin still
/// spawning the recorded path — the version-agnostic `opt` symlink (then
/// the `bin` symlink) is substituted instead.
pub fn herdr_bin_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(stable_herdr_bin_path(&exe).display().to_string())
}

fn stable_herdr_bin_path(exe: &std::path::Path) -> PathBuf {
    let mut parts = exe.iter();
    let mut prefix = PathBuf::new();
    while let Some(part) = parts.next() {
        if part == "Cellar" {
            // <prefix>/Cellar/<name>/<version>/bin/<bin>
            let name = parts.next();
            let bin = exe.file_name();
            if let (Some(name), Some(bin)) = (name, bin) {
                for candidate in [
                    prefix.join("opt").join(name).join("bin").join(bin),
                    prefix.join("bin").join(bin),
                ] {
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
            break;
        }
        prefix.push(part);
    }
    exe.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::stable_herdr_bin_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn cellar_path_prefers_opt_symlink() {
        let root = std::env::temp_dir().join(format!("herdr-brew-{}", std::process::id()));
        let cellar = root.join("Cellar/herdr/0.2.35/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        let exe = cellar.join("herdr");
        std::fs::write(&exe, b"bin").unwrap();
        let opt = root.join("opt/herdr/bin");
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::write(opt.join("herdr"), b"opt").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/herdr"), b"bin").unwrap();
        assert_eq!(
            stable_herdr_bin_path(&exe),
            root.join("opt/herdr/bin/herdr")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cellar_path_falls_back_to_prefix_bin_then_self() {
        let root = std::env::temp_dir().join(format!("herdr-brew2-{}", std::process::id()));
        let cellar = root.join("Cellar/herdr/0.2.35/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        let exe = cellar.join("herdr");
        std::fs::write(&exe, b"bin").unwrap();
        // No opt symlink: the unversioned <prefix>/bin/herdr wins.
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/herdr"), b"bin").unwrap();
        assert_eq!(stable_herdr_bin_path(&exe), root.join("bin/herdr"));
        std::fs::remove_file(root.join("bin/herdr")).unwrap();
        // Neither exists: keep the original executable path.
        assert_eq!(stable_herdr_bin_path(&exe), exe);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn non_cellar_path_is_unchanged() {
        let exe = PathBuf::from("/Users/x/.local/bin/herdr");
        assert_eq!(stable_herdr_bin_path(&exe), exe);
        let exe = Path::new("/opt/homebrew/bin/herdr").to_path_buf();
        assert_eq!(stable_herdr_bin_path(&exe), exe);
    }
}
