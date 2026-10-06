//! Context presentation captures the owning endpoint and waits for its viewer lease.
use super::*;
use crate::app::state::{ContextMenuFacts, MenuListState};
use crate::app::{ContextMenuInput, NavigateAction};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Clone, Copy)]
enum Kind {
    Tab,
    Workspace { facts: ContextMenuFacts },
    Pane { facts: ContextMenuFacts },
}

#[derive(Clone)]
enum Action {
    Navigate(NavigateAction),
    Method(Box<crate::api::schema::Method>),
    ToggleGroup,
    ConfiguredAgent(&'static str),
}

pub(super) struct Context {
    kind: Kind,
    target: super::super::ResourceKey,
    generation: u64,
    boot: String,
    x: u16,
    y: u16,
    list: MenuListState,
    pending: Option<Action>,
    request: Option<String>,
    finishing: bool,
}

impl Context {
    fn current(&self, frontend: &ClientFrontend) -> bool {
        frontend
            .runtime
            .shell
            .endpoint(&self.target.endpoint)
            .filter(|endpoint| endpoint.generation == Some(self.generation))
            .and_then(|endpoint| endpoint.cache.live_snapshot(self.generation))
            .is_some_and(|snapshot| {
                snapshot.boot_id == self.boot
                    && match self.kind {
                        Kind::Tab => snapshot.tabs.iter().any(|tab| tab.tab_id == self.target.id),
                        Kind::Workspace { .. } => snapshot
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.workspace_id == self.target.id),
                        Kind::Pane { .. } => snapshot
                            .panes
                            .iter()
                            .any(|pane| pane.pane_id == self.target.id),
                    }
            })
    }
    fn facts(&self) -> ContextMenuFacts {
        match self.kind {
            Kind::Tab => ContextMenuFacts::Tab {},
            Kind::Workspace { facts } | Kind::Pane { facts } => facts,
        }
    }
    fn rect(&self, frontend: &ClientFrontend) -> Rect {
        crate::app::context_menu_rect_from(
            Rect::new(0, 0, frontend.cols, frontend.rows),
            self.x,
            self.y,
            &self.facts().items(),
        )
    }
}

pub(super) fn open_tab(
    frontend: &mut ClientFrontend,
    target: super::super::ResourceKey,
    x: u16,
    y: u16,
) {
    let Some(endpoint) = frontend.runtime.shell.endpoint(&target.endpoint) else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    if !snapshot.tabs.iter().any(|tab| tab.tab_id == target.id) {
        return;
    }
    let boot = snapshot.boot_id.clone();
    selection::clear(frontend);
    frontend.prefix = false;
    frontend.context = Some(Context {
        kind: Kind::Tab,
        target,
        generation,
        boot,
        x,
        y,
        list: MenuListState::new(0),
        pending: None,
        request: None,
        finishing: false,
    });
    frontend.force_redraw = true;
}

pub(super) fn open_workspace(
    frontend: &mut ClientFrontend,
    target: super::super::ResourceKey,
    x: u16,
    y: u16,
) {
    let Some(endpoint) = frontend.runtime.shell.endpoint(&target.endpoint) else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == target.id)
    else {
        return;
    };
    let collapsed = workspace.worktree.as_ref().is_some_and(|worktree| {
        frontend
            .chrome
            .collapsed_groups
            .contains(&(target.endpoint.clone(), worktree.key.clone()))
    });
    let git = endpoint
        .cache
        .workspace_facts(generation, &target.id)
        .and_then(|facts| facts.git_space.as_ref());
    let facts = workspace_facts(workspace, &snapshot.workspaces, git, collapsed);
    let boot = snapshot.boot_id.clone();
    selection::clear(frontend);
    frontend.prefix = false;
    frontend.context = Some(Context {
        kind: Kind::Workspace { facts },
        target,
        generation,
        boot,
        x,
        y,
        list: MenuListState::new(0),
        pending: None,
        request: None,
        finishing: false,
    });
    frontend.force_redraw = true;
}

