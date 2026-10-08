//! Generation-1 projection with connection-owned selection and endpoint-owned resources.

use super::client_view::ClientShellLocation;
use crate::app::App;
use crate::protocol::endpoint_wire::{
    ClientShellAgent, ClientShellCommand, ClientShellPane, ClientShellSnapshot, ClientShellTab,
    ClientShellWorkspace, ClientShellWorktree,
};

pub(crate) fn snapshot(
    app: &App,
    boot_id: &str,
    revision: u64,
    diagnostic: Option<&str>,
    location: &ClientShellLocation,
    commands: Vec<ClientShellCommand>,
    resources: crate::api::schema::SessionSnapshot,
) -> crate::protocol::endpoint_projection::SnapshotJson {
    let focused_workspace_id = location.focused_workspace_id.clone();
    let focused_tab_id = location.focused_tab_id().map(str::to_owned);
    let focused_pane_id = location.focused_pane_id().map(str::to_owned);
    let workspaces = resources
        .workspaces
        .into_iter()
        .zip(&app.state.workspaces)
        .enumerate()
        .map(|(workspace_index, (resource, state))| {
            let workspace_id = resource.workspace_id;
            let active_tab_id = location
                .active_tab_ids
                .get(&workspace_id)
                .cloned()
                .unwrap_or(resource.active_tab_id);
            let follow_cwd = app
                .parse_tab_id(&active_tab_id)
                .filter(|(index, _)| *index == workspace_index)
                .and_then(|(_, tab_index)| {
                    let (_, pane) =
                        app.parse_pane_id(location.focused_pane_ids.get(&active_tab_id)?)?;
                    if state.find_tab_index_for_pane(pane) != Some(tab_index) {
                        return None;
                    }
                    app.follow_cwd_for_pane_in_workspace(workspace_index, pane)
                });
            let mut tokens = resource.tokens.into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            ClientShellWorkspace {
                focused: focused_workspace_id.as_deref() == Some(workspace_id.as_str()),
                workspace_id,
                active_tab_id,
                new_workspace_cwd: app
                    .resolve_new_terminal_cwd(follow_cwd)
                    .to_string_lossy()
                    .into_owned(),
                number: resource.number,
                label: resource.label,
                custom_label: state.custom_name.is_some(),
                branch: state.branch(),
                git_ahead_behind: state.git_ahead_behind(),
                tokens,
                worktree: resource.worktree.map(|worktree| ClientShellWorktree {
                    key: worktree.repo_key,
                    label: worktree.repo_name,
                    is_linked_worktree: worktree.is_linked_worktree,
                }),
                agent_status: resource.agent_status,
            }
        })
        .collect();
    let tabs = resources
        .tabs
        .into_iter()
        .filter_map(|resource| {
            let (workspace_index, tab_index) = app.parse_tab_id(&resource.tab_id)?;
            let state = app
                .state
                .workspaces
                .get(workspace_index)?
                .tabs
                .get(tab_index)?;
            Some(ClientShellTab {
                focused: focused_tab_id.as_deref() == Some(resource.tab_id.as_str()),
                tab_id: resource.tab_id,
                workspace_id: resource.workspace_id,
                number: resource.number,
                label: resource.label,
                custom_label: !state.is_auto_named(),
                zoomed: state.zoomed,
                agent_status: resource.agent_status,
            })
        })
        .collect();
    let panes = resources
        .panes
        .into_iter()
        .map(|resource| {
            // The fork has client-owned modifier passthrough, not a per-pane override.
            let right_click_passthrough = false;
            ClientShellPane {
                focused: focused_pane_id.as_deref() == Some(resource.pane_id.as_str()),
                pane_id: resource.pane_id,
                workspace_id: resource.workspace_id,
                tab_id: resource.tab_id,
                label: resource.label,
                cwd: resource.cwd,
                foreground_cwd: resource.foreground_cwd,
                right_click_passthrough,
            }
        })
        .collect();
    let agents = resources
        .agents
        .into_iter()
        .map(|resource| {
            let mut tokens = resource.tokens.into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            let mut state_labels = resource.state_labels.into_iter().collect::<Vec<_>>();
            state_labels.sort_by(|left, right| left.0.cmp(&right.0));
            ClientShellAgent {
                focused: focused_pane_id.as_deref() == Some(resource.pane_id.as_str()),
                pane_id: resource.pane_id,
                workspace_id: resource.workspace_id,
                tab_id: resource.tab_id,
                name: resource.name,
                display_agent: resource.display_agent,
                agent: resource.agent,
                title: resource.title,
                terminal_title: resource.terminal_title,
                terminal_title_stripped: resource.terminal_title_stripped,
                agent_status: resource.agent_status,
                state_change_seq: resource.state_change_seq,
                state_labels,
                tokens,
            }
        })
        .collect();
    let agent_order = crate::ui::agent_panel_entries_from(&app.state, &app.terminal_runtimes)
        .into_iter()
        .filter_map(|entry| app.public_pane_id(entry.ws_idx, entry.pane_id))
        .collect();
    let workspace_facts = app
        .state
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let git_space = state
                .git_space()
                .cloned()
                .or_else(|| {
                    state
                        .resolved_identity_cwd_from(&app.state.terminals, &app.terminal_runtimes)
                        .as_deref()
                        .and_then(crate::workspace::git_space_metadata)
                })
                .map(|space| ClientShellWorktree {
                    key: space.key,
                    label: space.label,
                    is_linked_worktree: space.is_linked_worktree,
                });
            (
                app.public_workspace_id(index),
                crate::protocol::endpoint_projection::WorkspaceFacts {
                    git_space,
                    section: Some(state.section),
                },
            )
        })
        .collect();
    let mut pane_facts = std::collections::BTreeMap::new();
    for (workspace_index, workspace) in app.state.workspaces.iter().enumerate() {
        for tab in &workspace.tabs {
            for pane_id in tab.layout.pane_ids() {
                let Some(pane) = tab.panes.get(&pane_id) else {
                    continue;
                };
                let Some(terminal) = app.state.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let Some(id) = app.public_pane_id(workspace_index, pane_id) else {
                    continue;
                };
                let Some(number) = workspace.public_pane_number(pane_id) else {
                    continue;
                };
                let presentation = terminal.effective_presentation();
                pane_facts.insert(
                    id,
                    crate::protocol::endpoint_projection::PaneFacts {
                        number,
                        effective_title: terminal.effective_title(),
                        title: presentation.title,
                        manual_label: terminal.manual_label.clone(),
                        agent_name: terminal.agent_name.clone(),
                        agent_label: terminal.effective_agent_label().map(str::to_owned),
                        display_agent: terminal.effective_display_agent(),
                        launch_argv: terminal.launch_argv.clone(),
                        state: match terminal.state {
                            crate::detect::AgentState::Idle => {
                                crate::api::schema::AgentStatus::Idle
                            }
                            crate::detect::AgentState::Working => {
                                crate::api::schema::AgentStatus::Working
                            }
                            crate::detect::AgentState::Blocked => {
                                crate::api::schema::AgentStatus::Blocked
                            }
                            crate::detect::AgentState::Unknown => {
                                crate::api::schema::AgentStatus::Unknown
                            }
                        },
                        seen: pane.seen,
                        state_labels: presentation.state_labels.into_iter().collect(),
                    },
                );
            }
        }
    }
    let snapshot = ClientShellSnapshot {
        boot_id: boot_id.to_owned(),
        revision,
        config_diagnostic: diagnostic.map(str::to_owned),
        // The fork deliberately removed upstream update and integration flows.
        product_announcement: None,
        update_available: None,
        update_install_command: app.state.update_install_command.clone(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: app.state.worktree_directory.to_string_lossy().into_owned(),
        release_notes: None,
        focused_workspace_id,
        focused_tab_id,
        focused_pane_id,
        // Fork tab chrome is composed from each tab's zoomed state.
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: app
            .state
            .agent_view_override
            .as_ref()
            .map(|view| view.label.clone().unwrap_or_else(|| "filtered".into())),
        agent_order,
        workspaces,
        tabs,
        panes,
        agents,
        commands,
    };
    let notification = app.state.toast.as_ref().map(|toast| {
        use crate::app::state::ToastKind;
        use crate::protocol::endpoint_wire::{SemanticNotification, SemanticNotificationKind};
        let target = toast.target.as_ref().and_then(|target| {
            let workspace_index = app
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == target.workspace_id)?;
            let pane_id = app.public_pane_id(workspace_index, target.pane_id)?;
            let tab_index =
                app.state.workspaces[workspace_index].find_tab_index_for_pane(target.pane_id)?;
            Some((
                target.workspace_id.clone(),
                app.public_tab_id(workspace_index, tab_index)?,
                pane_id,
            ))
        });
        SemanticNotification {
            kind: match toast.kind {
                ToastKind::NeedsAttention => SemanticNotificationKind::NeedsAttention,
                ToastKind::Finished => SemanticNotificationKind::Finished,
                ToastKind::UpdateInstalled => SemanticNotificationKind::UpdateInstalled,
            },
            title: toast.title.clone(),
            body: (!toast.context.is_empty()).then(|| toast.context.clone()),
            sound: None,
            agent: None,
            workspace_id: target.as_ref().map(|target| target.0.clone()),
            tab_id: target.as_ref().map(|target| target.1.clone()),
            pane_id: target.as_ref().map(|target| target.2.clone()),
            position: Some(
                toast
                    .position
                    .unwrap_or(app.state.toast_config.herdr.position),
            ),
        }
    });
    crate::protocol::endpoint_projection::SnapshotJson {
        snapshot,
        workspace_facts,
        pane_facts,
        notification,
    }
}
