use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Direction, Rect};

use crate::{
    app::state::{
        AppState, ContextMenuKind, ContextMenuState, DangerousAction, MenuListState, Mode,
    },
    input::TerminalKey,
    layout::NavDirection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalAction {
    Continue,
    Save,
    Clear,
    Cancel,
    Confirm,
    Apply,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalKeyBinding {
    Enter,
    Esc,
    CtrlC,
}

impl ModalKeyBinding {
    fn matches(self, key: &KeyEvent) -> bool {
        match self {
            Self::Enter => key.code == KeyCode::Enter,
            Self::Esc => key.code == KeyCode::Esc,
            Self::CtrlC => {
                key.code == KeyCode::Char('c')
                    && key.modifiers == crossterm::event::KeyModifiers::CONTROL
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ModalActionSpec<A> {
    pub action: A,
    pub bindings: &'static [ModalKeyBinding],
}

pub(super) fn modal_action_from_key<A: Copy>(
    key: &KeyEvent,
    specs: &[ModalActionSpec<A>],
) -> Option<A> {
    specs
        .iter()
        .find(|spec| spec.bindings.iter().any(|binding| binding.matches(key)))
        .map(|spec| spec.action)
}

pub(super) fn modal_action_from_buttons<A: Copy>(
    col: u16,
    row: u16,
    buttons: &[(Rect, A)],
) -> Option<A> {
    buttons.iter().find_map(|(rect, action)| {
        (col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height)
            .then_some(*action)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarWidthPreset {
    Narrow,
    Normal,
    Wide,
}

impl SidebarWidthPreset {
    fn label(self) -> &'static str {
        match self {
            Self::Narrow => "sidebar narrow",
            Self::Normal => "sidebar normal",
            Self::Wide => "sidebar wide",
        }
    }

    fn width(self, state: &AppState) -> u16 {
        match self {
            Self::Narrow => state.sidebar_min_width,
            Self::Normal => state
                .default_sidebar_width
                .clamp(state.sidebar_min_width, state.sidebar_max_width),
            Self::Wide => state.sidebar_max_width,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalMenuAction {
    Detach,
    WhatsNew,
    Keybinds,
    ReloadConfig,
    ToggleVimMode,
    Settings,
    SetSidebarWidth(SidebarWidthPreset),
}

pub(super) fn global_menu_actions(state: &AppState) -> Vec<GlobalMenuAction> {
    let mut actions = vec![
        GlobalMenuAction::Settings,
        GlobalMenuAction::Keybinds,
        GlobalMenuAction::ReloadConfig,
        GlobalMenuAction::ToggleVimMode,
        GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Narrow),
        GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Normal),
        GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Wide),
    ];
    if state.update_available.is_some() || state.latest_release_notes_available {
        actions.push(GlobalMenuAction::WhatsNew);
    }
    actions.push(GlobalMenuAction::Detach);
    actions
}

pub(super) fn global_menu_action_label(state: &AppState, action: GlobalMenuAction) -> &'static str {
    match action {
        GlobalMenuAction::Detach => "detach",
        GlobalMenuAction::WhatsNew => {
            if state.update_available.is_some() {
                "update ready"
            } else {
                "what's new"
            }
        }
        GlobalMenuAction::Keybinds => "keybinds",
        GlobalMenuAction::ReloadConfig => "reload config",
        GlobalMenuAction::ToggleVimMode => {
            if state.vim_mode_enabled {
                "vim mode on"
            } else {
                "vim mode off"
            }
        }
        GlobalMenuAction::Settings => "settings",
        GlobalMenuAction::SetSidebarWidth(preset) => preset.label(),
    }
}

pub(super) fn open_global_menu(state: &mut AppState) {
    state.global_menu = MenuListState::new(0);
    state.mode = Mode::GlobalMenu;
}

pub(super) fn open_keybind_help(state: &mut AppState) {
    state.keybind_help.scroll = 0;
    state.mode = Mode::KeybindHelp;
}

fn open_update_release_notes(state: &mut AppState) {
    let Some(notes) = crate::release_notes::load_latest() else {
        return;
    };

    state.release_notes = Some(crate::app::state::ReleaseNotesState {
        version: notes.version,
        body: notes.body,
        scroll: 0,
        preview: notes.preview,
    });
    state.mode = Mode::ReleaseNotes;
}

pub(super) fn request_detach(state: &mut AppState) {
    if state.detach_exits {
        state.should_quit = true;
    } else {
        state.detach_requested = true;
    }
}

pub(super) fn apply_global_menu_action(state: &mut AppState, action: GlobalMenuAction) {
    match action {
        GlobalMenuAction::Detach => {
            leave_modal(state);
            request_detach(state);
        }
        GlobalMenuAction::WhatsNew => open_update_release_notes(state),
        GlobalMenuAction::Keybinds => open_keybind_help(state),
        GlobalMenuAction::ReloadConfig => {
            state.request_reload_config = true;
            leave_modal(state);
        }
        GlobalMenuAction::ToggleVimMode => {
            state.vim_mode_enabled = !state.vim_mode_enabled;
            state.vim_insert_mode = false;
            leave_modal(state);
        }
        GlobalMenuAction::Settings => super::settings::open_settings(state),
        GlobalMenuAction::SetSidebarWidth(preset) => {
            state.sidebar_width = preset.width(state);
            state.sidebar_width_source = match preset {
                SidebarWidthPreset::Normal => crate::app::state::SidebarWidthSource::ConfigDefault,
                SidebarWidthPreset::Narrow | SidebarWidthPreset::Wide => {
                    crate::app::state::SidebarWidthSource::Manual
                }
            };
            state.sidebar_width_auto = false;
            state.mark_session_dirty();
            leave_modal(state);
        }
    }
}

pub(crate) fn handle_global_menu_key(state: &mut AppState, key: KeyEvent) {
    let actions = global_menu_actions(state);
    match key.code {
        KeyCode::Esc => leave_modal(state),
        KeyCode::Up | KeyCode::Char('k') => state.global_menu.move_prev(),
        KeyCode::Down | KeyCode::Char('j') => state.global_menu.move_next(actions.len()),
        KeyCode::Enter => {
            if let Some(action) = actions.get(state.global_menu.highlighted).copied() {
                apply_global_menu_action(state, action);
            }
        }
        _ => {}
    }
}

pub(crate) fn handle_keybind_help_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.scroll_keybind_help(-1),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_keybind_help(1),
        KeyCode::PageUp => state.scroll_keybind_help(-8),
        KeyCode::PageDown => state.scroll_keybind_help(8),
        KeyCode::Home => state.keybind_help.scroll = 0,
        KeyCode::End => state.keybind_help.scroll = state.keybind_help_max_scroll(),
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => leave_modal(state),
        _ => {}
    }
}

pub(super) fn open_rename_workspace(state: &mut AppState, ws_idx: usize) {
    state.selected = ws_idx;
    state.rename_pane_target = None;
    state.name_input = state.workspaces[ws_idx].display_name();
    state.name_input_replace_on_type = false;
    state.mode = Mode::RenameWorkspace;
}

pub(super) fn open_rename_active_tab(state: &mut AppState, replace_on_type: bool) {
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.rename_pane_target = None;
    if let Some(ws) = state.active.and_then(|i| state.workspaces.get(i)) {
        if let Some(name) = ws.active_tab_display_name() {
            state.name_input = name;
            state.name_input_replace_on_type = replace_on_type;
            state.mode = Mode::RenameTab;
        }
    }
}

pub(super) fn open_rename_pane(state: &mut AppState, pane_id: crate::layout::PaneId) {
    let Some(ws) = state.active.and_then(|i| state.workspaces.get(i)) else {
        return;
    };
    let Some(pane) = ws.pane_state(pane_id) else {
        return;
    };
    let terminal = state.terminals.get(&pane.attached_terminal_id);
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.rename_pane_target = Some(pane_id);
    state.name_input = terminal
        .and_then(|t| t.manual_label.clone())
        .unwrap_or_default();
    state.name_input_replace_on_type = terminal.and_then(|t| t.manual_label.as_ref()).is_none();
    state.mode = Mode::RenamePane;
}

fn next_new_tab_default_name(state: &AppState) -> String {
    state
        .active
        .and_then(|i| state.workspaces.get(i))
        .map(|ws| (ws.tabs.len() + 1).to_string())
        .unwrap_or_else(|| "1".to_string())
}

pub(super) fn open_new_tab_dialog(state: &mut AppState) {
    state.creating_new_tab = true;
    state.requested_new_tab_name = None;
    state.rename_pane_target = None;
    state.name_input = next_new_tab_default_name(state);
    state.name_input_replace_on_type = true;
    state.mode = Mode::RenameTab;
}

pub(super) fn leave_modal(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    } else {
        state.mode = Mode::Navigate;
    }
}

pub(super) const ONBOARDING_WELCOME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Continue,
    bindings: &[ModalKeyBinding::Enter],
}];

pub(super) const RELEASE_NOTES_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Close,
    bindings: &[ModalKeyBinding::Enter, ModalKeyBinding::Esc],
}];