fn workspace_facts(
    workspace: &wire::ClientShellWorkspace,
    workspaces: &[wire::ClientShellWorkspace],
    git: Option<&wire::ClientShellWorktree>,
    collapsed: bool,
) -> ContextMenuFacts {
    let managed = workspace.worktree.as_ref();
    if managed.is_none() && git.is_none_or(|space| space.is_linked_worktree) {
        return ContextMenuFacts::Workspace {};
    }
    let is_linked_worktree = managed
        .or(git)
        .is_some_and(|space| space.is_linked_worktree);
    let grouped = managed
        .filter(|space| !space.is_linked_worktree)
        .is_some_and(|space| {
            workspaces
                .iter()
                .filter(|candidate| {
                    candidate
                        .worktree
                        .as_ref()
                        .is_some_and(|member| member.key == space.key)
                })
                .count()
                >= 2
        });
    ContextMenuFacts::GitWorkspace {
        is_linked_worktree,
        has_worktree_children: grouped,
        collapsed: grouped && collapsed,
    }
}

pub(super) fn open_pane(
    frontend: &mut ClientFrontend,
    target: super::super::ResourceKey,
    x: u16,
    y: u16,
) {
    let Some(endpoint) = frontend.runtime.shell.endpoint(&target.endpoint) else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == target.id) else {
        return;
    };
    let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == pane.tab_id) else {
        return;
    };
    let facts = ContextMenuFacts::Pane {
        has_manual_label: pane.label.is_some(),
        has_layout_actions: snapshot
            .panes
            .iter()
            .filter(|p| p.tab_id == pane.tab_id)
            .count()
            > 1,
        is_zoomed: tab.zoomed,
    };
    let boot = snapshot.boot_id.clone();
    selection::clear(frontend);
    frontend.prefix = false;
    frontend.context = Some(Context {
        kind: Kind::Pane { facts },
        target,
        generation,
        boot,
        x,
        y,
        list: MenuListState::new(0),
        pending: None,
        request: None,
        finishing: false,
    });
    frontend.force_redraw = true;
}

pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(context) = frontend.context.as_ref() else {
        return Ok(());
    };
    tracing::debug!(
        finishing = context.finishing,
        request = ?context.request,
        lease_current = frontend.runtime.input_lease_current(),
        "context observes endpoint presentation"
    );
    if !context.current(frontend) {
        frontend.context = None;
        frontend.force_redraw = true;
        return Ok(());
    }
    if context.finishing {
        if frontend.runtime.input_lease_current() {
            frontend.context = None;
            frontend.force_redraw = true;
        }
        return Ok(());
    }
    if context.request.is_some() {
        return Ok(());
    }
    if let Some(action) = context.pending.clone() {
        let selected = frontend.runtime.shell.active_endpoint_id == context.target.endpoint
            && frontend
                .runtime
                .shell
                .endpoint(&context.target.endpoint)
                .and_then(|endpoint| endpoint.cache.live_snapshot(context.generation))
                .is_some_and(|snapshot| match context.kind {
                    Kind::Workspace { .. } => {
                        snapshot.focused_workspace_id.as_deref() == Some(context.target.id.as_str())
                    }
                    Kind::Tab => {
                        snapshot.focused_tab_id.as_deref() == Some(context.target.id.as_str())
                    }
                    Kind::Pane { .. } => {
                        snapshot.focused_pane_id.as_deref() == Some(context.target.id.as_str())
                    }
                });
        if selected && frontend.runtime.input_lease_current() {
            let action = if let Action::ConfiguredAgent(agent) = action {
                let Some(pane_id) = super::input::focused_pane(frontend).map(|pane| pane.id) else {
                    return Ok(());
                };
                Action::Method(Box::new(crate::api::schema::Method::PaneAgentStart(
                    crate::api::schema::PaneAgentStartParams {
                        pane_id,
                        agent: agent.to_string(),
                    },
                )))
            } else {
                action
            };
            match action {
                Action::ConfiguredAgent(_) => {
                    unreachable!("configured action converted to owner method")
                }
                Action::ToggleGroup => {
                    let target = context.target.clone();
                    toggle_group(frontend, &target);
                    frontend.context = None;
                }
                Action::Navigate(action) => {
                    frontend.context = None;
                    if !worktrees::action(frontend, action)? {
                        modal::action(frontend, action)?;
                    }
                }
                Action::Method(method) => match frontend.runtime.issue_method_with_id(*method) {
                    Ok((request, update)) => {
                        if let Some(context) = frontend.context.as_mut() {
                            context.request = Some(request);
                        }
                        frontend.update(update)?;
                    }
                    Err(error) => {
                        frontend.context = None;
                        frontend.notice = Some(error);
                    }
                },
            }
            frontend.force_redraw = true;
        }
    }
    Ok(())
}

