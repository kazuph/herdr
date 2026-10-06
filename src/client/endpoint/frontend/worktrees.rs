//! Borrowed worktree presentation; Git and path resolution remain at the owner.
use super::*;
use crate::api::schema as api;
use crate::app::state::{
    WorktreeCreateState, WorktreeOpenEntry, WorktreeOpenState, WorktreeRemoveState,
};
use crate::app::NavigateAction;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};

struct Owner {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    workspace: String,
}

impl Owner {
    fn current(&self, frontend: &ClientFrontend) -> bool {
        frontend.runtime.shell.active_endpoint_id == self.endpoint
            && frontend
                .runtime
                .shell
                .endpoint(&self.endpoint)
                .is_some_and(|endpoint| {
                    endpoint.generation == Some(self.generation)
                        && endpoint
                            .cache
                            .live_snapshot(self.generation)
                            .is_some_and(|snapshot| snapshot.boot_id == self.boot)
                })
    }
}

enum Dialog {
    Query(NavigateAction),
    Create {
        state: WorktreeCreateState,
        prefix: String,
        input: String,
        replace: bool,
    },
    Open(WorktreeOpenState),
    Remove(WorktreeRemoveState),
}

pub(super) struct Worktrees {
    owner: Owner,
    dialog: Dialog,
    request: Option<String>,
    finishing: bool,
}

pub(super) fn action(frontend: &mut ClientFrontend, action: NavigateAction) -> io::Result<bool> {
    if !matches!(
        action,
        NavigateAction::NewWorktree | NavigateAction::OpenWorktree | NavigateAction::RemoveWorktree
    ) {
        return Ok(false);
    }
    if !frontend.runtime.input_lease_current() {
        return Ok(true);
    }
    let Some(endpoint) = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
    else {
        return Ok(true);
    };
    let Some(generation) = endpoint.generation else {
        return Ok(true);
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return Ok(true);
    };
    let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|ws| Some(&ws.workspace_id) == snapshot.focused_workspace_id.as_ref())
    else {
        return Ok(true);
    };
    let managed = workspace.worktree.as_ref();
    let git = endpoint
        .cache
        .workspace_facts(generation, &workspace.workspace_id)
        .and_then(|facts| facts.git_space.as_ref());
    if action != NavigateAction::RemoveWorktree
        && (managed.is_some_and(|space| space.is_linked_worktree)
            || git.is_some_and(|space| space.is_linked_worktree))
    {
        return Ok(true);
    }
    if action == NavigateAction::RemoveWorktree
        && !managed.is_some_and(|space| space.is_linked_worktree)
    {
        frontend.notice = Some("This workspace is not a Herdr-managed worktree checkout.".into());
        return Ok(true);
    }
    let owner = Owner {
        endpoint: endpoint.endpoint_id.clone(),
        generation,
        boot: snapshot.boot_id.clone(),
        workspace: workspace.workspace_id.clone(),
    };
    let method = api::Method::WorktreeList(api::WorktreeListParams {
        workspace_id: Some(owner.workspace.clone()),
        cwd: None,
    });
    frontend.worktrees = Some(Worktrees {
        owner,
        dialog: Dialog::Query(action),
        request: None,
        finishing: false,
    });
    issue(frontend, method)?;
    Ok(true)
}

fn error(value: &mut Worktrees, message: String, code: Option<&str>) -> bool {
    match &mut value.dialog {
        Dialog::Create { state, .. } => {
            state.creating = false;
            state.error = Some(message);
        }
        Dialog::Open(state) => state.error = Some(message),
        Dialog::Remove(state) => {
            state.removing = false;
            if code == Some("dirty_worktree_requires_force") {
                state.force_confirmation = true;
                state.error = None;
            } else {
                state.error = Some(message);
            }
        }
        Dialog::Query(_) => return false,
    }
    true
}

fn issue(frontend: &mut ClientFrontend, method: api::Method) -> io::Result<()> {
    match frontend.runtime.issue_method_with_id(method) {
        Ok((request, update)) => {
            if let Some(value) = frontend.worktrees.as_mut() {
                value.request = Some(request);
            }
            frontend.update(update)?;
        }
        Err(message) => {
            if let Some(mut value) = frontend.worktrees.take() {
                if error(&mut value, message.clone(), None) {
                    frontend.worktrees = Some(value);
                } else {
                    frontend.notice = Some(message);
                }
            }
        }
    }
    Ok(())
}