pub(super) const RENAME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Save,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Clear,
        bindings: &[ModalKeyBinding::CtrlC],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const CONFIRM_CLOSE_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Confirm,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const SETTINGS_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Apply,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Close,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) fn apply_rename_action(state: &mut AppState, action: ModalAction) {
    match action {
        ModalAction::Save => {
            let new_name = if state.name_input.trim().is_empty() {
                state.name_input.clone()
            } else {
                state.name_input.trim().to_string()
            };
            match state.mode {
                Mode::RenameWorkspace if !state.workspaces.is_empty() && !new_name.is_empty() => {
                    let workspace_id = state.workspaces[state.selected].id.clone();
                    state.workspaces[state.selected].set_custom_name(new_name);
                    crate::logging::workspace_renamed(&workspace_id);
                    state.mark_session_dirty();
                }
                Mode::RenameTab if state.creating_new_tab => {
                    state.request_new_tab = true;
                    let default_name = next_new_tab_default_name(state);
                    state.requested_new_tab_name =
                        if new_name.is_empty() || new_name == default_name {
                            None
                        } else {
                            Some(new_name)
                        };
                }
                Mode::RenameTab => {
                    if let Some(ws_idx) = state.active {
                        if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                            let workspace_id = ws.id.clone();
                            let active_tab = ws.active_tab;
                            if let Some(tab) = ws.active_tab_mut() {
                                let keep_auto_name =
                                    tab.is_auto_named() && new_name == tab.number.to_string();
                                if !new_name.is_empty() && !keep_auto_name {
                                    tab.set_custom_name(new_name);
                                    let tab_id = format!("{}:{}", workspace_id, active_tab + 1);
                                    crate::logging::tab_renamed(&workspace_id, &tab_id);
                                    state.mark_session_dirty();
                                }
                            }
                        }
                    }
                }
                Mode::RenamePane => {
                    if let (Some(ws_idx), Some(pane_id)) = (state.active, state.rename_pane_target)
                    {
                        if let Some(ws) = state.workspaces.get(ws_idx) {
                            if let Some(pane) = ws.pane_state(pane_id) {
                                let terminal_id = pane.attached_terminal_id.clone();
                                if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
                                    terminal.set_manual_label(new_name);
                                    state.mark_session_dirty();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            state.creating_new_tab = false;
            state.rename_pane_target = None;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        ModalAction::Clear => {
            state.name_input.clear();
            state.name_input_replace_on_type = false;
        }
        ModalAction::Cancel => {
            state.creating_new_tab = false;
            state.requested_new_tab_name = None;
            state.rename_pane_target = None;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        _ => {}
    }
}

fn clear_rename_input(state: &mut AppState) {
    state.name_input.clear();
    state.name_input_replace_on_type = false;
}

fn delete_rename_input_char(state: &mut AppState) {
    if state.name_input_replace_on_type {
        clear_rename_input(state);
    } else {
        state.name_input.pop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameWordDeleteClass {
    Word,
    Separator,
}

fn rename_word_delete_class(ch: char) -> RenameWordDeleteClass {
    if ch.is_alphanumeric() || ch == '_' {
        RenameWordDeleteClass::Word
    } else {
        RenameWordDeleteClass::Separator
    }
}

fn delete_rename_input_word(state: &mut AppState) {
    if state.name_input_replace_on_type {
        clear_rename_input(state);
        return;
    }

    while state
        .name_input
        .chars()
        .last()
        .is_some_and(char::is_whitespace)
    {
        state.name_input.pop();
    }

    let Some(class) = state
        .name_input
        .chars()
        .last()
        .map(rename_word_delete_class)
    else {
        return;
    };

    while state
        .name_input
        .chars()
        .last()
        .is_some_and(|ch| !ch.is_whitespace() && rename_word_delete_class(ch) == class)
    {
        state.name_input.pop();
    }
}

pub(crate) fn handle_rename_key(state: &mut AppState, key: KeyEvent) {
    if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
        apply_rename_action(state, action);
        return;
    }

    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            clear_rename_input(state);
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {
            clear_rename_input(state);
        }
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            delete_rename_input_word(state);
        }
        KeyCode::Char('h' | 'w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_rename_input_word(state);
        }
        KeyCode::Backspace => delete_rename_input_char(state),
        KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            if state.name_input_replace_on_type {
                clear_rename_input(state);
            }
            state.name_input.push(c);
        }
        _ => {}
    }
}

pub(crate) fn handle_resize_key(state: &mut AppState, raw_key: TerminalKey) {
    let key = raw_key.as_key_event();
    if key.code == KeyCode::Esc
        || key.code == KeyCode::Enter
        || state.keybinds.resize_mode.matches_prefix_key(raw_key)
        || state.keybinds.resize_mode.matches_direct_key(raw_key)
    {
        if state.active.is_some() {
            state.mode = Mode::Terminal;
        } else {
            state.mode = Mode::Navigate;
        }
        return;
    }

    match key.code {
        KeyCode::Char('h') | KeyCode::Left => state.resize_pane(NavDirection::Left),
        KeyCode::Char('l') | KeyCode::Right => state.resize_pane(NavDirection::Right),
        KeyCode::Char('j') | KeyCode::Down => state.resize_pane(NavDirection::Down),
        KeyCode::Char('k') | KeyCode::Up => state.resize_pane(NavDirection::Up),
        _ => {}
    }
}

pub(super) fn open_confirm_close(state: &mut AppState) {
    state.mode = Mode::ConfirmClose;
}

pub(super) fn open_confirm_danger(state: &mut AppState, action: DangerousAction) {
    state.pending_danger_action = Some(action);
    state.mode = Mode::ConfirmDanger;
}

pub(super) fn confirm_close_accept(state: &mut AppState) {
    state.close_selected_workspace();
    if state.workspaces.is_empty() {
        state.mode = Mode::Navigate;
    } else {
        state.mode = Mode::Terminal;
    }
}

pub(super) fn confirm_close_cancel(state: &mut AppState) {
    state.mode = Mode::Navigate;
}

pub(super) fn confirm_danger_accept(state: &mut AppState) {
    let Some(action) = state.pending_danger_action.take() else {
        leave_modal(state);
        return;
    };
    match action {
        DangerousAction::StopServer => {
            state.should_quit = true;
            leave_modal(state);
        }
        DangerousAction::Restart => {
            state.request_restart = true;
            state.should_quit = true;
            leave_modal(state);
        }
        DangerousAction::RestoreAgents => {
            state.request_agent_restore = true;
            leave_modal(state);
        }
    }
}

pub(super) fn confirm_danger_cancel(state: &mut AppState) {
    state.pending_danger_action = None;
    leave_modal(state);
}

pub(crate) fn handle_confirm_close_key(state: &mut AppState, key: KeyEvent) {
    match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
        Some(ModalAction::Confirm) => confirm_close_accept(state),
        Some(ModalAction::Cancel) => confirm_close_cancel(state),
        _ => {}
    }
}

pub(crate) fn handle_confirm_danger_key(state: &mut AppState, key: KeyEvent) {
    match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
        Some(ModalAction::Confirm) => confirm_danger_accept(state),
        Some(ModalAction::Cancel) => confirm_danger_cancel(state),
        _ => {}
    }
}

pub(super) fn apply_context_menu_action(state: &mut AppState, menu: ContextMenuState, idx: usize) {
    let item = menu.items().get(idx).copied();
    match (menu.kind, item) {
        (ContextMenuKind::Workspace { ws_idx }, Some("⭐ favorite")) => {
            let section = crate::workspace::WorkspaceSection::Favorite;
            if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                ws.section = section;
            }
            state.collapsed_workspace_sections.remove(&section);
            state.workspace_scroll = 0;
            state.agent_panel_scroll = 0;
            state.mark_session_dirty();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("💼 work")) => {
            let section = crate::workspace::WorkspaceSection::Work;
            if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                ws.section = section;
            }
            state.collapsed_workspace_sections.remove(&section);
            state.workspace_scroll = 0;
            state.agent_panel_scroll = 0;
            state.mark_session_dirty();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("🏠 personal")) => {
            let section = crate::workspace::WorkspaceSection::Personal;
            if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                ws.section = section;
            }
            state.collapsed_workspace_sections.remove(&section);
            state.workspace_scroll = 0;
            state.agent_panel_scroll = 0;
            state.mark_session_dirty();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("No section")) => {
            let section = crate::workspace::WorkspaceSection::None;
            if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                ws.section = section;
            }
            state.collapsed_workspace_sections.remove(&section);
            state.workspace_scroll = 0;
            state.agent_panel_scroll = 0;
            state.mark_session_dirty();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some(label))
            if crate::app::state::AgentPreset::from_menu_label(label).is_some() =>
        {
            let Some(preset) = crate::app::state::AgentPreset::from_menu_label(label) else {
                leave_modal(state);
                return;
            };
            state.selected = ws_idx;
            state.pending_agent_start = Some(crate::app::state::PendingAgentStartRequest {
                target: crate::app::state::AgentStartTarget::Workspace { ws_idx },
                preset,
            });
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("New worktree")) => {
            state.selected = ws_idx;
            state.pending_worktree_action =
                Some(crate::app::state::WorktreeActionRequest::New { ws_idx });
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Open worktree")) => {
            state.selected = ws_idx;
            state.pending_worktree_action =
                Some(crate::app::state::WorktreeActionRequest::Open { ws_idx });
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Remove worktree")) => {
            state.selected = ws_idx;
            state.pending_worktree_action =
                Some(crate::app::state::WorktreeActionRequest::Remove { ws_idx });
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Duplicate")) => {
            state.selected = ws_idx;
            state.pending_duplicate_workspace = Some(ws_idx);
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Rename")) => {
            open_rename_workspace(state, ws_idx);
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Close")) => {
            state.selected = ws_idx;
            if state.confirm_close {
                open_confirm_close(state);
            } else {
                state.close_selected_workspace();
                state.mode = Mode::Navigate;
            }
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("New tab")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            open_new_tab_dialog(state);
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("Rename")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            open_rename_active_tab(state, false);
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("Close")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.close_tab();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Rename pane")) => {
            open_rename_pane(state, pane_id);
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Clear pane name")) => {
            if let Some(ws_idx) = state.active {
                if let Some(ws) = state.workspaces.get(ws_idx) {
                    if let Some(pane) = ws.pane_state(pane_id) {
                        let terminal_id = pane.attached_terminal_id.clone();
                        if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
                            terminal.clear_manual_label();
                            state.mark_session_dirty();
                        }
                    }
                }
            }
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some(label))
            if crate::app::state::AgentPreset::from_menu_label(label).is_some() =>
        {
            let Some(preset) = crate::app::state::AgentPreset::from_menu_label(label) else {
                leave_modal(state);
                return;
            };
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.pending_agent_start = Some(crate::app::state::PendingAgentStartRequest {
                target: crate::app::state::AgentStartTarget::Pane { pane_id },
                preset,
            });
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Split vertical")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.split_pane(Direction::Horizontal);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Split horizontal")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.split_pane(Direction::Vertical);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Move to left split")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.move_focused_pane_to_split_side(
                Direction::Horizontal,
                crate::layout::RootSplitSide::First,
            );
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Move to right split")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.move_focused_pane_to_split_side(
                Direction::Horizontal,
                crate::layout::RootSplitSide::Second,
            );
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Move to upper split")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.move_focused_pane_to_split_side(
                Direction::Vertical,
                crate::layout::RootSplitSide::First,
            );
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Move to lower split")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.move_focused_pane_to_split_side(
                Direction::Vertical,
                crate::layout::RootSplitSide::Second,
            );
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Equalize pane sizes")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.equalize_pane_sizes();
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Cycle pane layout")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.cycle_pane_layout();
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Rotate panes")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.rotate_panes(false);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { pane_id, .. }, Some("Rotate panes reverse")) => {
            if let Some(ws) = state
                .active
                .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
            {
                ws.layout.focus_pane(pane_id);
            }
            state.rotate_panes(true);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { .. }, Some("Zoom" | "Unzoom")) => {
            state.toggle_zoom();
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane { .. }, Some("Close pane")) => {
            state.close_pane();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        (ContextMenuKind::SidebarBlank, Some("New workspace")) => {
            state.request_new_workspace = true;
            leave_modal(state);
        }
        (ContextMenuKind::SidebarBlank, Some("New tab")) => {
            open_new_tab_dialog(state);
        }
        (ContextMenuKind::SidebarBlank, Some("Settings")) => {
            super::settings::open_settings(state);
        }
        (ContextMenuKind::SidebarBlank, Some("Keybinds")) => {
            open_keybind_help(state);
        }
        (ContextMenuKind::SidebarBlank, Some("Reload config")) => {
            state.request_reload_config = true;
            leave_modal(state);
        }
        (ContextMenuKind::SidebarBlank, Some("Restore agents...")) => {
            open_confirm_danger(state, DangerousAction::RestoreAgents);
        }
        (ContextMenuKind::SidebarBlank, Some("Stop server")) => {
            open_confirm_danger(state, DangerousAction::StopServer);
        }
        (ContextMenuKind::SidebarBlank, Some("Restart")) => {
            open_confirm_danger(state, DangerousAction::Restart);
        }
        (ContextMenuKind::SidebarBlank, Some("Detach")) => {
            request_detach(state);
            leave_modal(state);
        }
        _ => leave_modal(state),
    }
}

