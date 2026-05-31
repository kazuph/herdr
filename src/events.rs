//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::workspace::WorkspaceGitStatus;

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub result: Result<(), String>,
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A pane's child process exited.
    PaneDied { pane_id: PaneId },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    #[allow(dead_code)]
    // Update fetching is disabled in this fork; tests keep UI behavior covered.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite { content: Vec<u8> },
    /// A pane child emitted an OSC window/icon title update.
    PaneTitleChanged {
        pane_id: PaneId,
        title: Option<String>,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed { results: Vec<WorkspaceGitStatus> },
    /// A background `git worktree add` command finished.
    WorktreeAddFinished(WorktreeAddResult),
    /// A background `git worktree remove` command finished.
    WorktreeRemoveFinished(WorktreeRemoveResult),
}