fn from_list(
    owner: &Owner,
    action: NavigateAction,
    source: api::WorktreeSourceInfo,
    worktrees: Vec<api::WorktreeInfo>,
) -> Option<Dialog> {
    match action {
        NavigateAction::NewWorktree => {
            // Missing owner facts must not be resolved against the client's filesystem.
            let prefix = source.checkout_path_prefix?;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(0);
            let branch = crate::worktree::generated_branch_slug(seed);
            let path = format!("{prefix}{}", crate::worktree::branch_to_path_slug(&branch));
            Some(Dialog::Create {
                state: WorktreeCreateState {
                    source_workspace_id: owner.workspace.clone(),
                    source_checkout_path: source.source_checkout_path.into(),
                    source_existing_membership: None,
                    source_repo_root: source.repo_root.into(),
                    repo_key: source.repo_key,
                    repo_name: source.repo_name,
                    branch: branch.clone(),
                    checkout_path: path.into(),
                    error: None,
                    creating: false,
                },
                prefix,
                input: branch,
                replace: true,
            })
        }
        NavigateAction::OpenWorktree => {
            let entries = worktrees
                .into_iter()
                .filter(|entry| !entry.is_bare && !entry.is_prunable)
                .enumerate()
                .map(|(index, entry)| WorktreeOpenEntry {
                    path: entry.path.into(),
                    branch: entry.branch,
                    is_linked_worktree: entry.is_linked_worktree,
                    // Display-only marker, never an index into local AppState.
                    already_open_ws_idx: entry.open_workspace_id.is_some().then_some(index),
                })
                .collect::<Vec<_>>();
            Some(Dialog::Open(WorktreeOpenState {
                source_workspace_id: owner.workspace.clone(),
                source_existing_membership: None,
                source_checkout_path: source.source_checkout_path.into(),
                source_repo_root: source.repo_root.into(),
                repo_key: source.repo_key,
                repo_name: source.repo_name,
                entries,
                selected: 0,
                query: String::new(),
                search_focused: false,
                error: None,
            }))
        }
        NavigateAction::RemoveWorktree => {
            let entry = worktrees.into_iter().find(|entry| {
                entry.open_workspace_id.as_deref() == Some(&owner.workspace)
                    && entry.is_linked_worktree
            })?;
            Some(Dialog::Remove(WorktreeRemoveState {
                workspace_id: owner.workspace.clone(),
                repo_root: source.repo_root.into(),
                path: entry.path.into(),
                error: None,
                removing: false,
                force_confirmation: false,
            }))
        }
        _ => None,
    }
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) -> io::Result<bool> {
    let Some(value) = frontend.worktrees.as_ref() else {
        return Ok(false);
    };
    if value.owner.endpoint != result.endpoint_id
        || value.owner.generation != result.generation
        || value.owner.boot != result.boot_id
        || value.request.as_deref() != Some(&result.request_id)
    {
        return Ok(false);
    }
    let Some(mut value) = frontend.worktrees.take() else {
        return Ok(false);
    };
    if !value.owner.current(frontend) {
        return Ok(true);
    }
    value.request = None;
    match &result.result {
        Err(failure) => {
            if !error(&mut value, failure.message.clone(), failure.code.as_deref()) {
                frontend.notice = Some(failure.message.clone());
                return Ok(true);
            }
        }
        Ok(json) => {
            let response: api::ResponseResult =
                serde_json::from_value(json.clone()).map_err(io::Error::other)?;
            if let Dialog::Query(action) = value.dialog {
                let api::ResponseResult::WorktreeList { source, worktrees } = response else {
                    return Ok(true);
                };
                let Some(dialog) = from_list(&value.owner, action, source, worktrees) else {
                    return Ok(true);
                };
                if matches!(&dialog, Dialog::Open(state) if state.entries.is_empty()) {
                    frontend.notice = Some("No Git worktrees found for this repo.".into());
                    return Ok(true);
                }
                value.dialog = dialog;
            } else {
                let success = match (&mut value.dialog, response) {
                    (
                        Dialog::Create { state, .. },
                        api::ResponseResult::WorktreeCreated { worktree, .. },
                    ) => {
                        state.creating = false;
                        state.checkout_path.to_str() == Some(worktree.path.as_str())
                    }
                    (Dialog::Open(_), api::ResponseResult::WorktreeOpened { .. }) => true,
                    (
                        Dialog::Remove(state),
                        api::ResponseResult::WorktreeRemoved {
                            workspace_id, path, ..
                        },
                    ) => {
                        state.workspace_id == workspace_id
                            && state.path.to_str() == Some(path.as_str())
                    }
                    _ => false,
                };
                if success {
                    value.finishing = true;
                    let endpoint = value.owner.endpoint.clone();
                    frontend.worktrees = Some(value);
                    let update = frontend.runtime.activate(endpoint, None, Instant::now());
                    frontend.update(update)?;
                    return Ok(true);
                }
            }
        }
    }
    frontend.worktrees = Some(value);
    frontend.force_redraw = true;
    Ok(true)
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend.worktrees.as_ref().is_some_and(|value| {
        !value.owner.current(frontend)
            || (value.finishing && frontend.runtime.input_lease_current())
    }) {
        frontend.worktrees = None;
        frontend.force_redraw = true;
    }
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(value) = frontend.worktrees.as_ref() else {
        return;
    };
    let area = frame.area();
    let palette = &frontend.chrome.settings.palette;
    match &value.dialog {
        Dialog::Create { state, input, .. } => {
            crate::ui::render_worktree_create(state, input, palette, frame, area)
        }
        Dialog::Open(state) => crate::ui::render_worktree_open(state, palette, frame, area),
        Dialog::Remove(state) => crate::ui::render_worktree_remove(state, palette, frame, area),
        Dialog::Query(_) => {}
    }
}