pub(crate) fn handle_context_menu_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            state.context_menu = None;
            leave_modal(state);
        }
        KeyCode::Up => {
            if let Some(menu) = &mut state.context_menu {
                move_context_menu_selection(menu, -1);
            }
        }
        KeyCode::Down => {
            if let Some(menu) = &mut state.context_menu {
                move_context_menu_selection(menu, 1);
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = state.context_menu.take() {
                let idx = menu.list.highlighted;
                if menu.is_separator(idx) {
                    state.context_menu = Some(menu);
                } else {
                    apply_context_menu_action(state, menu, idx);
                }
            }
        }
        _ => {}
    }
}

fn move_context_menu_selection(menu: &mut ContextMenuState, direction: isize) {
    let item_count = menu.items().len();
    if item_count == 0 {
        return;
    }
    let mut idx = menu.list.highlighted.min(item_count - 1);
    for _ in 0..item_count {
        idx = if direction < 0 {
            idx.saturating_sub(1)
        } else {
            (idx + 1).min(item_count - 1)
        };
        if !menu.is_separator(idx) {
            menu.list.highlighted = idx;
            return;
        }
        if (direction < 0 && idx == 0) || (direction > 0 && idx + 1 == item_count) {
            return;
        }
    }
}

impl AppState {
    pub(super) fn global_menu_item_at(&self, col: u16, row: u16) -> Option<GlobalMenuAction> {
        let rect = self.global_menu_rect();
        if col <= rect.x
            || col >= rect.x + rect.width.saturating_sub(1)
            || row <= rect.y
            || row >= rect.y + rect.height.saturating_sub(1)
        {
            return None;
        }
        let idx = (row - rect.y - 1) as usize;
        global_menu_actions(self).get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::{Direction, Rect};

    use super::super::{capture_snapshot, state_with_workspaces};
    use super::*;

    fn config_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-modal-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    #[test]
    fn custom_resize_key_exits_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::Resize;
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("g");

        handle_resize_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn direct_resize_key_exits_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::Resize;
        state.keybinds.resize_mode = crate::config::ActionKeybinds::direct("ctrl+alt+r");

        handle_resize_key(
            &mut state,
            TerminalKey::new(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        );

        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn resize_key_exit_matches_enhanced_shifted_punctuation() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::Resize;
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("?");

        handle_resize_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        );

        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn global_menu_sidebar_width_presets_update_width_source_and_snapshot() {
        let mut state = state_with_workspaces(&["test"]);
        state.sidebar_min_width = 18;
        state.default_sidebar_width = 26;
        state.sidebar_max_width = 36;

        apply_global_menu_action(
            &mut state,
            GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Narrow),
        );
        assert_eq!(state.sidebar_width, 18);
        assert_eq!(
            state.sidebar_width_source,
            crate::app::state::SidebarWidthSource::Manual
        );
        assert_eq!(capture_snapshot(&state).sidebar_width, Some(18));

        apply_global_menu_action(
            &mut state,
            GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Wide),
        );
        assert_eq!(state.sidebar_width, 36);
        assert_eq!(
            state.sidebar_width_source,
            crate::app::state::SidebarWidthSource::Manual
        );
        assert_eq!(capture_snapshot(&state).sidebar_width, Some(36));