pub(super) fn new_workspace_in_section(
    frontend: &mut ClientFrontend,
    endpoint_id: ClientEndpointId,
    section: crate::workspace::WorkspaceSection,
) -> io::Result<()> {
    let Some(endpoint) = frontend.runtime.shell.endpoint(&endpoint_id) else {
        return Ok(());
    };
    let Some(generation) = endpoint.generation else {
        return Ok(());
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(());
    };
    let Some(workspace) = snapshot.workspaces.iter().find(|workspace| {
        snapshot.focused_workspace_id.as_deref() == Some(workspace.workspace_id.as_str())
    }) else {
        return Ok(());
    };
    let target = super::super::ResourceKey {
        endpoint: endpoint_id.clone(),
        id: workspace.workspace_id.clone(),
    };
    let method =
        crate::api::schema::Method::WorkspaceCreate(crate::api::schema::WorkspaceCreateParams {
            section: Some(section),
            cwd: Some(workspace.new_workspace_cwd.clone()),
            focus: true,
            label: None,
            env: Default::default(),
        });
    open_workspace(frontend, target, 0, 0);
    if let Some(context) = frontend.context.as_mut() {
        context.pending = Some(Action::Method(Box::new(method)));
    }
    let update = frontend.runtime.activate(endpoint_id, None, Instant::now());
    frontend.update(update)?;
    Ok(())
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) -> io::Result<()> {
    let Some(context) = frontend.context.as_ref() else {
        return Ok(());
    };
    if context.target.endpoint == result.endpoint_id
        && context.generation == result.generation
        && context.boot == result.boot_id
        && context.request.as_deref() == Some(&result.request_id)
    {
        tracing::debug!(
            request = %result.request_id,
            success = result.result.is_ok(),
            "context received owning endpoint response"
        );
        if result.result.is_ok() {
            let endpoint = context.target.endpoint.clone();
            if let Some(Action::Method(method)) = &context.pending {
                let section = match method.as_ref() {
                    crate::api::schema::Method::WorkspaceSetSection(params) => Some(params.section),
                    crate::api::schema::Method::WorkspaceCreate(params) => params.section,
                    _ => None,
                };
                if let Some(section) = section {
                    frontend
                        .chrome
                        .collapsed_sections
                        .remove(&(endpoint.clone(), section));
                    frontend.chrome.workspace_scroll = 0;
                    frontend.chrome.agent_scroll = 0;
                    frontend.persist_chrome_preferences();
                }
            }
            if let Some(context) = frontend.context.as_mut() {
                context.finishing = true;
            }
            // A mutation response precedes the resized surface. Keep input fenced until
            // the existing activation barrier commits it, without changing viewer focus.
            let update = frontend.runtime.activate(endpoint, None, Instant::now());
            frontend.update(update)?;
        } else {
            frontend.context = None;
        }
        frontend.force_redraw = true;
    }
    Ok(())
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(context) = frontend
        .context
        .as_ref()
        .filter(|context| context.pending.is_none())
    else {
        return;
    };
    crate::ui::render_context_menu_from(
        frame,
        context.rect(frontend),
        &frontend.chrome.settings.palette,
        &context.facts().items(),
        context.list.highlighted,
    );
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    let Some(mut context) = frontend.context.take() else {
        return Ok(false);
    };
    if !context.current(frontend) {
        return Ok(true);
    }
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        frontend.context = Some(context);
        return Ok(false);
    }
    if context.pending.is_some() {
        if matches!(event, RawInputEvent::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc)
        {
            frontend.force_redraw = true;
            return Ok(true);
        }
        frontend.context = Some(context);
        return Ok(true);
    }
    let items = context.facts().items();
    let mut selected = None;
    let mut closed = false;
    match event {
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
            let mut input = ContextMenuInput {
                list: &mut context.list,
                items: &items,
                closed: false,
            };
            selected = input.key(key.as_key_event());
            closed = input.closed;
        }
        RawInputEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
            let rect = context.rect(frontend);
            if !rect.contains((mouse.column, mouse.row).into()) {
                closed = true;
            } else {
                selected = crate::app::context_menu_item_at_from(
                    rect,
                    mouse.column,
                    mouse.row,
                    items.len() as u16,
                );
            }
        }
        RawInputEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => {
            let hovered = crate::app::context_menu_item_at_from(
                context.rect(frontend),
                mouse.column,
                mouse.row,
                items.len() as u16,
            );
            context.list.hover(hovered);
        }
        _ => {}
    }
    if closed {
        frontend.force_redraw = true;
        return Ok(true);
    }
    if let Some(action) = selected
        .and_then(|index| items.get(index))
        .and_then(|item| action(&context, item))
    {
        if matches!(action, Action::ToggleGroup) {
            toggle_group(frontend, &context.target);
            frontend.force_redraw = true;
            return Ok(true);
        }
        context.pending = Some(action);
        let target = context.target.clone();
        let focus = match context.kind {
            Kind::Workspace { .. } => super::super::FocusTarget::Workspace(target.id),
            Kind::Tab => super::super::FocusTarget::Tab(target.id),
            Kind::Pane { .. } => super::super::FocusTarget::Pane(target.id),
        };
        frontend.context = Some(context);
        let update = frontend
            .runtime
            .activate(target.endpoint, Some(focus), Instant::now());
        frontend.update(update)?;
    } else {
        frontend.context = Some(context);
    }
    frontend.force_redraw = true;
    Ok(true)
}