fn edit_create(state: &mut WorktreeCreateState, prefix: &str, input: &str) {
    state.branch = input.into();
    state.checkout_path = format!("{prefix}{}", crate::worktree::branch_to_path_slug(input)).into();
    state.error = None;
}

fn submit(value: &mut Worktrees) -> Option<api::Method> {
    if value.request.is_some() || value.finishing {
        return None;
    }
    match &mut value.dialog {
        Dialog::Create {
            state,
            input,
            prefix,
            ..
        } => {
            let branch = input.trim().to_string();
            if branch.is_empty() {
                state.error = Some("branch is required".into());
                return None;
            }
            *input = branch.clone();
            edit_create(state, prefix, input);
            state.creating = true;
            Some(api::Method::WorktreeCreate(api::WorktreeCreateParams {
                workspace_id: Some(value.owner.workspace.clone()),
                branch: Some(branch),
                base: Some("HEAD".into()),
                path: Some(state.checkout_path.display().to_string()),
                focus: true,
                ..Default::default()
            }))
        }
        Dialog::Open(state) => {
            let entry = state.entries.get(state.selected_entry_index()?)?;
            Some(api::Method::WorktreeOpen(api::WorktreeOpenParams {
                workspace_id: Some(value.owner.workspace.clone()),
                path: Some(entry.path.display().to_string()),
                focus: true,
                ..Default::default()
            }))
        }
        Dialog::Remove(state) => {
            state.removing = true;
            state.error = None;
            Some(api::Method::WorktreeRemove(api::WorktreeRemoveParams {
                workspace_id: value.owner.workspace.clone(),
                force: state.force_confirmation,
            }))
        }
        Dialog::Query(_) => None,
    }
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        return Ok(false);
    }
    let Some(mut value) = frontend.worktrees.take() else {
        return Ok(false);
    };
    if !value.owner.current(frontend) {
        return Ok(true);
    }
    let mut accept = false;
    let mut cancel = false;
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    match event {
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Esc => cancel = true,
            KeyCode::Enter => accept = true,
            _ => match &mut value.dialog {
                Dialog::Create {
                    state,
                    input,
                    prefix,
                    replace,
                } => {
                    match key.code {
                        KeyCode::Backspace => {
                            if *replace {
                                input.clear();
                                *replace = false;
                            } else {
                                input.pop();
                            }
                        }
                        KeyCode::Char(ch) => {
                            crate::input::rename::insert(input, replace, &ch.to_string())
                        }
                        _ => {}
                    }
                    edit_create(state, prefix, input);
                }
                Dialog::Open(state) => {
                    match key.code {
                        KeyCode::Up => state.select_previous_filtered(),
                        KeyCode::Down => state.select_next_filtered(),
                        KeyCode::Char('/') => {
                            if state.search_focused {
                                state.query.push('/');
                            } else {
                                state.search_focused = true;
                            }
                        }
                        KeyCode::Char(ch)
                            if state.search_focused
                                && (key.modifiers.is_empty()
                                    || key.modifiers == KeyModifiers::SHIFT)
                                && !ch.is_control() =>
                        {
                            state.query.push(ch)
                        }
                        KeyCode::Backspace if state.search_focused => {
                            state.query.pop();
                        }
                        _ => {}
                    }
                    state.normalize_selection();
                }
                _ => {}
            },
        },
        RawInputEvent::Paste(text) => match &mut value.dialog {
            Dialog::Create {
                state,
                input,
                prefix,
                replace,
            } => {
                crate::input::rename::insert(input, replace, text);
                edit_create(state, prefix, input);
            }
            Dialog::Open(state) if state.search_focused => {
                state.query.push_str(text);
                state.normalize_selection();
            }
            _ => {}
        },
        RawInputEvent::Mouse(mouse) => {
            let point = Position::new(mouse.column, mouse.row);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let buttons = match &mut value.dialog {
                    Dialog::Create { .. } => crate::ui::new_linked_worktree_inner_rect(area)
                        .map(crate::ui::new_linked_worktree_button_rects),
                    Dialog::Remove(state) => {
                        crate::ui::remove_worktree_popup_rect(area).map(|popup| {
                            crate::ui::remove_worktree_button_rects(
                                Rect::new(
                                    popup.x + 1,
                                    popup.y + 1,
                                    popup.width.saturating_sub(2),
                                    popup.height.saturating_sub(2),
                                ),
                                state.force_confirmation,
                            )
                        })
                    }
                    Dialog::Open(state) => crate::ui::open_existing_worktree_inner_rect(
                        area,
                        state.entries.len(),
                    )
                    .map(|inner| {
                        if mouse.row == inner.y.saturating_add(1)
                            && mouse.column >= inner.x
                            && mouse.column < inner.x.saturating_add(inner.width)
                        {
                            state.search_focused = true;
                        } else if inner.contains(point) {
                            let max_rows =
                                crate::ui::open_existing_worktree_max_visible_rows(inner);
                            let start =
                                crate::ui::open_existing_worktree_visible_start(state, max_rows);
                            if let Some(index) = mouse
                                .row
                                .checked_sub(inner.y.saturating_add(3))
                                .map(usize::from)
                                .map(|row| row / 2)
                                .filter(|row| *row < max_rows)
                                .and_then(|row| state.filtered_indices().get(start + row).copied())
                            {
                                state.selected = index;
                                accept = true;
                            }
                        }
                        crate::ui::open_existing_worktree_button_rects(inner)
                    }),
                    Dialog::Query(_) => None,
                };
                if let Some((confirm, close)) = buttons {
                    accept |= confirm.contains(point);
                    cancel = close.contains(point);
                }
            } else if let Dialog::Open(state) = &mut value.dialog {
                match mouse.kind {
                    MouseEventKind::ScrollUp => state.select_previous_filtered(),
                    MouseEventKind::ScrollDown => state.select_next_filtered(),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let busy = value.finishing
        || matches!(&value.dialog, Dialog::Create { state, .. } if state.creating)
        || matches!(&value.dialog, Dialog::Remove(state) if state.removing);
    if cancel && !busy {
        frontend.force_redraw = true;
        return Ok(true);
    }
    let method = accept.then(|| submit(&mut value)).flatten();
    frontend.worktrees = Some(value);
    if let Some(method) = method {
        issue(frontend, method)?;
    }
    frontend.force_redraw = true;
    Ok(true)
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<ratatui::layout::Rect> {
    let area = ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows);
    let inner = match &frontend.worktrees.as_ref()?.dialog {
        Dialog::Create { .. } => crate::ui::new_linked_worktree_inner_rect(area),
        Dialog::Open(state) => {
            crate::ui::open_existing_worktree_inner_rect(area, state.entries.len())
        }
        Dialog::Remove(_) => return crate::ui::remove_worktree_popup_rect(area),
        Dialog::Query(_) => return None,
    }?;
    Some(ratatui::layout::Rect::new(
        inner.x.saturating_sub(1),
        inner.y.saturating_sub(1),
        inner.width.saturating_add(2),
        inner.height.saturating_add(2),
    ))
}