        apply_global_menu_action(
            &mut state,
            GlobalMenuAction::SetSidebarWidth(SidebarWidthPreset::Normal),
        );
        assert_eq!(state.sidebar_width, 26);
        assert_eq!(
            state.sidebar_width_source,
            crate::app::state::SidebarWidthSource::ConfigDefault
        );
        assert_eq!(capture_snapshot(&state).sidebar_width, Some(26));
    }

    #[test]
    fn global_menu_toggles_vim_mode_and_returns_to_normal() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::GlobalMenu;
        state.vim_mode_enabled = false;
        state.vim_insert_mode = true;

        assert!(global_menu_actions(&state).contains(&GlobalMenuAction::ToggleVimMode));
        assert_eq!(
            global_menu_action_label(&state, GlobalMenuAction::ToggleVimMode),
            "vim mode off"
        );

        apply_global_menu_action(&mut state, GlobalMenuAction::ToggleVimMode);

        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.vim_mode_enabled);
        assert!(!state.vim_insert_mode);
        assert_eq!(
            global_menu_action_label(&state, GlobalMenuAction::ToggleVimMode),
            "vim mode on"
        );
    }

    #[test]
    fn detach_requests_client_detach_in_persistence_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.detach_exits = false;

        request_detach(&mut state);

        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }

    #[test]
    fn detach_exits_in_no_session_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.detach_exits = true;

        request_detach(&mut state);

        assert!(state.should_quit);
        assert!(!state.detach_requested);
    }

    #[test]
    fn global_menu_whats_new_opens_saved_release_notes() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("whats-new-saved-release-notes");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- Menu")
            .unwrap();

        let mut state = state_with_workspaces(&["test"]);
        state.latest_release_notes_available = true;

        assert!(global_menu_actions(&state).contains(&GlobalMenuAction::WhatsNew));

        apply_global_menu_action(&mut state, GlobalMenuAction::WhatsNew);

        assert_eq!(state.mode, Mode::ReleaseNotes);
        assert_eq!(
            state
                .release_notes
                .as_ref()
                .map(|notes| notes.body.as_str()),
            Some("### Changed\n- Menu")
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rename_modal_keyboard_and_mouse_share_actions() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "hello".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(state.name_input.is_empty());

        state.name_input = "renamed".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.workspaces[0].display_name(), "renamed");
        let snapshot = capture_snapshot(&state);
        assert_eq!(
            snapshot.workspaces[0].custom_name.as_deref(),
            Some("renamed")
        );

        state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        state.view.terminal_area = Rect::new(26, 0, 80, 20);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "mouse".into();
        let inner = state.rename_modal_inner().unwrap();
        let (save, _, _) = crate::ui::rename_button_rects(inner);
        let action = modal_action_from_buttons(save.x, save.y, &[(save, ModalAction::Save)]);
        assert_eq!(action, Some(ModalAction::Save));
    }

    #[test]
    fn tab_rename_updates_captured_snapshot() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameTab;
        state.name_input = "logs".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        let snapshot = capture_snapshot(&state);
        assert_eq!(
            snapshot.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn rename_cancel_returns_to_terminal_when_workspace_is_active() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameTab;
        state.name_input = "test".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.name_input.is_empty());
    }

    #[test]
    fn rename_modal_replaces_prefilled_text_on_first_type() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameTab;
        state.name_input = "2".into();
        state.name_input_replace_on_type = true;

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "n");
        assert!(!state.name_input_replace_on_type);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "ne");
    }

    #[test]
    fn rename_modal_handles_line_editing_shortcuts() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "website zero".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "website zer");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website ");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        );
        assert_eq!(state.name_input, "website-");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website-");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website-");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
        );
        assert!(state.name_input.is_empty());

        state.name_input = "website zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert!(state.name_input.is_empty());
    }

    #[test]
    fn rename_modal_does_not_insert_modified_shortcut_chars() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "website".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.name_input, "websiteZ");
    }

    #[test]
    fn open_rename_active_tab_can_prefill_default_new_tab_name() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_add_tab(None);
        state.workspaces[0].switch_tab(1);

        open_rename_active_tab(&mut state, true);

        assert_eq!(state.mode, Mode::RenameTab);
        assert_eq!(state.name_input, "2");
        assert!(state.name_input_replace_on_type);
    }

    #[test]
    fn cancel_new_tab_dialog_leaves_workspace_unchanged() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(!state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
        assert_eq!(state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn saving_new_tab_dialog_requests_creation_with_name() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);
        state.name_input = "logs".into();
        state.name_input_replace_on_type = false;

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert_eq!(state.requested_new_tab_name.as_deref(), Some("logs"));
    }

    #[test]
    fn saving_new_tab_dialog_with_default_name_keeps_tab_auto_named() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
    }

    #[test]
    fn closing_first_auto_tab_resets_remaining_auto_tab_and_next_prompt() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        state.workspaces[0].test_add_tab(state.requested_new_tab_name.as_deref());
        state.request_new_tab = false;
        state.requested_new_tab_name = None;

        state.workspaces[0].close_tab(0);
        state.workspaces[0].switch_tab(0);

        assert_eq!(state.workspaces[0].tabs[0].display_name(), "1");
        assert!(state.workspaces[0].tabs[0].custom_name.is_none());

        open_new_tab_dialog(&mut state);
        assert_eq!(state.name_input, "2");
    }

    #[test]
    fn renaming_auto_tab_to_its_default_number_keeps_it_auto_named() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_add_tab(None);
        state.workspaces[0].switch_tab(1);

        open_rename_active_tab(&mut state, false);
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.workspaces[0].tabs[1].custom_name.is_none());
        assert_eq!(state.workspaces[0].tabs[1].display_name(), "2");
    }

    #[test]
    fn confirm_close_keyboard_actions_are_direct_not_focused() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.mode = Mode::ConfirmClose;
        state.selected = 1;

        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::Navigate);
        assert_eq!(state.workspaces.len(), 2);

        state.mode = Mode::ConfirmClose;
        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn context_menu_workspace_worktree_action_defers_to_app_runtime() {
        let mut state = state_with_workspaces(&["a"]);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        apply_context_menu_action(&mut state, menu, 4);

        assert_eq!(
            state.pending_worktree_action,
            Some(crate::app::state::WorktreeActionRequest::New { ws_idx: 0 })
        );
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn context_menu_workspace_duplicate_defers_to_app_runtime() {
        let mut state = state_with_workspaces(&["a"]);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        apply_context_menu_action(&mut state, menu, 7);

        assert_eq!(state.pending_duplicate_workspace, Some(0));
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn workspace_context_menu_groups_section_actions_without_toggle_item() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert!(menu.items().contains(&"--"));
        assert!(!menu.items().contains(&"Toggle section"));
        assert_eq!(menu.items()[12], "⭐ favorite");
        assert!(menu.is_separator(3));
        assert!(menu.is_separator(8));
        assert!(menu.is_separator(11));
    }

    #[test]
    fn workspace_context_menu_agent_action_defers_to_app_runtime() {
        let mut state = state_with_workspaces(&["a"]);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        apply_context_menu_action(&mut state, menu, 1);

        assert_eq!(
            state.pending_agent_start,
            Some(crate::app::state::PendingAgentStartRequest {
                target: crate::app::state::AgentStartTarget::Workspace { ws_idx: 0 },
                preset: crate::app::state::AgentPreset::Codex,
            })
        );
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn pane_context_menu_includes_layout_actions() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: crate::layout::PaneId::from_raw(1),
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert!(menu.items().contains(&"Move to left split"));
        assert!(menu.items().contains(&"Move to right split"));
        assert!(menu.items().contains(&"Move to upper split"));
        assert!(menu.items().contains(&"Move to lower split"));
        assert!(menu.items().contains(&"New Claude Code agent"));
        assert!(menu.items().contains(&"New Codex agent"));
        assert!(menu.items().contains(&"New Gemini agent"));
        assert!(menu.items().contains(&"Equalize pane sizes"));
        assert!(menu.items().contains(&"Cycle pane layout"));
        assert!(menu.items().contains(&"Rotate panes"));
        assert!(menu.items().contains(&"Rotate panes reverse"));
        assert!(menu.items().iter().filter(|item| **item == "--").count() >= 3);
        assert_eq!(menu.items().last(), Some(&"Close pane"));
    }

    #[test]
    fn pane_context_menu_agent_action_defers_to_app_runtime() {
        let mut state = state_with_workspaces(&["a"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id,
                has_manual_label: false,
                has_layout_actions: false,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        apply_context_menu_action(&mut state, menu, 2);

        assert_eq!(
            state.pending_agent_start,
            Some(crate::app::state::PendingAgentStartRequest {
                target: crate::app::state::AgentStartTarget::Pane { pane_id },
                preset: crate::app::state::AgentPreset::Claude,
            })
        );
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn single_pane_context_menu_hides_layout_actions() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: crate::layout::PaneId::from_raw(1),
                has_manual_label: false,
                has_layout_actions: false,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert!(!menu.items().contains(&"Move to left split"));
        assert!(!menu.items().contains(&"Move to right split"));
        assert!(!menu.items().contains(&"Move to upper split"));
        assert!(!menu.items().contains(&"Move to lower split"));
        assert!(!menu.items().contains(&"Equalize pane sizes"));
        assert!(!menu.items().contains(&"Cycle pane layout"));
        assert!(!menu.items().contains(&"Rotate panes"));
        assert!(!menu.items().contains(&"Rotate panes reverse"));
        assert!(!menu.items().contains(&"Zoom"));
        assert!(menu.items().contains(&"--"));
    }

    #[test]
    fn zoomed_pane_context_menu_shows_unzoom() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: crate::layout::PaneId::from_raw(1),
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: true,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert!(menu.items().contains(&"Unzoom"));
        assert!(!menu.items().contains(&"Zoom"));
        assert!(menu.items().iter().filter(|item| **item == "--").count() >= 3);
        assert_eq!(menu.items().last(), Some(&"Close pane"));
    }

    #[test]
    fn pane_context_menu_moves_clicked_pane_to_lower_split() {
        let mut state = state_with_workspaces(&["a"]);
        state.active = Some(0);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let third = state.workspaces[0].test_split(Direction::Horizontal);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: second,
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };
        let idx = menu
            .items()
            .iter()
            .position(|item| *item == "Move to lower split")
            .unwrap();

        apply_context_menu_action(&mut state, menu, idx);

        let tab = &state.workspaces[0].tabs[0];
        assert_eq!(tab.layout.focused(), second);
        assert_eq!(tab.layout.pane_ids(), vec![root, third, second]);
        let panes = tab.layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 45, 20));
        assert_eq!(panes[1].rect, Rect::new(45, 0, 45, 20));
        assert_eq!(panes[2].rect, Rect::new(0, 20, 90, 10));
        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.session_dirty);
    }

    #[test]
    fn pane_context_menu_moves_clicked_pane_to_left_split() {
        let mut state = state_with_workspaces(&["a"]);
        state.active = Some(0);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let third = state.workspaces[0].test_split(Direction::Horizontal);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: second,
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };
        let idx = menu
            .items()
            .iter()
            .position(|item| *item == "Move to left split")
            .unwrap();

        apply_context_menu_action(&mut state, menu, idx);

        let tab = &state.workspaces[0].tabs[0];
        assert_eq!(tab.layout.focused(), second);
        assert_eq!(tab.layout.pane_ids(), vec![second, root, third]);
        let panes = tab.layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 30, 30));
        assert_eq!(panes[1].rect, Rect::new(30, 0, 30, 30));
        assert_eq!(panes[2].rect, Rect::new(60, 0, 30, 30));
        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.session_dirty);
    }

    #[test]
    fn pane_context_menu_cycles_pane_layout() {
        let mut state = state_with_workspaces(&["a"]);
        state.active = Some(0);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let third = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0]
            .layout
            .arrange_all(Direction::Horizontal);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: second,
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };
        let idx = menu
            .items()
            .iter()
            .position(|item| *item == "Cycle pane layout")
            .unwrap();

        apply_context_menu_action(&mut state, menu, idx);

        let tab = &state.workspaces[0].tabs[0];
        assert_eq!(tab.layout.focused(), second);
        assert_eq!(tab.layout.pane_ids(), vec![root, second, third]);
        let panes = tab.layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 90, 10));
        assert_eq!(panes[1].rect, Rect::new(0, 10, 90, 10));
        assert_eq!(panes[2].rect, Rect::new(0, 20, 90, 10));
        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.session_dirty);
    }

    #[test]
    fn pane_context_menu_rotates_pane_ids_without_reassigning_terminals() {
        let mut state = state_with_workspaces(&["a"]);
        state.active = Some(0);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        state.workspaces[0].tabs[0].layout.focus_pane(root);
        let before_layout = state.workspaces[0].tabs[0].layout.pane_ids();
        let root_terminal_before = state.terminal_id_for_pane(0, root).unwrap();
        let second_terminal_before = state.terminal_id_for_pane(0, second).unwrap();
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: second,
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };
        let idx = menu
            .items()
            .iter()
            .position(|item| *item == "Rotate panes")
            .unwrap();

        apply_context_menu_action(&mut state, menu, idx);

        assert_eq!(
            state.workspaces[0].tabs[0].layout.pane_ids(),
            vec![second, root]
        );
        assert_ne!(state.workspaces[0].tabs[0].layout.pane_ids(), before_layout);
        assert_eq!(
            state.terminal_id_for_pane(0, root).unwrap(),
            root_terminal_before
        );
        assert_eq!(
            state.terminal_id_for_pane(0, second).unwrap(),
            second_terminal_before
        );
        assert_eq!(state.workspaces[0].tabs[0].layout.focused(), second);
        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.session_dirty);
    }

    #[test]
    fn pane_context_menu_equalizes_pane_sizes() {
        let mut state = state_with_workspaces(&["a"]);
        state.active = Some(0);
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0].layout.set_ratio_at(&[], 0.8);
        state.workspaces[0].tabs[0]
            .layout
            .set_ratio_at(&[true], 0.8);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                pane_id: second,
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };
        let idx = menu
            .items()
            .iter()
            .position(|item| *item == "Equalize pane sizes")
            .unwrap();

        apply_context_menu_action(&mut state, menu, idx);

        let panes = state.workspaces[0].tabs[0]
            .layout
            .panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect.width, 30);
        assert_eq!(panes[1].rect.width, 30);
        assert_eq!(panes[2].rect.width, 30);
        assert_eq!(state.mode, Mode::Terminal);
        assert!(state.session_dirty);
    }

    #[test]
    fn context_menu_keyboard_skips_separator_rows() {
        let mut state = state_with_workspaces(&["a"]);
        state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 0,
            y: 0,
            list: MenuListState::new(7),
        });
        state.mode = Mode::ContextMenu;

        handle_context_menu_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.context_menu.as_ref().unwrap().list.highlighted, 9);
    }
}
