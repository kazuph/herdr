//! Existing fork dialogs operate on qualified endpoint facts, never local runtime state.
use super::*;
use crate::api::schema as api;
use crate::app::NavigateAction;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

#[derive(Clone)]
pub(super) struct Target {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    workspace: String,
    cwd: String,
}

impl Target {
    fn current(&self, frontend: &ClientFrontend) -> bool {
        frontend.runtime.shell.active_endpoint_id == self.endpoint
            && frontend
                .runtime
                .shell
                .endpoint(&self.endpoint)
                .is_some_and(|endpoint| {
                    endpoint
                        .cache
                        .live_snapshot(self.generation)
                        .is_some_and(|snapshot| snapshot.boot_id == self.boot)
                })
    }
}

pub(super) enum Modal {
    Rename(Rename),
    Confirm(Confirm),
    NewTabQuery { target: Target, request_id: String },
}

pub(super) struct Rename {
    target: Target,
    kind: RenameKind,
    input: String,
    replace_on_type: bool,
}

enum RenameKind {
    Workspace,
    Tab {
        id: String,
        auto_name: Option<String>,
    },
    Pane {
        id: String,
    },
    NewTab {
        default_name: String,
    },
}

pub(super) struct Confirm {
    target: Target,
    title: String,
    detail: String,
}

impl Modal {
    fn target(&self) -> &Target {
        match self {
            Self::Rename(value) => &value.target,
            Self::Confirm(value) => &value.target,
            Self::NewTabQuery { target, .. } => target,
        }
    }
    pub(super) fn hides_terminal_cursor(&self) -> bool {
        matches!(self, Self::Rename(_) | Self::Confirm(_))
    }
    pub(super) fn current(&self, frontend: &ClientFrontend) -> bool {
        self.target().current(frontend)
    }
    pub(super) fn render(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        terminal_area: ratatui::layout::Rect,
        palette: &crate::app::state::Palette,
    ) {
        match self {
            Self::Rename(value) => crate::ui::render_rename_dialog(
                frame,
                area,
                match value.kind {
                    RenameKind::Workspace => "rename workspace",
                    RenameKind::Tab { .. } => "rename tab",
                    RenameKind::Pane { .. } => "rename pane",
                    RenameKind::NewTab { .. } => "new tab",
                },
                &value.input,
                palette,
            ),
            Self::Confirm(value) => crate::ui::render_confirm_close_dialog(
                frame,
                terminal_area,
                &value.title,
                &value.detail,
                palette,
            ),
            Self::NewTabQuery { .. } => {}
        }
    }
}

