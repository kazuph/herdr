use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent};

use super::{
    state::{WorktreeCreateState, WorktreeOpenEntry, WorktreeOpenState, WorktreeRemoveState},
    App, Mode,
};
use crate::events::{AppEvent, WorktreeAddResult, WorktreeRemoveResult};

impl App {
    fn selected_workspace_cwd(&self, ws_idx: usize) -> Result<std::path::PathBuf, String> {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| {
                ws.resolved_identity_cwd_from(&self.state.terminals, &self.state.terminal_runtimes)
            })
            .ok_or_else(|| "workspace has no filesystem location".to_string())
    }

    fn selected_worktree_source(
        &self,
        ws_idx: usize,
    ) -> Result<crate::worktree::WorktreeSource, String> {
        let cwd = self.selected_workspace_cwd(ws_idx)?;
        crate::worktree::source_for_cwd(&cwd)
    }

    pub(crate) fn open_new_linked_worktree_dialog(&mut self, ws_idx: usize) {
        let source = match self.selected_worktree_source(ws_idx) {
            Ok(source) => source,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        };
        let source_workspace_id = self.state.workspaces[ws_idx].id.clone();
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        let branch = crate::worktree::generated_branch_slug(seed);
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &source.repo_name,
            &branch,
        );

        self.state.selected = ws_idx;
        self.state.name_input = branch.clone();
        self.state.name_input_replace_on_type = true;
        self.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id,
            source_repo_root: source.repo_root,
            repo_name: source.repo_name,
            branch,
            checkout_path,
            error: None,
            creating: false,
        });
        self.state.mode = Mode::NewLinkedWorktree;
    }

    pub(crate) fn open_existing_worktree_dialog(&mut self, ws_idx: usize) {
        let source = match self.selected_worktree_source(ws_idx) {
            Ok(source) => source,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        };
        let entries = match crate::worktree::list_existing_worktrees(&source.repo_root) {
            Ok(entries) => entries,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        }
        .into_iter()
        .filter(|entry| !entry.is_bare && !entry.is_prunable)
        .map(|entry| {
            let canonical_path = crate::worktree::canonical_or_original(&entry.path);
            let already_open_ws_idx = self.state.workspaces.iter().position(|ws| {
                ws.resolved_identity_cwd_from(&self.state.terminals, &self.state.terminal_runtimes)
                    .as_deref()
                    .is_some_and(|cwd| {
                        crate::worktree::canonical_or_original(cwd) == canonical_path
                    })
            });
            WorktreeOpenEntry {
                path: entry.path,
                branch: entry.branch,
                already_open_ws_idx,
            }
        })
        .collect::<Vec<_>>();

        if entries.is_empty() {
            self.state.config_diagnostic = Some("no Git worktrees found for this repo".into());
            return;
        }

        self.state.selected = ws_idx;
        self.state.worktree_open = Some(WorktreeOpenState {
            source_repo_root: source.repo_root,
            entries,
            selected: 0,
            error: None,
        });
        self.state.mode = Mode::OpenExistingWorktree;
    }

    pub(crate) fn open_remove_linked_worktree_confirmation(&mut self, ws_idx: usize) {
        let cwd = match self.selected_workspace_cwd(ws_idx) {
            Ok(cwd) => cwd,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        };
        let source = match crate::worktree::source_for_cwd(&cwd) {
            Ok(source) => source,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        };
        if crate::worktree::canonical_or_original(&cwd)
            == crate::worktree::canonical_or_original(&source.repo_root)
        {
            self.state.config_diagnostic =
                Some("the root checkout cannot be removed as a linked worktree".into());
            return;
        }

        self.state.selected = ws_idx;
        self.state.worktree_remove = Some(WorktreeRemoveState {
            workspace_id: self.state.workspaces[ws_idx].id.clone(),
            repo_root: source.repo_root,
            path: cwd,
            error: None,
            removing: false,
            force_confirmation: false,
        });
        self.state.mode = Mode::ConfirmRemoveWorktree;
    }

    pub(crate) fn handle_worktree_create_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self
                    .state
                    .worktree_create
                    .as_ref()
                    .is_some_and(|create| create.creating)
                {
                    return;
                }
                self.close_worktree_create_dialog();
            }
            KeyCode::Enter => self.start_worktree_add(),
            KeyCode::Backspace => {
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                } else {
                    self.state.name_input.pop();
                }
                self.sync_worktree_branch_from_input();
            }
            KeyCode::Char(c) => {
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                }
                self.state.name_input.push(c);
                self.sync_worktree_branch_from_input();
            }
            _ => {}
        }
    }

    pub(crate) fn handle_worktree_open_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state.worktree_open = None;
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            KeyCode::Up => {
                if let Some(open) = &mut self.state.worktree_open {
                    open.selected = open.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(open) = &mut self.state.worktree_open {
                    open.selected = open
                        .selected
                        .saturating_add(1)
                        .min(open.entries.len().saturating_sub(1));
                }
            }
            KeyCode::Enter => self.open_selected_existing_worktree(),
            _ => {}
        }
    }

    pub(crate) fn handle_worktree_remove_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self
                    .state
                    .worktree_remove
                    .as_ref()
                    .is_some_and(|remove| remove.removing)
                {
                    return;
                }
                self.state.worktree_remove = None;
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            KeyCode::Enter => self.start_worktree_remove(),
            _ => {}
        }
    }

    pub(crate) fn close_worktree_create_dialog(&mut self) {
        self.state.worktree_create = None;
        self.state.name_input.clear();
        self.state.name_input_replace_on_type = false;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    fn sync_worktree_branch_from_input(&mut self) {
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        create.branch = self.state.name_input.clone();
        create.checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &create.repo_name,
            &create.branch,
        );
        create.error = None;
    }

    pub(crate) fn start_worktree_add(&mut self) {
        self.sync_worktree_branch_from_input();
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        let branch = create.branch.trim().to_string();
        if branch.is_empty() {
            create.error = Some("branch is required".into());
            return;
        }
        if create.creating {
            return;
        }

        create.branch = branch.clone();
        self.state.name_input = branch.clone();
        create.checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &create.repo_name,
            &branch,
        );
        create.creating = true;
        create.error = None;

        let parent_dir = create
            .checkout_path
            .parent()
            .map(std::path::Path::to_path_buf);
        let path = create.checkout_path.clone();
        let source_repo_root = create.source_repo_root.clone();
        let branch = create.branch.clone();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = if let Some(parent_dir) = parent_dir {
                std::fs::create_dir_all(&parent_dir).map_err(|err| err.to_string())
            } else {
                Ok(())
            }
            .and_then(|()| {
                crate::worktree::run_worktree_add_command(&source_repo_root, &path, &branch, "HEAD")
            });
            let _ = event_tx.blocking_send(AppEvent::WorktreeAddFinished(WorktreeAddResult {
                path,
                result,
            }));
        });
    }

    pub(crate) fn close_worktree_open_dialog(&mut self) {
        self.state.worktree_open = None;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    pub(crate) fn open_selected_existing_worktree(&mut self) {
        let Some(open) = self.state.worktree_open.as_ref() else {
            return;
        };
        let Some(entry) = open.entries.get(open.selected).cloned() else {
            return;
        };
        let source_repo_root = open.source_repo_root.clone();
        self.state.worktree_open = None;

        if let Some(ws_idx) = entry.already_open_ws_idx {
            self.state.switch_workspace(ws_idx);
            self.state.mode = Mode::Terminal;
            return;
        }

        match self.create_workspace_with_options(entry.path.clone(), true) {
            Ok(_) => {}
            Err(err) => {
                self.state.worktree_open = Some(WorktreeOpenState {
                    source_repo_root,
                    entries: vec![entry],
                    selected: 0,
                    error: Some(format!("failed to open worktree: {err}")),
                });
                self.state.mode = Mode::OpenExistingWorktree;
            }
        }
    }

    pub(crate) fn close_worktree_remove_dialog(&mut self) {
        if self
            .state
            .worktree_remove
            .as_ref()
            .is_some_and(|remove| remove.removing)
        {
            return;
        }
        self.state.worktree_remove = None;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    pub(crate) fn start_worktree_remove(&mut self) {
        let Some(remove) = &mut self.state.worktree_remove else {
            return;
        };
        if remove.removing {
            return;
        }
        remove.removing = true;
        remove.error = None;

        let force = remove.force_confirmation;
        let command =
            crate::worktree::build_worktree_remove_command(&remove.repo_root, &remove.path, force);
        let path = remove.path.clone();
        let workspace_id = remove.workspace_id.clone();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = crate::worktree::run_worktree_command(&command);
            let _ =
                event_tx.blocking_send(AppEvent::WorktreeRemoveFinished(WorktreeRemoveResult {
                    workspace_id,
                    path,
                    result,
                }));
        });
    }

    pub(crate) fn handle_worktree_add_finished(&mut self, result: WorktreeAddResult) {
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        if create.checkout_path != result.path {
            return;
        }

        match result.result {
            Ok(()) => {
                let path = create.checkout_path.clone();
                let source_workspace_id = create.source_workspace_id.clone();
                self.state.worktree_create = None;
                self.state.name_input.clear();
                self.state.name_input_replace_on_type = false;
                match self.create_workspace_with_options(path, true) {
                    Ok(_) => {
                        if self
                            .state
                            .workspaces
                            .iter()
                            .any(|ws| ws.id == source_workspace_id)
                        {
                            self.state.mark_session_dirty();
                        }
                    }
                    Err(err) => {
                        self.state.config_diagnostic = Some(format!(
                            "created worktree but failed to open workspace: {err}"
                        ));
                        self.state.mode = Mode::Navigate;
                    }
                }
            }
            Err(message) => {
                create.creating = false;
                create.error = Some(message);
            }
        }
        self.render_dirty.store(true, Ordering::Release);
        self.render_notify.notify_one();
    }

    pub(crate) fn handle_worktree_remove_finished(&mut self, result: WorktreeRemoveResult) {
        let Some(remove) = &mut self.state.worktree_remove else {
            return;
        };
        if remove.workspace_id != result.workspace_id || remove.path != result.path {
            return;
        }

        match result.result {
            Ok(()) => {
                self.state.worktree_remove = None;
                if let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|ws| ws.id == result.workspace_id)
                {
                    self.state.selected = ws_idx;
                    self.state.close_selected_workspace();
                }
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            Err(message) if crate::worktree::is_dirty_worktree_remove_error(&message) => {
                remove.removing = false;
                remove.force_confirmation = true;
                remove.error = Some(message);
            }
            Err(message) => {
                remove.removing = false;
                remove.error = Some(message);
            }
        }
        self.render_dirty.store(true, Ordering::Release);
        self.render_notify.notify_one();
    }
}