fn action(context: &Context, item: &str) -> Option<Action> {
    use crate::api::schema as api;
    if matches!(context.kind, Kind::Workspace { .. }) {
        let command = match item {
            "New Claude Code agent" => Some("claude"),
            "New Codex agent" => Some("codex"),
            "New agy agent" => Some("agy"),
            "New grok agent" => Some("grok"),
            "New letta agent" => Some("letta"),
            "New qwen agent" => Some("qwen"),
            _ => None,
        };
        if let Some(command) = command {
            return Some(Action::ConfiguredAgent(command));
        }
    }
    let pane_id = context.target.id.clone();
    let navigate = match (context.kind, item) {
        (Kind::Workspace { .. }, "Rename") => Some(NavigateAction::RenameWorkspace),
        (Kind::Workspace { .. }, "New worktree") => Some(NavigateAction::NewWorktree),
        (Kind::Workspace { .. }, "Open worktree...") => Some(NavigateAction::OpenWorktree),
        (Kind::Workspace { .. }, "Delete worktree checkout...") => {
            Some(NavigateAction::RemoveWorktree)
        }
        (Kind::Workspace { .. }, "Close" | "Close group") => Some(NavigateAction::CloseWorkspace),
        (Kind::Tab, "New tab") => Some(NavigateAction::NewTab),
        (Kind::Tab, "Rename") => Some(NavigateAction::RenameTab),
        (Kind::Tab, "Close") => Some(NavigateAction::CloseTab),
        (Kind::Pane { .. }, "Rename pane") => Some(NavigateAction::RenamePane),
        (Kind::Pane { .. }, "Close pane") => Some(NavigateAction::ClosePane),
        _ => None,
    };
    if let Some(action) = navigate {
        return Some(Action::Navigate(action));
    }
    if matches!(context.kind, Kind::Workspace { .. }) {
        if matches!(item, "Collapse" | "Expand") {
            return Some(Action::ToggleGroup);
        }
        if let Some(section) = crate::app::workspace_section_for_menu_item(Some(item)) {
            return Some(Action::Method(Box::new(api::Method::WorkspaceSetSection(
                api::WorkspaceSetSectionParams {
                    workspace_id: context.target.id.clone(),
                    section,
                },
            ))));
        }
    }
    if matches!(context.kind, Kind::Workspace { .. }) && item == "Duplicate" {
        return Some(Action::Method(Box::new(api::Method::WorkspaceDuplicate(
            api::WorkspaceDuplicateParams {
                workspace_id: context.target.id.clone(),
                focus: true,
            },
        ))));
    }
    if !matches!(context.kind, Kind::Pane { .. }) {
        return None;
    }
    let arrangement = match item {
        "Move to left split" => Some(api::PaneArrangement::MoveLeft),
        "Move to right split" => Some(api::PaneArrangement::MoveRight),
        "Move to upper split" => Some(api::PaneArrangement::MoveUp),
        "Move to lower split" => Some(api::PaneArrangement::MoveDown),
        "Equalize pane sizes" => Some(api::PaneArrangement::Equalize),
        "Cycle pane layout" => Some(api::PaneArrangement::Cycle),
        "Rotate panes" => Some(api::PaneArrangement::RotateForward),
        "Rotate panes reverse" => Some(api::PaneArrangement::RotateBackward),
        _ => None,
    };
    if let Some(arrangement) = arrangement {
        return Some(Action::Method(Box::new(api::Method::PaneArrange(
            api::PaneArrangeParams {
                pane_id,
                arrangement,
            },
        ))));
    }
    let method = match item {
        "Clear pane name" => api::Method::PaneRename(api::PaneRenameParams {
            pane_id,
            label: None,
        }),
        "Zoom" | "Unzoom" => api::Method::PaneZoom(api::PaneZoomParams {
            pane_id: Some(pane_id),
            mode: api::PaneZoomMode::Toggle,
        }),
        "Split vertical" | "Split horizontal" => api::Method::PaneSplit(api::PaneSplitParams {
            workspace_id: None,
            target_pane_id: Some(pane_id),
            direction: if item == "Split vertical" {
                api::SplitDirection::Right
            } else {
                api::SplitDirection::Down
            },
            ratio: None,
            cwd: None,
            focus: true,
            env: Default::default(),
        }),
        _ => return None,
    };
    Some(Action::Method(Box::new(method)))
}