fn issue(frontend: &mut ClientFrontend, method: api::Method) -> io::Result<()> {
    match frontend.runtime.issue_method(method) {
        Ok(update) => {
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn action(frontend: &mut ClientFrontend, action: NavigateAction) -> io::Result<bool> {
    if !matches!(
        action,
        NavigateAction::NewWorkspace
            | NavigateAction::RenameWorkspace
            | NavigateAction::CloseWorkspace
            | NavigateAction::NewTab
            | NavigateAction::RenameTab
            | NavigateAction::CloseTab
            | NavigateAction::RenamePane
            | NavigateAction::ClosePane
    ) {
        return Ok(false);
    }
    if !frontend.runtime.input_lease_current() {
        return Ok(true);
    }
    let endpoint = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .expect("active lease endpoint");
    let generation = endpoint.generation.expect("active lease generation");
    let snapshot = endpoint
        .cache
        .live_snapshot(generation)
        .expect("active lease snapshot")
        .clone();
    let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| Some(&workspace.workspace_id) == snapshot.focused_workspace_id.as_ref())
    else {
        if action == NavigateAction::NewWorkspace {
            issue(
                frontend,
                api::Method::WorkspaceCreate(api::WorkspaceCreateParams {
                    section: None,
                    cwd: None,
                    focus: true,
                    label: None,
                    env: Default::default(),
                }),
            )?;
        }
        return Ok(true);
    };
    let target = Target {
        endpoint: endpoint.endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        workspace: workspace.workspace_id.clone(),
        cwd: workspace.new_workspace_cwd.clone(),
    };
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| Some(&tab.tab_id) == snapshot.focused_tab_id.as_ref());
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| Some(&pane.pane_id) == snapshot.focused_pane_id.as_ref());
    let rename = |kind, input, replace_on_type| {
        Modal::Rename(Rename {
            target: target.clone(),
            kind,
            input,
            replace_on_type,
        })
    };
    match action {
        NavigateAction::NewWorkspace => issue(
            frontend,
            api::Method::WorkspaceCreate(api::WorkspaceCreateParams {
                section: None,
                cwd: Some(target.cwd),
                focus: true,
                label: None,
                env: Default::default(),
            }),
        )?,
        NavigateAction::RenameWorkspace => {
            frontend.modal = Some(rename(
                RenameKind::Workspace,
                workspace.label.clone(),
                false,
            ))
        }
        NavigateAction::RenameTab => {
            if let Some(tab) = tab {
                frontend.modal = Some(rename(
                    RenameKind::Tab {
                        id: tab.tab_id.clone(),
                        auto_name: (!tab.custom_label).then(|| tab.label.clone()),
                    },
                    tab.label.clone(),
                    false,
                ));
            }
        }
        NavigateAction::RenamePane => {
            if let Some(pane) = pane {
                frontend.modal = Some(rename(
                    RenameKind::Pane {
                        id: pane.pane_id.clone(),
                    },
                    pane.label.clone().unwrap_or_default(),
                    pane.label.is_none(),
                ));
            }
        }
        NavigateAction::NewTab if frontend.prompt_new_tab_name => {
            match frontend
                .runtime
                .issue_method_with_id(api::Method::WorkspaceGet(api::WorkspaceTarget {
                    workspace_id: target.workspace.clone(),
                })) {
                Ok((request_id, update)) => {
                    frontend.modal = Some(Modal::NewTabQuery { target, request_id });
                    frontend.update(update)?;
                }
                Err(error) => frontend.notice = Some(error),
            }
        }
        NavigateAction::NewTab => issue(frontend, new_tab(&target, None))?,
        NavigateAction::CloseWorkspace | NavigateAction::CloseTab | NavigateAction::ClosePane => {
            let tab_count = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == target.workspace)
                .count();
            let pane_count = tab.map_or(0, |tab| {
                snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.tab_id == tab.tab_id)
                    .count()
            });
            let closes_workspace = action == NavigateAction::CloseWorkspace
                || action == NavigateAction::CloseTab && tab_count <= 1
                || action == NavigateAction::ClosePane && tab_count <= 1 && pane_count <= 1;
            let members: Vec<_> = workspace
                .worktree
                .as_ref()
                .filter(|space| !space.is_linked_worktree)
                .map(|space| {
                    snapshot
                        .workspaces
                        .iter()
                        .filter(|member| {
                            member
                                .worktree
                                .as_ref()
                                .is_some_and(|worktree| worktree.key == space.key)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let group = members.len() > 1;
            if closes_workspace
                && frontend.confirm_close
                && (action == NavigateAction::CloseWorkspace || group)
            {
                let count = if group {
                    members
                        .iter()
                        .map(|workspace| {
                            snapshot
                                .panes
                                .iter()
                                .filter(|pane| {
                                    pane.workspace_id == workspace.workspace_id
                                        && pane.tab_id == workspace.active_tab_id
                                })
                                .count()
                        })
                        .sum()
                } else {
                    pane_count
                };
                let pane_text = if count == 1 {
                    "1 pane".into()
                } else {
                    format!("{count} panes")
                };
                let workspace_text = if group {
                    format!("{} workspaces, ", members.len())
                } else {
                    String::new()
                };
                frontend.modal = Some(Modal::Confirm(Confirm {
                    target,
                    title: if group {
                        "Close worktree group?"
                    } else {
                        "Close workspace?"
                    }
                    .into(),
                    detail: format!("{} — {workspace_text}{pane_text}", workspace.label),
                }));
            } else if closes_workspace {
                issue(
                    frontend,
                    api::Method::WorkspaceClose(api::WorkspaceTarget {
                        workspace_id: target.workspace,
                    }),
                )?;
            } else if action == NavigateAction::CloseTab {
                if let Some(tab) = tab {
                    issue(
                        frontend,
                        api::Method::TabClose(api::TabTarget {
                            tab_id: tab.tab_id.clone(),
                        }),
                    )?;
                }
            } else if let Some(pane) = pane {
                issue(
                    frontend,
                    api::Method::PaneClose(api::PaneTarget {
                        pane_id: pane.pane_id.clone(),
                    }),
                )?;
            }
        }
        _ => {}
    }
    Ok(true)
}

fn new_tab(target: &Target, label: Option<String>) -> api::Method {
    api::Method::TabCreate(api::TabCreateParams {
        workspace_id: Some(target.workspace.clone()),
        cwd: Some(target.cwd.clone()),
        focus: true,
        label,
        env: Default::default(),
    })
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    completed: &super::super::commands::EndpointCommandResult,
) {
    let Some(Modal::NewTabQuery { target, request_id }) = frontend.modal.as_ref() else {
        return;
    };
    if completed.endpoint_id != target.endpoint
        || completed.generation != target.generation
        || completed.boot_id != target.boot
        || completed.request_id != *request_id
    {
        return;
    }
    let target = target.clone();
    frontend.modal = None;
    if !target.current(frontend) {
        return;
    }
    let Ok(value) = &completed.result else {
        return;
    };
    let Ok(response) = serde_json::from_value::<api::ResponseResult>(value.clone()) else {
        return;
    };
    if let api::ResponseResult::WorkspaceInfo { workspace } = response {
        if workspace.workspace_id != target.workspace {
            return;
        }
        if let Some(number) = workspace.next_public_tab_number {
            let default_name = number.to_string();
            frontend.modal = Some(Modal::Rename(Rename {
                target,
                kind: RenameKind::NewTab {
                    default_name: default_name.clone(),
                },
                input: default_name,
                replace_on_type: true,
            }));
        }
    }
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained | RawInputEvent::OuterFocusLost
    ) {
        return Ok(false);
    }
    let Some(mut modal) = frontend.modal.take() else {
        return Ok(false);
    };
    if !modal.current(frontend) {
        return Ok(true);
    }
    let mut save = false;
    let mut cancel = false;
    match event {
        RawInputEvent::Key(raw) if raw.kind != KeyEventKind::Release => {
            let key = raw.as_key_event();
            cancel = key.code == KeyCode::Esc
                || matches!(modal, Modal::Confirm(_))
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL);
            save = key.code == KeyCode::Enter;
            if let Modal::Rename(value) = &mut modal {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    crate::input::rename::clear(&mut value.input, &mut value.replace_on_type);
                } else if !save && !cancel {
                    crate::input::rename::edit_key(
                        &mut value.input,
                        &mut value.replace_on_type,
                        key,
                    );
                }
            }
        }
        RawInputEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
            let area = ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows);
            let popup = match modal {
                Modal::Rename(_) => crate::ui::centered_popup_rect(area, 56, 7),
                Modal::Confirm(_) => {
                    let view = frontend.chrome.compute_view(
                        &frontend.runtime.shell,
                        frontend.cols,
                        frontend.rows,
                    );
                    crate::ui::confirm_close_popup_rect(view.layout.pane_surface)
                }
                Modal::NewTabQuery { .. } => None,
            };
            if let Some(popup) = popup {
                let inner = ratatui::layout::Rect::new(
                    popup.x + 1,
                    popup.y + 1,
                    popup.width.saturating_sub(2),
                    popup.height.saturating_sub(2),
                );
                let position = (mouse.column, mouse.row).into();
                match &mut modal {
                    Modal::Rename(value) => {
                        let (accept, clear, close) = crate::ui::rename_button_rects(inner);
                        save = accept.contains(position);
                        cancel = close.contains(position);
                        if clear.contains(position) {
                            crate::input::rename::clear(
                                &mut value.input,
                                &mut value.replace_on_type,
                            );
                        }
                    }
                    Modal::Confirm(_) => {
                        let (accept, close) = crate::ui::confirm_close_button_rects(inner);
                        save = accept.contains(position);
                        cancel = close.contains(position);
                    }
                    Modal::NewTabQuery { .. } => {}
                }
            }
        }
        RawInputEvent::Paste(text) => {
            if let Modal::Rename(value) = &mut modal {
                crate::input::rename::insert(&mut value.input, &mut value.replace_on_type, text);
            }
        }
        _ => {}
    }
    if matches!(modal, Modal::NewTabQuery { .. }) {
        save = false;
    }
    if save {
        let method = match modal {
            Modal::Confirm(value) => Some(api::Method::WorkspaceClose(api::WorkspaceTarget {
                workspace_id: value.target.workspace,
            })),
            Modal::Rename(value) => {
                let name = if value.input.trim().is_empty() {
                    value.input
                } else {
                    value.input.trim().to_string()
                };
                match value.kind {
                    RenameKind::Workspace => (!name.is_empty()).then_some({
                        api::Method::WorkspaceRename(api::WorkspaceRenameParams {
                            workspace_id: value.target.workspace,
                            label: name,
                        })
                    }),
                    RenameKind::Tab { id, auto_name } => {
                        (!name.is_empty() && auto_name.as_ref() != Some(&name)).then_some({
                            api::Method::TabRename(api::TabRenameParams {
                                tab_id: id,
                                label: name,
                            })
                        })
                    }
                    RenameKind::Pane { id } => {
                        Some(api::Method::PaneRename(api::PaneRenameParams {
                            pane_id: id,
                            label: Some(name),
                        }))
                    }
                    RenameKind::NewTab { default_name } => Some(new_tab(
                        &value.target,
                        (!name.is_empty() && name != default_name).then_some(name),
                    )),
                }
            }
            Modal::NewTabQuery { .. } => None,
        };
        if let Some(method) = method {
            issue(frontend, method)?;
        }
    } else if !cancel {
        frontend.modal = Some(modal);
    }
    Ok(true)
}