pub(super) fn toggle_group(frontend: &mut ClientFrontend, target: &super::super::ResourceKey) {
    let Some(key) = frontend
        .runtime
        .shell
        .endpoint(&target.endpoint)
        .and_then(|endpoint| {
            endpoint
                .generation
                .and_then(|generation| endpoint.cache.live_snapshot(generation))
        })
        .and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == target.id)
        })
        .and_then(|workspace| workspace.worktree.as_ref())
        .map(|space| (target.endpoint.clone(), space.key.clone()))
    else {
        return;
    };
    if !frontend.chrome.collapsed_groups.remove(&key) {
        frontend.chrome.collapsed_groups.insert(key);
    }
    frontend.persist_chrome_preferences();
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<Rect> {
    Some(
        frontend
            .context
            .as_ref()
            .filter(|context| context.pending.is_none())?
            .rect(frontend),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_original_six_agent_rows_dispatch_configured_owner_without_launch() {
        let facts = ContextMenuFacts::Workspace {};
        let context = Context {
            kind: Kind::Workspace { facts },
            target: super::super::super::ResourceKey {
                endpoint: ClientEndpointId::Local,
                id: "s1".into(),
            },
            generation: 1,
            boot: "owner-boot".into(),
            x: 0,
            y: 0,
            list: MenuListState::new(0),
            pending: None,
            request: None,
            finishing: false,
        };
        for (label, agent) in [
            ("New Claude Code agent", "claude"),
            ("New Codex agent", "codex"),
            ("New agy agent", "agy"),
            ("New grok agent", "grok"),
            ("New letta agent", "letta"),
            ("New qwen agent", "qwen"),
        ] {
            assert!(facts.items().contains(&label));
            assert!(
                matches!(action(&context, label), Some(Action::ConfiguredAgent(actual)) if actual == agent)
            );
        }
    }

    #[test]
    fn context_original_worktree_rows_dispatch_existing_owner_actions() {
        for linked in [false, true] {
            let facts = ContextMenuFacts::GitWorkspace {
                is_linked_worktree: linked,
                has_worktree_children: false,
                collapsed: false,
            };
            let context = Context {
                kind: Kind::Workspace { facts },
                target: super::super::super::ResourceKey {
                    endpoint: ClientEndpointId::Local,
                    id: "s1".into(),
                },
                generation: 1,
                boot: "owner-boot".into(),
                x: 0,
                y: 0,
                list: MenuListState::new(0),
                pending: None,
                request: None,
                finishing: false,
            };
            let expected = if linked {
                vec![(
                    "Delete worktree checkout...",
                    NavigateAction::RemoveWorktree,
                )]
            } else {
                vec![
                    ("New worktree", NavigateAction::NewWorktree),
                    ("Open worktree...", NavigateAction::OpenWorktree),
                ]
            };
            for (label, target) in expected {
                assert!(facts.items().contains(&label));
                assert!(
                    matches!(action(&context, label), Some(Action::Navigate(actual)) if actual == target)
                );
            }
        }
    }

    fn workspace() -> wire::ClientShellWorkspace {
        wire::ClientShellWorkspace {
            workspace_id: "s1".into(),
            active_tab_id: "s1:t1".into(),
            new_workspace_cwd: "/owned".into(),
            number: 1,
            label: "owned".into(),
            custom_label: false,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: None,
            focused: false,
            agent_status: crate::api::schema::AgentStatus::Unknown,
        }
    }

    #[test]
    fn context_workspace_git_menu_retains_unmanaged_root_and_linked_distinction() {
        let mut workspace = workspace();
        assert!(!workspace_facts(&workspace, &[], None, false)
            .items()
            .contains(&"New worktree"));
        let mut git = wire::ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        };
        assert!(workspace_facts(&workspace, &[], Some(&git), false)
            .items()
            .contains(&"New worktree"));
        git.is_linked_worktree = true;
        assert!(!workspace_facts(&workspace, &[], Some(&git), false)
            .items()
            .contains(&"Delete worktree checkout..."));
        workspace.worktree = Some(git.clone());
        assert!(workspace_facts(&workspace, &[], Some(&git), false)
            .items()
            .contains(&"Delete worktree checkout..."));
    }

    #[test]
    fn context_workspace_group_counts_members_without_requiring_linked_child() {
        let mut root = workspace();
        root.worktree = Some(wire::ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let duplicate = root.clone();
        let members = [root.clone(), duplicate];
        assert!(workspace_facts(&root, &members, None, false)
            .items()
            .contains(&"Close group"));
        assert!(workspace_facts(&root, &members, None, false)
            .items()
            .contains(&"Collapse"));
        assert!(workspace_facts(&root, &members, None, true)
            .items()
            .contains(&"Expand"));
        assert!(!workspace_facts(&root, &[root.clone()], None, true)
            .items()
            .contains(&"Expand"));
    }

    #[test]
    fn context_workspace_older_json_without_git_fact_stays_decodable() {
        let old: crate::protocol::endpoint_projection::SnapshotJson =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
            )))
            .unwrap();
        assert!(old.workspace_facts.is_empty());
    }
}