pub(super) fn graphics_rect(
    frontend: &ClientFrontend,
    terminal_area: ratatui::layout::Rect,
) -> Option<ratatui::layout::Rect> {
    match frontend.modal.as_ref()? {
        Modal::Rename(_) => crate::ui::centered_popup_rect(
            ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows),
            56,
            7,
        ),
        Modal::Confirm(_) => crate::ui::confirm_close_popup_rect(terminal_area),
        Modal::NewTabQuery { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::commands::EndpointCommandResult;
    use super::*;

    fn query() -> (ClientFrontend, Target) {
        let config = crate::config::Config::default();
        let snapshot: wire::ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        let target = Target {
            endpoint: ClientEndpointId::Local,
            generation: 3,
            boot: snapshot.boot_id.clone(),
            workspace: snapshot.focused_workspace_id.clone().unwrap(),
            cwd: "/endpoint-owned/opaque".into(),
        };
        let mut shell = ClientShellState::new();
        shell.begin_connection(&target.endpoint, target.generation);
        shell.receive_snapshot(&target.endpoint, target.generation, snapshot);
        let options = EndpointConnectOptions {
            surface_size: wire::ClientSurfaceSize { cols: 80, rows: 24 },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: false,
        };
        let runtime = EndpointRuntime::new(
            shell,
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        let mut frontend = ClientFrontend::from_runtime(
            runtime,
            &config,
            ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None),
            (80, 24),
            options,
        );
        frontend.modal = Some(Modal::NewTabQuery {
            target: target.clone(),
            request_id: "opaque-query".into(),
        });
        (frontend, target)
    }

    fn response(target: &Target) -> EndpointCommandResult {
        EndpointCommandResult {
            endpoint_id: target.endpoint.clone(),
            generation: target.generation,
            boot_id: target.boot.clone(),
            request_id: "opaque-query".into(),
            result: Ok(
                serde_json::json!({"type":"workspace_info","workspace":{"workspace_id":target.workspace,"number":1,"label":"owner","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"opaque-tab","agent_status":"unknown","next_public_tab_number":42}}),
            ),
        }
    }

    #[test]
    fn endpoint_new_tab_query_requires_every_qualified_identity_without_redirecting_selection() {
        for mismatch in 0..4 {
            let (mut frontend, target) = query();
            let mut result = response(&target);
            match mismatch {
                0 => result.endpoint_id = ClientEndpointId::Ssh("other-same-resource-id".into()),
                1 => result.generation += 1,
                2 => result.boot_id.push_str("-retired"),
                3 => result.request_id.push_str("-different"),
                _ => unreachable!(),
            }
            completed(&mut frontend, &result);
            assert!(matches!(frontend.modal, Some(Modal::NewTabQuery { .. })));
            assert_eq!(frontend.runtime.shell.active_endpoint_id, target.endpoint);
        }
        let (mut frontend, target) = query();
        completed(&mut frontend, &response(&target));
        let Some(Modal::Rename(rename)) = frontend.modal else {
            panic!("qualified counter prompt");
        };
        assert_eq!(rename.input, "42");
        assert!(rename.replace_on_type);
    }

    #[test]
    fn endpoint_new_tab_query_rejects_reconnect_selection_change_wrong_resource_and_missing_counter(
    ) {
        for mismatch in 0..4 {
            let (mut frontend, target) = query();
            let mut result = response(&target);
            match mismatch {
                0 => {
                    frontend
                        .runtime
                        .shell
                        .begin_connection(&target.endpoint, target.generation + 1);
                }
                1 => {
                    frontend.runtime.shell.active_endpoint_id =
                        ClientEndpointId::Ssh("selected-other".into())
                }
                2 => {
                    result.result.as_mut().unwrap()["workspace"]["workspace_id"] =
                        serde_json::json!("wrong-opaque-resource")
                }
                3 => {
                    result.result.as_mut().unwrap()["workspace"]
                        .as_object_mut()
                        .unwrap()
                        .remove("next_public_tab_number");
                }
                _ => unreachable!(),
            }
            let selected = frontend.runtime.shell.active_endpoint_id.clone();
            completed(&mut frontend, &result);
            assert!(frontend.modal.is_none());
            assert_eq!(frontend.runtime.shell.active_endpoint_id, selected);
        }
    }

    #[test]
    fn endpoint_dialogs_keep_host_focus_dispatch_and_hide_only_visible_modal_terminal_cursor() {
        let (mut frontend, target) = query();
        assert!(!frontend.modal.as_ref().unwrap().hides_terminal_cursor());
        assert!(!input(&mut frontend, &RawInputEvent::OuterFocusLost).unwrap());
        assert!(matches!(frontend.modal, Some(Modal::NewTabQuery { .. })));
        completed(&mut frontend, &response(&target));
        assert!(frontend.modal.as_ref().unwrap().hides_terminal_cursor());
        assert!(!input(&mut frontend, &RawInputEvent::OuterFocusGained).unwrap());
        assert!(matches!(frontend.modal, Some(Modal::Rename(_))));
    }
}
