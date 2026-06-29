//! Pure state mutations on AppState.
//! These don't need channels, async, or PTY runtime.

use tracing::{info, warn};

use ratatui::layout::Direction;

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::{find_in_direction, NavDirection, PaneId};
use crate::terminal::EffectiveStateChange;
use crate::workspace::WorkspaceGitStatus;

use super::state::{
    AppState, Mode, PaneFocusLocation, ToastKind, ToastNotification, ToastTarget, ViewLayout,
};

fn is_background_completion_transition(prev_state: AgentState, new_state: AgentState) -> bool {
    matches!(new_state, AgentState::Idle)
        && matches!(prev_state, AgentState::Working | AgentState::Blocked)
}

pub fn active_tab_suppresses_notifications(
    is_active_tab: bool,
    outer_terminal_focus: Option<bool>,
) -> bool {
    is_active_tab && outer_terminal_focus != Some(false)
}

pub fn notification_sound_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_background_completion_transition(prev_state, new_state)
                && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_toast_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle if is_background_completion_transition(prev_state, new_state) => {
            Some(ToastKind::Finished)
        }
        _ => None,
    }
}

pub fn notification_context(
    ws: &crate::workspace::Workspace,
    ws_idx: usize,
    pane_id: PaneId,
) -> String {
    let mut context = format!("{} · {}", ws.display_name(), ws_idx + 1);
    if ws.tabs.len() > 1 {
        if let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) {
            let tab = &ws.tabs[tab_idx];
            context.push_str(&format!(" · {}", tab.display_name()));
        }
    }
    context
}

pub fn notification_title_for_pane(state: &AppState, ws_idx: usize, _pane_id: PaneId) -> String {
    let Some(ws) = state.workspaces.get(ws_idx) else {
        return format!("{} workspace", ws_idx + 1);
    };
    format!(
        "{} {}",
        ws_idx + 1,
        ws.display_name_from(&state.terminals, &state.terminal_runtimes)
    )
}

pub fn notification_body_for_pane(
    state: &AppState,
    ws_idx: usize,
    pane_id: PaneId,
) -> Option<String> {
    let runtime = state.runtime_for_pane_in_workspace(ws_idx, pane_id)?;
    let title = notification_title_for_pane(state, ws_idx, pane_id);
    notification_body_excerpt_ignoring_chrome(&runtime.recent_unwrapped_text(80), Some(&title))
}

pub fn notification_message_for_pane(state: &AppState, ws_idx: usize, pane_id: PaneId) -> String {
    let title = notification_title_for_pane(state, ws_idx, pane_id);
    match notification_body_for_pane(state, ws_idx, pane_id) {
        Some(body) => format!("{title}: {body}"),
        None => title,
    }
}

const NOTIFICATION_BODY_MAX_CHARS: usize = 120;

fn notification_body_excerpt_ignoring_chrome(text: &str, title: Option<&str>) -> Option<String> {
    let cleaned: Vec<String> = text.lines().map(clean_notification_line).collect();
    // Everything from the composer prompt down is input-box chrome and status
    // line territory (custom statuslines included), so cut it off wholesale.
    let cutoff = cleaned
        .iter()
        .rposition(|line| is_composer_prompt_line(line))
        .unwrap_or(cleaned.len());
    let lines = &cleaned[..cutoff];

    if let Some(excerpt) = excerpt_from_last_response_marker(lines, title) {
        return Some(excerpt);
    }

    let mut blocks = Vec::new();
    let mut current_block = Vec::new();
    for line in lines {
        if line.is_empty() {
            if !current_block.is_empty() {
                blocks.push(std::mem::take(&mut current_block));
            }
            continue;
        }
        current_block.push(line.clone());
    }
    if !current_block.is_empty() {
        blocks.push(current_block);
    }
    let excerpt = blocks
        .into_iter()
        .rev()
        .flat_map(|block| block.into_iter())
        .find(|line| !is_notification_chrome_line(line, title))?;
    Some(truncate_notification_body(
        &excerpt,
        NOTIFICATION_BODY_MAX_CHARS,
    ))
}

/// Codex prefixes agent messages with "• " and Claude Code with "⏺ ". The last
/// such marker is the agent's most recent message, which makes the best
/// notification body. Lines after it are appended until chrome interrupts.
fn excerpt_from_last_response_marker(lines: &[String], title: Option<&str>) -> Option<String> {
    let start = lines.iter().rposition(|line| {
        is_agent_response_marker(line) && !is_notification_chrome_line(line, title)
    })?;
    let mut collected = String::new();
    for line in &lines[start..] {
        if line.is_empty() {
            continue;
        }
        if !collected.is_empty()
            && (is_agent_response_marker(line) || is_notification_chrome_line(line, title))
        {
            break;
        }
        let segment = line
            .strip_prefix("• ")
            .or_else(|| line.strip_prefix("⏺ "))
            .unwrap_or(line);
        if !collected.is_empty() {
            collected.push(' ');
        }
        collected.push_str(segment);
        if collected.chars().count() >= NOTIFICATION_BODY_MAX_CHARS {
            break;
        }
    }
    if collected.is_empty() {
        return None;
    }
    Some(truncate_notification_body(
        &collected,
        NOTIFICATION_BODY_MAX_CHARS,
    ))
}

/// Collapse runs of whitespace and trim box-drawing/border characters so that
/// separator rows ("────…") become empty and boxed content ("│ text │")
/// surfaces its inner text.
fn clean_notification_line(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|ch: char| ch.is_whitespace() || is_decoration_char(ch))
        .to_string()
}

fn is_decoration_char(ch: char) -> bool {
    // Box drawing (U+2500–U+257F) and block elements (U+2580–U+259F).
    ('\u{2500}'..='\u{259F}').contains(&ch)
}

fn is_composer_prompt_line(line: &str) -> bool {
    matches!(line, "❯" | "›" | ">")
}

fn is_agent_response_marker(line: &str) -> bool {
    line.starts_with("• ") || line.starts_with("⏺ ")
}

fn is_notification_chrome_line(line: &str, title: Option<&str>) -> bool {
    if title.is_some_and(|title| line == title) {
        return true;
    }
    if line.starts_with('›')
        || line.starts_with('❯')
        || line == ">"
        || line.starts_with('⎿')
        || line.starts_with('└')
        || line.starts_with("⏵⏵")
        || line.starts_with('※')
    {
        return true;
    }
    // Claude Code turn summaries ("✻ Crunched for 9m 24s · …") and spinners.
    if matches!(line.chars().next(), Some(ch) if "✻✽✶✢✳✣✤❋✺".contains(ch)) {
        return true;
    }
    // Claude Code task list summary ("8 tasks (0 done, 6 in progress, 2 open)").
    if let Some((count, rest)) = line.split_once(' ') {
        if !count.is_empty()
            && count.chars().all(|ch| ch.is_ascii_digit())
            && (rest.starts_with("tasks (") || rest.starts_with("task ("))
        {
            return true;
        }
    }

    let lower = line.to_lowercase();
    lower.starts_with("gpt-")
        || lower.starts_with("worked for ")
        || lower.contains("% left")
        || lower.contains("esc to interrupt")
        || lower.contains("ctrl+c to interrupt")
        || lower.contains("ctrl+o to expand")
        || lower.contains("? for shortcuts")
        || lower.contains("/ps to view")
        || lower.contains("tokens used")
        || lower.contains("disable recaps in /config")
}

fn truncate_notification_body(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    let prefix: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{prefix}…")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateUpdate {
    pub pane_id: PaneId,
    pub ws_idx: usize,
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub custom_status: Option<String>,
}

// ---------------------------------------------------------------------------
// Workspace operations
// ---------------------------------------------------------------------------

impl AppState {
    pub(crate) fn current_pane_focus_location(&self) -> Option<PaneFocusLocation> {
        let ws_idx = self.active?;
        let ws = self.workspaces.get(ws_idx)?;
        let tab_idx = ws.active_tab;
        let pane_id = ws.active_tab()?.layout.focused();
        Some(PaneFocusLocation {
            ws_idx,
            tab_idx,
            pane_id,
        })
    }

    fn pane_focus_location_exists(&self, location: PaneFocusLocation) -> bool {
        self.workspaces
            .get(location.ws_idx)
            .and_then(|ws| ws.tabs.get(location.tab_idx))
            .is_some_and(|tab| tab.panes.contains_key(&location.pane_id))
    }

    fn focus_pane_location_without_history(&mut self, location: PaneFocusLocation) -> bool {
        if !self.pane_focus_location_exists(location) {
            return false;
        }
        self.switch_workspace(location.ws_idx);
        self.switch_tab(location.tab_idx);
        let Some(tab) = self
            .workspaces
            .get_mut(location.ws_idx)
            .and_then(|ws| ws.tabs.get_mut(location.tab_idx))
        else {
            return false;
        };
        tab.layout.focus_pane(location.pane_id);
        self.mark_session_dirty();
        true
    }

    fn record_pane_focus_change(&mut self, before: Option<PaneFocusLocation>) {
        let Some(before) = before else {
            return;
        };
        if self.current_pane_focus_location() == Some(before) {
            return;
        }
        if self.pane_focus_back.last().copied() != Some(before) {
            self.pane_focus_back.push(before);
        }
        self.pane_focus_forward.clear();
    }

    pub(crate) fn pane_focus_history_back(&mut self) -> bool {
        let Some(current) = self.current_pane_focus_location() else {
            return false;
        };
        while let Some(previous) = self.pane_focus_back.pop() {
            if previous == current || !self.pane_focus_location_exists(previous) {
                continue;
            }
            if self.focus_pane_location_without_history(previous) {
                self.pane_focus_forward.push(current);
                return true;
            }
        }
        false
    }

    pub(crate) fn pane_focus_history_forward(&mut self) -> bool {
        let Some(current) = self.current_pane_focus_location() else {
            return false;
        };
        while let Some(next) = self.pane_focus_forward.pop() {
            if next == current || !self.pane_focus_location_exists(next) {
                continue;
            }
            if self.focus_pane_location_without_history(next) {
                self.pane_focus_back.push(current);
                return true;
            }
        }
        false
    }

    pub(crate) fn pane_is_in_active_tab(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if active_ws_idx != ws_idx {
            return false;
        }
        self.workspaces[ws_idx]
            .find_tab_index_for_pane(pane_id)
            .is_some_and(|tab_idx| tab_idx == self.workspaces[ws_idx].active_tab)
    }

    pub fn switch_workspace(&mut self, idx: usize) {
        if idx < self.workspaces.len() {
            self.selection = None;
            self.selection_autoscroll = None;
            self.active = Some(idx);
            self.selected = idx;
            let section = crate::ui::workspace_effective_section(self, idx);
            self.collapsed_workspace_sections.remove(&section);
            let workspace_id = self.workspaces[idx].id.clone();
            crate::logging::workspace_focused(&workspace_id);
            self.mark_session_dirty();
            if matches!(
                self.agent_panel_scope,
                crate::app::state::AgentPanelScope::CurrentWorkspace
            ) {
                self.agent_panel_scroll = 0;
            }
            self.ensure_workspace_visible(idx);
            if let Some(ws) = self.workspaces.get_mut(idx) {
                let active_tab = ws.active_tab;
                ws.switch_tab(active_tab);
                let tab_id = format!("{}:{}", workspace_id, active_tab + 1);
                crate::logging::tab_focused(&workspace_id, &tab_id);
            }
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }

    pub(crate) fn ensure_workspace_visible(&mut self, idx: usize) {
        if idx >= self.workspaces.len() {
            return;
        }

        if self.view.layout == ViewLayout::Mobile && self.mode == Mode::Navigate {
            self.ensure_mobile_workspace_visible(idx);
            return;
        }

        if self.sidebar_collapsed {
            return;
        }

        let mut cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
        if cards.is_empty() {
            self.workspace_scroll = idx;
            return;
        }

        if cards.iter().any(|card| card.ws_idx == idx) {
            return;
        }

        self.workspace_scroll = 0;
        cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
        if cards.iter().any(|card| card.ws_idx == idx) {
            return;
        }

        for _ in 0..self.workspaces.len() {
            let previous_scroll = self.workspace_scroll;
            self.workspace_scroll = self.workspace_scroll.saturating_add(1);
            if self.workspace_scroll == previous_scroll {
                break;
            }
            cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
            if cards.is_empty() || cards.iter().any(|card| card.ws_idx == idx) {
                break;
            }
        }
    }

    fn ensure_mobile_workspace_visible(&mut self, idx: usize) {
        let viewport = crate::ui::mobile_switcher_areas(self).viewport;
        if viewport.height == 0 {
            return;
        }

        let row_range = crate::ui::mobile_switcher_workspace_doc_range(self, idx);
        let visible_start = self.mobile_switcher_scroll;
        let visible_end = visible_start.saturating_add(viewport.height as usize);
        if row_range.start < visible_start {
            self.mobile_switcher_scroll = row_range.start;
        } else if row_range.end > visible_end {
            self.mobile_switcher_scroll = row_range.end.saturating_sub(viewport.height as usize);
        }
        self.mobile_switcher_scroll = self
            .mobile_switcher_scroll
            .min(crate::ui::mobile_switcher_max_scroll(self));
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if let Some(ws_idx) = self.active {
            self.selection = None;
            self.selection_autoscroll = None;
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            ws.switch_tab(idx);
            let workspace_id = ws.id.clone();
            let tab_id = format!("{}:{}", workspace_id, idx + 1);
            crate::logging::tab_focused(&workspace_id, &tab_id);
            self.mark_session_dirty();
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }

    pub(crate) fn mark_active_tab_seen(&mut self) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(crate::workspace::Workspace::active_tab_mut)
        else {
            return false;
        };

        let mut changed = false;
        for pane in tab.panes.values_mut() {
            if !pane.seen {
                pane.seen = true;
                changed = true;
            }
        }
        changed
    }

    pub fn next_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.active.unwrap_or(self.selected);
            let next = (current + 1) % self.workspaces.len();
            self.switch_workspace(next);
        }
    }

    pub fn previous_workspace(&mut self) {
        if !self.workspaces.is_empty() {
            let current = self.active.unwrap_or(self.selected);
            let prev = if current == 0 {
                self.workspaces.len() - 1
            } else {
                current - 1
            };
            self.switch_workspace(prev);
        }
    }

    pub fn move_workspace(&mut self, source_idx: usize, insert_idx: usize) {
        if source_idx >= self.workspaces.len() || insert_idx > self.workspaces.len() {
            return;
        }

        self.mark_session_dirty();

        let active_id = self.active.map(|idx| self.workspaces[idx].id.clone());
        let selected_id = self
            .workspaces
            .get(self.selected)
            .map(|workspace| workspace.id.clone());

        let workspace = self.workspaces.remove(source_idx);
        let target_idx = if source_idx < insert_idx {
            insert_idx.saturating_sub(1)
        } else {
            insert_idx
        }
        .min(self.workspaces.len());
        self.workspaces.insert(target_idx, workspace);

        self.active = active_id.and_then(|id| self.workspaces.iter().position(|ws| ws.id == id));
        self.selected = selected_id
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id))
            .unwrap_or(0);
        self.ensure_workspace_visible(self.selected);
    }

    pub fn scroll_tabs_left(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_sub(1);
        self.refresh_tab_bar_view();
    }

    pub fn scroll_tabs_right(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_add(1);
        self.refresh_tab_bar_view();
    }

    pub fn move_tab(&mut self, source_idx: usize, insert_idx: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if ws.move_tab(source_idx, insert_idx) {
                self.mark_session_dirty();
                self.tab_scroll_follow_active = true;
                self.refresh_tab_bar_view();
            }
        }
    }

    pub fn next_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let next = (ws.active_tab + 1) % ws.tabs.len();
                self.switch_tab(next);
            }
        }
    }

    pub fn previous_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let prev = if ws.active_tab == 0 {
                    ws.tabs.len() - 1
                } else {
                    ws.active_tab - 1
                };
                self.switch_tab(prev);
            }
        }
    }

    pub fn next_agent(&mut self) {
        self.cycle_agent_entry(true);
    }

    pub fn previous_agent(&mut self) {
        self.cycle_agent_entry(false);
    }

    pub fn focus_agent_entry(&mut self, idx: usize) -> bool {
        let entries = crate::ui::agent_panel_entries(self);
        let Some(target) = entries.get(idx) else {
            return false;
        };
        let ws_idx = target.ws_idx;
        let tab_idx = target.tab_idx;
        let pane_id = target.pane_id;

        self.switch_workspace(ws_idx);
        self.switch_tab(tab_idx);
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            if tab.panes.contains_key(&pane_id) {
                tab.layout.focus_pane(pane_id);
                self.mark_session_dirty();
                self.ensure_agent_panel_entry_visible(idx);
                return true;
            }
        }
        false
    }

    pub(crate) fn focus_pane_target(&mut self, ws_idx: usize, pane_id: PaneId) -> bool {
        let before = self.current_pane_focus_location();
        let Some(tab_idx) = self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
        else {
            return false;
        };

        self.switch_workspace(ws_idx);
        self.switch_tab(tab_idx);
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return false;
        };
        if !tab.panes.contains_key(&pane_id) {
            return false;
        }
        tab.layout.focus_pane(pane_id);
        self.mark_session_dirty();
        self.record_pane_focus_change(before);
        true
    }

    fn cycle_agent_entry(&mut self, forward: bool) {
        let entries = crate::ui::agent_panel_entries(self);
        if entries.is_empty() {
            return;
        }

        let focused = self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .and_then(crate::workspace::Workspace::focused_pane_id);
        let current_idx =
            focused.and_then(|pane_id| entries.iter().position(|entry| entry.pane_id == pane_id));
        let target_idx = match (current_idx, forward) {
            (Some(idx), true) => (idx + 1) % entries.len(),
            (Some(0), false) => entries.len() - 1,
            (Some(idx), false) => idx - 1,
            (None, true) => 0,
            (None, false) => entries.len() - 1,
        };

        self.focus_agent_entry(target_idx);
    }

    fn ensure_agent_panel_entry_visible(&mut self, idx: usize) {
        if self.sidebar_collapsed {
            return;
        }

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        let metrics = crate::ui::agent_panel_scroll_metrics(self, detail_area);
        let visible = metrics.viewport_rows;
        if visible == 0 {
            return;
        }

        if idx < self.agent_panel_scroll {
            self.agent_panel_scroll = idx;
        } else if idx >= self.agent_panel_scroll.saturating_add(visible) {
            self.agent_panel_scroll = idx.saturating_add(1).saturating_sub(visible);
        }

        let max_scroll =
            crate::ui::agent_panel_scroll_metrics(self, detail_area).max_offset_from_bottom;
        self.agent_panel_scroll = self.agent_panel_scroll.min(max_scroll);
    }

    pub(crate) fn terminal_ids_for_workspace(
        &self,
        ws_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)
            .into_iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect()
    }

    pub(crate) fn terminal_ids_for_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .into_iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect()
    }

    pub(crate) fn terminal_id_for_pane(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)?
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
    }

    pub(crate) fn remove_unattached_terminal_ids(
        &mut self,
        terminal_ids: impl IntoIterator<Item = crate::terminal::TerminalId>,
    ) {
        for terminal_id in terminal_ids {
            let still_attached = self.workspaces.iter().any(|ws| {
                ws.tabs.iter().any(|tab| {
                    tab.panes
                        .values()
                        .any(|pane| pane.attached_terminal_id == terminal_id)
                })
            });
            if !still_attached {
                self.terminals.remove(&terminal_id);
                if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
                    runtime.shutdown();
                }
            }
        }
    }

    pub(crate) fn remove_agent_ledger_workspace(&mut self, workspace_id: &str) {
        self.agent_session_ledger.remove_workspace(workspace_id);
        self.save_agent_session_ledger();
    }

    pub(crate) fn remove_agent_ledger_tab(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let workspace_id = ws.id.clone();
        let tab_id = format!("{}:{}", workspace_id, tab_idx + 1);
        self.agent_session_ledger.remove_tab(&workspace_id, &tab_id);
        self.save_agent_session_ledger();
    }

    pub(crate) fn remove_agent_ledger_pane(&mut self, ws_idx: usize, pane_id: PaneId) {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) else {
            return;
        };
        let workspace_id = ws.id.clone();
        let tab_id = format!("{}:{}", workspace_id, tab_idx + 1);
        self.agent_session_ledger
            .remove_pane(&workspace_id, &tab_id, pane_id.raw());
        self.save_agent_session_ledger();
    }

    fn agent_ledger_entry_for_pane(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<crate::persist::agent_ledger::AgentSessionLedgerEntry> {
        let ws = self.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let workspace_id = ws.id.clone();
        let tab_id = format!("{}:{}", workspace_id, tab_idx + 1);
        self.agent_session_ledger
            .get(&workspace_id, &tab_id, pane_id.raw())
            .filter(|entry| crate::agent_sessions::is_safe_session_id(&entry.session_id))
            .cloned()
    }

    fn save_agent_session_ledger(&self) {
        if let Some(path) = &self.agent_session_ledger_path {
            if let Err(err) =
                crate::persist::agent_ledger::save_to_path(path, &self.agent_session_ledger)
            {
                warn!(err = %err, "failed to save agent session ledger");
            }
        }
    }

    pub fn close_selected_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let terminal_ids = self.terminal_ids_for_workspace(self.selected);
        let workspace_id = self.workspaces[self.selected].id.clone();
        self.remove_agent_ledger_workspace(&workspace_id);
        crate::logging::workspace_closed(&workspace_id);
        self.workspaces.remove(self.selected);
        self.remove_unattached_terminal_ids(terminal_ids);
        if self.workspaces.is_empty() {
            self.active = None;
            self.selected = 0;
            self.workspace_scroll = 0;
            self.tab_scroll = 0;
            self.tab_scroll_follow_active = true;
        } else {
            if self.selected >= self.workspaces.len() {
                self.selected = self.workspaces.len() - 1;
            }
            self.active = Some(self.selected);
            self.workspace_scroll = self
                .workspace_scroll
                .min(self.workspaces.len().saturating_sub(1));
            self.ensure_workspace_visible(self.selected);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }

    fn refresh_tab_bar_view(&mut self) {
        let area = self.view.tab_bar_rect;
        let Some(ws) = self.active.and_then(|idx| self.workspaces.get(idx)) else {
            self.tab_scroll = 0;
            self.view.tab_hit_areas.clear();
            self.view.tab_scroll_left_hit_area = ratatui::layout::Rect::default();
            self.view.tab_scroll_right_hit_area = ratatui::layout::Rect::default();
            self.view.new_tab_hit_area = ratatui::layout::Rect::default();
            return;
        };

        let layout = crate::ui::compute_tab_bar_view(
            ws,
            area,
            self.tab_scroll,
            self.tab_scroll_follow_active,
            self.mouse_capture,
        );
        self.tab_scroll = layout.scroll;
        self.view.tab_hit_areas = layout.tab_hit_areas;
        self.view.tab_scroll_left_hit_area = layout.scroll_left_hit_area;
        self.view.tab_scroll_right_hit_area = layout.scroll_right_hit_area;
        self.view.new_tab_hit_area = layout.new_tab_hit_area;
    }
}

// ---------------------------------------------------------------------------
// Pane operations
// ---------------------------------------------------------------------------

impl AppState {
    pub fn navigate_pane(&mut self, direction: NavDirection) {
        let before = self.current_pane_focus_location();
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return;
        };
        let panes = if tab.zoomed {
            tab.layout.panes(self.view.terminal_area)
        } else {
            self.view.pane_infos.clone()
        };

        if let Some(focused) = panes.iter().find(|p| p.is_focused) {
            if let Some(target) = find_in_direction(focused, direction, &panes) {
                if let Some(tab) = self
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.active_tab_mut())
                {
                    tab.layout.focus_pane(target);
                    self.mark_session_dirty();
                    self.record_pane_focus_change(before);
                }
            }
        }
    }

    pub fn resize_pane(&mut self, direction: NavDirection) {
        if let Some(first) = self.view.pane_infos.first() {
            let area = self
                .view
                .pane_infos
                .iter()
                .fold(first.rect, |acc, p| acc.union(p.rect));
            if let Some(tab) = self
                .active
                .and_then(|i| self.workspaces.get_mut(i))
                .and_then(|ws| ws.active_tab_mut())
            {
                tab.layout.resize_focused(direction, 0.05, area);
                self.mark_session_dirty();
            }
        }
    }

    pub fn move_focused_pane_to_split_side(
        &mut self,
        direction: Direction,
        side: crate::layout::RootSplitSide,
    ) {
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            if tab.layout.move_focused_to_root_split_side(direction, side) {
                self.mark_session_dirty();
            }
        }
    }

    pub fn cycle_pane_layout(&mut self) {
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            if tab.layout.cycle_layout() {
                self.mark_session_dirty();
            }
        }
    }

    pub fn rotate_panes(&mut self, reverse: bool) {
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            if tab.rotate_panes(reverse) {
                self.mark_session_dirty();
            }
        }
    }

    pub fn equalize_pane_sizes(&mut self) {
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            tab.layout.equalize();
            self.mark_session_dirty();
        }
    }

    pub fn cycle_pane(&mut self, reverse: bool) {
        let before = self.current_pane_focus_location();
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            if reverse {
                tab.layout.focus_prev();
            } else {
                tab.layout.focus_next();
            }
            self.mark_session_dirty();
            self.record_pane_focus_change(before);
        }
    }

    pub fn toggle_zoom(&mut self) {
        if let Some(tab) = self
            .active
            .and_then(|i| self.workspaces.get_mut(i))
            .and_then(|ws| ws.active_tab_mut())
        {
            if tab.layout.pane_count() > 1 {
                tab.zoomed = !tab.zoomed;
                self.mark_session_dirty();
            }
        }
    }

    pub fn close_pane(&mut self) {
        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let active = self.active;
        let focused = active.and_then(|i| {
            self.workspaces
                .get(i)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (i, pane_id)))
        });
        let terminal_ids = active
            .and(focused)
            .and_then(|(i, pane_id)| self.terminal_id_for_pane(i, pane_id))
            .into_iter()
            .collect::<Vec<_>>();
        let should_close_workspace = focused
            .and_then(|(ws_idx, pane_id)| {
                self.workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
                    .map(|tab_idx| self.workspaces[ws_idx].tabs[tab_idx].layout.pane_count() <= 1)
            })
            .unwrap_or(false);
        if !should_close_workspace {
            if let Some((ws_idx, pane_id)) = focused {
                self.remove_agent_ledger_pane(ws_idx, pane_id);
            }
        }
        let should_close_workspace = active
            .and_then(|i| self.workspaces.get_mut(i))
            .is_some_and(|ws| ws.close_focused());
        if should_close_workspace {
            if let Some(active) = active {
                self.selected = active;
            }
            self.close_selected_workspace();
        } else {
            self.remove_unattached_terminal_ids(terminal_ids);
        }
    }

    pub fn close_tab(&mut self) {
        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let should_close_workspace = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .is_some_and(|ws| ws.tabs.len() <= 1);
        if should_close_workspace {
            if let Some(active) = self.active {
                self.selected = active;
            }
            self.close_selected_workspace();
            return;
        }
        if let Some(ws_idx) = self.active {
            let terminal_ids = self
                .workspaces
                .get(ws_idx)
                .map(|ws| self.terminal_ids_for_tab(ws_idx, ws.active_tab))
                .unwrap_or_default();
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            let workspace_id = ws.id.clone();
            let active_tab = ws.active_tab;
            let closing_tab_id = format!("{}:{}", workspace_id, active_tab + 1);
            self.remove_agent_ledger_tab(ws_idx, active_tab);
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            ws.close_active_tab();
            self.remove_unattached_terminal_ids(terminal_ids);
            crate::logging::tab_closed(&workspace_id, &closing_tab_id);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

impl AppState {
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_autoscroll = None;
    }

    pub(crate) fn stop_selection_autoscroll_state(&mut self) {
        self.selection_autoscroll = None;
    }

    pub fn copy_selection(&mut self) {
        let mut sel = match self.selection.take() {
            Some(sel) => sel,
            None => return,
        };
        if !sel.finish() {
            return;
        }
        let line_count = sel.selected_line_count();

        let ws_idx = match self.active {
            Some(ws_idx) if self.workspaces.get(ws_idx).is_some() => ws_idx,
            _ => return,
        };

        let text = self
            .runtime_for_pane_in_workspace(ws_idx, sel.pane_id)
            .and_then(|rt| rt.extract_selection(&sel));

        if let Some(text) = text {
            if !text.is_empty() {
                self.request_clipboard_write = Some(crate::app::state::ClipboardWriteRequest {
                    content: text.into_bytes(),
                    line_count,
                });
                info!("copied selection to clipboard");
            }
        }

        self.selection = None;
        self.selection_autoscroll = None;
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl AppState {
    pub fn apply_workspace_git_statuses(&mut self, results: Vec<WorkspaceGitStatus>) -> bool {
        let mut changed = false;
        for result in results {
            let Some(ws_idx) = self
                .workspaces
                .iter()
                .position(|ws| ws.id == result.workspace_id)
            else {
                continue;
            };

            if self.workspaces[ws_idx]
                .resolved_identity_cwd_from(&self.terminals, &self.terminal_runtimes)
                .as_ref()
                != Some(&result.resolved_identity_cwd)
            {
                continue;
            }

            let ws = &mut self.workspaces[ws_idx];
            if ws.cached_git_branch != result.branch {
                ws.cached_git_branch = result.branch;
                changed = true;
            }
            if ws.cached_git_ahead_behind != result.ahead_behind {
                ws.cached_git_ahead_behind = result.ahead_behind;
                changed = true;
            }
            if ws.cached_git_diff_stats != result.diff_stats {
                ws.cached_git_diff_stats = result.diff_stats;
                changed = true;
            }
        }
        changed
    }

    pub fn handle_app_event(&mut self, event: AppEvent) -> Vec<PaneStateUpdate> {
        match event {
            AppEvent::PaneDied { pane_id } => {
                self.handle_pane_died(pane_id);
                Vec::new()
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                self.update_available = Some(version.clone());
                self.update_install_command = install_command.clone();
                self.latest_release_notes_available = true;
                self.update_dismissed = true;
                if matches!(
                    self.toast_config.delivery,
                    crate::config::ToastDelivery::Herdr
                ) {
                    self.toast = Some(ToastNotification {
                        kind: ToastKind::UpdateInstalled,
                        title: format!("v{version} available"),
                        context: format!("detach, then run `{install_command}`"),
                        target: None,
                    });
                }
                Vec::new()
            }
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.set_detected_state(agent, state)
                })
                .into_iter()
                .collect(),
            AppEvent::AgentSessionObserved {
                pane_id,
                agent,
                session_id,
            } => {
                self.record_agent_session(pane_id, agent, session_id);
                Vec::new()
            }
            AppEvent::HookStateReported {
                pane_id,
                source,
                agent_label,
                state,
                message,
                custom_status,
                seq,
                title,
            } => {
                let updates: Vec<_> = self
                    .update_terminal_state(pane_id, |terminal| {
                        terminal.set_hook_authority_with_custom_status(
                            source,
                            agent_label,
                            state,
                            message,
                            custom_status,
                            seq,
                        )
                    })
                    .into_iter()
                    .collect();
                self.update_agent_task_title(pane_id, title);
                updates
            }
            AppEvent::HookAuthorityCleared {
                pane_id,
                source,
                seq,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.clear_hook_authority(source.as_deref(), seq)
                })
                .into_iter()
                .collect(),
            AppEvent::HookAgentReleased {
                pane_id,
                source,
                agent_label,
                seq,
                ..
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.release_agent(&source, &agent_label, seq)
                })
                .into_iter()
                .collect(),
            // Intercepted in App::handle_internal_event before reaching this
            // dispatch; never touches AppState.
            AppEvent::ClipboardWrite { .. } => Vec::new(),
            AppEvent::PaneTitleChanged { pane_id, title } => {
                self.update_pane_title(pane_id, title);
                Vec::new()
            }
            AppEvent::GitStatusRefreshed { results } => {
                self.apply_workspace_git_statuses(results);
                Vec::new()
            }
            AppEvent::WorktreeAddFinished(_) | AppEvent::WorktreeRemoveFinished(_) => Vec::new(),
        }
    }

    fn update_terminal_state<F>(&mut self, pane_id: PaneId, update: F) -> Option<PaneStateUpdate>
    where
        F: FnOnce(&mut crate::terminal::TerminalState) -> Option<EffectiveStateChange>,
    {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.pane_state(pane_id).is_some())?;
        let terminal_id = self.workspaces[ws_idx]
            .pane_state(pane_id)?
            .attached_terminal_id
            .clone();
        let change = {
            let terminal = self.terminals.get_mut(&terminal_id)?;
            update(terminal)?
        };
        let update = PaneStateUpdate {
            pane_id,
            ws_idx,
            previous_agent_label: change.previous_agent_label.clone(),
            previous_known_agent: change.previous_known_agent,
            previous_state: change.previous_state,
            agent_label: change.agent_label.clone(),
            known_agent: change.known_agent,
            state: change.state,
            custom_status: change.custom_status.clone(),
        };
        self.apply_pane_state_change(ws_idx, pane_id, &change);
        Some(update)
    }

    fn record_agent_session(
        &mut self,
        pane_id: PaneId,
        agent: crate::detect::Agent,
        session_id: String,
    ) -> bool {
        if !crate::agent_sessions::is_safe_session_id(&session_id) {
            return false;
        }
        let Some((ws_idx, tab_idx)) =
            self.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
                ws.tabs
                    .iter()
                    .position(|tab| tab.panes.contains_key(&pane_id))
                    .map(|tab_idx| (ws_idx, tab_idx))
            })
        else {
            return false;
        };
        let Some(terminal_id) = self.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .get(&pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return false;
        };
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return false;
        };
        if terminal
            .effective_known_agent()
            .is_some_and(|known| known != agent)
        {
            return false;
        }
        terminal.agent_session_id = Some(session_id.clone());
        terminal.agent_session_agent = Some(agent);
        let title = terminal
            .agent_task_title
            .clone()
            .or_else(|| terminal.pane_title.clone())
            .or_else(|| terminal.manual_label.clone());
        let cwd = terminal.cwd.clone();
        let workspace_id = self.workspaces[ws_idx].id.clone();
        self.agent_session_ledger
            .upsert(crate::persist::agent_ledger::AgentSessionLedgerEntry {
                pane_id: pane_id.raw(),
                terminal_id: terminal_id.to_string(),
                workspace_id: workspace_id.clone(),
                tab_id: format!("{}:{}", workspace_id, tab_idx + 1),
                cwd,
                agent: crate::detect::agent_label(agent).to_string(),
                session_id,
                observed_at: crate::persist::agent_ledger::now_millis(),
                source: "observed".to_string(),
                title,
            });
        self.save_agent_session_ledger();
        self.mark_session_dirty();
        true
    }

    fn update_agent_task_title(&mut self, pane_id: PaneId, title: Option<String>) -> bool {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|ws| ws.pane_state(pane_id).is_some())
        else {
            return false;
        };
        let Some(terminal_id) = self.workspaces[ws_idx]
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return false;
        };
        let changed = self
            .terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| terminal.set_agent_task_title(title));
        if changed {
            self.mark_session_dirty();
        }
        changed
    }

    fn update_pane_title(&mut self, pane_id: PaneId, title: Option<String>) -> bool {
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|ws| ws.pane_state(pane_id).is_some())
        else {
            return false;
        };
        let Some(terminal_id) = self.workspaces[ws_idx]
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return false;
        };
        self.terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| terminal.set_pane_title(title))
    }

    fn apply_pane_state_change(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        change: &EffectiveStateChange,
    ) {
        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);
        let Some(pane) = self.workspaces[ws_idx]
            .tabs
            .iter_mut()
            .find_map(|tab| tab.panes.get_mut(&pane_id))
        else {
            return;
        };

        if change.state != AgentState::Idle {
            pane.seen = true;
        } else if is_background_completion_transition(change.previous_state, change.state) {
            pane.seen = suppress_active_tab_notifications;
        }

        if self.local_sound_playback && self.sound.allows(change.known_agent) {
            if let Some(sound) = notification_sound_for_state_change(
                suppress_active_tab_notifications,
                change.previous_state,
                change.state,
            ) {
                crate::sound::play(sound, &self.sound);
            }
        }

        if matches!(
            self.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        ) {
            if let Some(kind) = notification_toast_for_state_change(
                is_active_tab,
                change.previous_state,
                change.state,
            )
            .filter(|kind| {
                self.notification_throttle
                    .allow(pane_id, *kind, std::time::Instant::now())
            }) {
                let title = notification_title_for_pane(self, ws_idx, pane_id);
                let context =
                    notification_body_for_pane(self, ws_idx, pane_id).unwrap_or_else(|| {
                        notification_context(&self.workspaces[ws_idx], ws_idx, pane_id)
                    });
                self.toast = Some(ToastNotification {
                    kind,
                    title,
                    context,
                    target: Some(ToastTarget {
                        workspace_id: self.workspaces[ws_idx].id.clone(),
                        pane_id,
                    }),
                });
            }
        }
    }

    fn handle_pane_died(&mut self, pane_id: PaneId) {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.find_tab_index_for_pane(pane_id).is_some());

        let Some(ws_idx) = ws_idx else {
            warn!(pane = pane_id.raw(), "PaneDied for unknown pane");
            return;
        };

        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.pane_id == pane_id)
        {
            self.selection = None;
            self.selection_autoscroll = None;
        }

        let pane_terminal_id = self.terminal_id_for_pane(ws_idx, pane_id);
        let ledger_entry = self.agent_ledger_entry_for_pane(ws_idx, pane_id);
        if let Some(terminal_id) = pane_terminal_id.as_ref() {
            if ledger_entry.is_some()
                || self
                    .terminals
                    .get(terminal_id)
                    .is_some_and(terminal_has_restorable_agent_session)
            {
                if let Some(terminal) = self.terminals.get_mut(terminal_id) {
                    if terminal
                        .agent_session_id
                        .as_deref()
                        .is_none_or(|id| !crate::agent_sessions::is_safe_session_id(id))
                    {
                        if let Some(entry) = ledger_entry.as_ref() {
                            terminal.agent_session_id = Some(entry.session_id.clone());
                            terminal.agent_session_agent =
                                crate::detect::parse_agent_label(&entry.agent);
                        }
                    }
                    terminal.detected_agent = None;
                    terminal.fallback_state = AgentState::Unknown;
                    terminal.hook_authority = None;
                    terminal.state = AgentState::Unknown;
                    terminal.revision = terminal.revision.saturating_add(1);
                }
                if let Some(runtime) = self.terminal_runtimes.remove(terminal_id) {
                    runtime.shutdown();
                }
                self.mark_session_dirty();
                info!(
                    pane = pane_id.raw(),
                    "preserving restorable agent pane after child exit"
                );
                return;
            }
        }
        let workspace_terminal_ids = self.terminal_ids_for_workspace(ws_idx);
        let should_close_workspace = {
            let ws = &mut self.workspaces[ws_idx];
            ws.remove_pane(pane_id)
        };
        self.mark_session_dirty();

        if should_close_workspace {
            self.workspaces.remove(ws_idx);
            self.remove_unattached_terminal_ids(workspace_terminal_ids);
            if self.workspaces.is_empty() {
                self.active = None;
                self.selected = 0;
                if self.mode == Mode::Terminal {
                    self.mode = Mode::Navigate;
                }
            } else {
                if let Some(active) = self.active {
                    if active >= self.workspaces.len() {
                        self.active = Some(self.workspaces.len() - 1);
                    }
                }
                if self.selected >= self.workspaces.len() {
                    self.selected = self.workspaces.len() - 1;
                }
            }
        } else {
            self.remove_unattached_terminal_ids(pane_terminal_id);
        }
    }
}

fn terminal_has_restorable_agent_session(terminal: &crate::terminal::TerminalState) -> bool {
    if terminal
        .agent_session_id
        .as_deref()
        .is_some_and(crate::agent_sessions::is_safe_session_id)
        && terminal.agent_session_agent.is_some()
    {
        return true;
    }
    terminal.pending_restore.as_ref().is_some_and(|restore| {
        restore
            .session_id
            .as_deref()
            .is_some_and(crate::agent_sessions::is_safe_session_id)
            && crate::detect::parse_agent_label(&restore.agent).is_some()
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;
    use ratatui::layout::Direction;

    fn app_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        for name in names {
            let ws = Workspace::test_new(name);
            state.workspaces.push(ws);
        }
        state.ensure_test_terminals();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.mode = Mode::Terminal;
        }
        state
    }

    #[test]
    fn apply_workspace_git_statuses_updates_matching_workspace() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_id = state.workspaces[0].id.clone();
        let first_cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        let second_id = state.workspaces[1].id.clone();

        let changed = state.apply_workspace_git_statuses(vec![WorkspaceGitStatus {
            workspace_id: first_id,
            resolved_identity_cwd: first_cwd,
            branch: Some("main".into()),
            ahead_behind: Some((2, 1)),
            diff_stats: Some((12, 3)),
        }]);

        assert!(changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("main"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((2, 1)));
        assert_eq!(state.workspaces[0].git_diff_stats(), Some((12, 3)));
        assert_eq!(state.workspaces[1].id, second_id);
        assert_eq!(state.workspaces[1].git_ahead_behind(), None);
        assert_eq!(state.workspaces[1].git_diff_stats(), None);
    }

    #[test]
    fn apply_workspace_git_statuses_ignores_stale_cwd() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        state.workspaces[0].cached_git_branch = Some("old".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 0));
        state.workspaces[0].cached_git_diff_stats = Some((4, 5));

        let changed = state.apply_workspace_git_statuses(vec![WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd: std::path::PathBuf::from("/definitely/not/current"),
            branch: Some("main".into()),
            ahead_behind: Some((0, 1)),
            diff_stats: Some((9, 9)),
        }]);

        assert!(!changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("old"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((1, 0)));
        assert_eq!(state.workspaces[0].git_diff_stats(), Some((4, 5)));
    }

    #[test]
    fn apply_workspace_git_statuses_clears_missing_git_status() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        let cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        state.workspaces[0].cached_git_branch = Some("main".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 2));
        state.workspaces[0].cached_git_diff_stats = Some((3, 4));

        let changed = state.apply_workspace_git_statuses(vec![WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd: cwd,
            branch: None,
            ahead_behind: None,
            diff_stats: None,
        }]);

        assert!(changed);
        assert_eq!(state.workspaces[0].branch(), None);
        assert_eq!(state.workspaces[0].git_ahead_behind(), None);
        assert_eq!(state.workspaces[0].git_diff_stats(), None);
    }

    #[test]
    fn agent_session_observed_records_pane_session_id() {
        let mut state = app_with_workspaces(&["one"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::AgentSessionObserved {
            pane_id,
            agent: Agent::Codex,
            session_id: "019ef3a2-749c-7b52-b324-2c20cb0b2379".into(),
        });

        assert_eq!(
            state.terminals[&terminal_id].agent_session_id.as_deref(),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379")
        );
        let workspace_id = state.workspaces[0].id.clone();
        let entry = state
            .agent_session_ledger
            .get(&workspace_id, &format!("{workspace_id}:1"), pane_id.raw())
            .expect("session ledger entry");
        assert_eq!(entry.agent, "codex");
        assert_eq!(entry.terminal_id, terminal_id.to_string());
        assert_eq!(entry.session_id, "019ef3a2-749c-7b52-b324-2c20cb0b2379");
        assert!(state.session_dirty);
    }

    #[test]
    fn hook_state_reported_records_pane_title() {
        let mut state = app_with_workspaces(&["one"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "codex".into(),
            agent_label: "codex".into(),
            state: AgentState::Working,
            message: None,
            custom_status: None,
            seq: None,
            title: Some("restore pane sessions".into()),
        });

        assert_eq!(
            state.terminals[&terminal_id].agent_task_title.as_deref(),
            Some("restore pane sessions")
        );
        assert!(state.session_dirty);
    }

    #[test]
    fn agent_session_observed_rejects_unsafe_id() {
        let mut state = app_with_workspaces(&["one"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::AgentSessionObserved {
            pane_id,
            agent: Agent::Codex,
            session_id: "bad;id".into(),
        });

        assert_eq!(state.terminals[&terminal_id].agent_session_id, None);
        assert!(!state.session_dirty);
    }

    #[test]
    fn update_ready_sets_explicit_upgrade_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let updates = state.handle_app_event(crate::events::AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: crate::update::update_install_command().into(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.update_available.as_deref(), Some("0.5.0"));
        assert!(state.latest_release_notes_available);
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(toast.title, "v0.5.0 available");
        assert_eq!(
            toast.context,
            format!(
                "detach, then run `{}`",
                crate::update::update_install_command()
            )
        );
    }

    fn mark_agent(state: &mut AppState, ws_idx: usize, tab_idx: usize, pane_id: PaneId) {
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        }
    }

    #[test]
    fn next_agent_cycles_agent_panel_entries_in_all_scope() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_root);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::AllWorkspaces;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 0, 0, first_second);
        mark_agent(&mut state, 1, 0, second_root);

        state.next_agent();
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_second));

        state.next_agent();
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));

        state.previous_agent();
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_second));
    }

    #[test]
    fn focus_agent_entry_uses_agent_panel_order() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_root);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::AllWorkspaces;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 0, 0, first_second);
        mark_agent(&mut state, 1, 0, second_root);

        assert!(state.focus_agent_entry(2));

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[test]
    fn focus_pane_target_switches_workspace_tab_and_pane() {
        let first = Workspace::test_new("first");
        let mut second = Workspace::test_new("second");
        let second_tab = second.test_add_tab(None);
        let target_pane = second.tabs[second_tab].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.active = Some(0);
        state.selected = 0;

        assert!(state.focus_pane_target(1, target_pane));

        assert_eq!(state.active, Some(1));
        assert_eq!(state.selected, 1);
        assert_eq!(state.workspaces[1].active_tab, second_tab);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(target_pane));
    }

    #[test]
    fn next_agent_cycles_only_current_scope_entries() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_second);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::CurrentWorkspace;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 0, 0, first_second);
        mark_agent(&mut state, 1, 0, second_root);

        state.next_agent();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));
    }

    #[test]
    fn previous_agent_keeps_wrapped_target_visible_in_agent_panel() {
        let mut workspace = Workspace::test_new("one");
        let root = workspace.tabs[0].root_pane;
        for idx in 1..8 {
            workspace.test_add_tab(Some(&format!("tab-{idx}")));
        }

        let mut state = AppState::test_new();
        state.workspaces = vec![workspace];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_scope = crate::app::state::AgentPanelScope::CurrentWorkspace;
        for tab_idx in 0..state.workspaces[0].tabs.len() {
            let pane_id = state.workspaces[0].tabs[tab_idx].root_pane;
            mark_agent(&mut state, 0, tab_idx, pane_id);
        }
        state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 14));

        state.previous_agent();

        let last_idx = state.workspaces[0].tabs.len() - 1;
        assert_eq!(state.workspaces[0].active_tab, last_idx);
        assert!(state.agent_panel_scroll > 0);
    }

    #[test]
    fn switch_workspace_updates_active_and_selected() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.switch_workspace(2);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn switch_workspace_keeps_selected_visible_in_scrolled_sidebar() {
        let mut state = app_with_workspaces(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));

        state.switch_workspace(7);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));

        assert!(state
            .view
            .workspace_card_areas
            .iter()
            .any(|card| card.ws_idx == 7));
    }

    #[test]
    fn switch_workspace_marks_panes_seen() {
        let mut state = app_with_workspaces(&["a", "b"]);
        // Mark a pane in workspace 1 as unseen
        let id = *state.workspaces[1].panes.keys().next().unwrap();
        state.workspaces[1].panes.get_mut(&id).unwrap().seen = false;

        state.switch_workspace(1);
        assert!(state.workspaces[1].panes.get(&id).unwrap().seen);
    }

    #[test]
    fn switch_workspace_out_of_bounds_is_noop() {
        let mut state = app_with_workspaces(&["a"]);
        state.switch_workspace(5);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn move_workspace_reorders_without_changing_logical_selection() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        let active_id = state.workspaces[1].id.clone();
        let selected_id = state.workspaces[2].id.clone();
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 2);
        assert_eq!(state.workspaces[state.active.unwrap()].id, active_id);
        assert_eq!(state.workspaces[state.selected].id, selected_id);
    }

    #[test]
    fn move_workspace_accepts_insert_at_end() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);

        state.move_workspace(0, state.workspaces.len());

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }

    #[test]
    fn close_workspace_adjusts_indices() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 2);
        assert_eq!(state.selected, 1);
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].custom_name.as_deref(), Some("c"));
    }

    #[test]
    fn close_last_workspace_clears_active() {
        let mut state = app_with_workspaces(&["only"]);
        state.selected = 0;
        state.close_selected_workspace();

        assert!(state.workspaces.is_empty());
        assert_eq!(state.active, None);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn close_workspace_at_end_adjusts_selected() {
        let mut state = app_with_workspaces(&["a", "b"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn pane_died_last_pane_removes_workspace() {
        let mut state = app_with_workspaces(&["a", "b"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].custom_name.as_deref(), Some("b"));
    }

    #[test]
    fn pane_died_last_workspace_enters_navigate() {
        let mut state = app_with_workspaces(&["only"]);
        state.mode = Mode::Terminal;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert!(state.workspaces.is_empty());
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn pane_died_multi_pane_keeps_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);

        state.handle_pane_died(second_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn pane_died_preserves_restorable_agent_pane() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.agent_session_agent = Some(Agent::Codex);
        terminal.agent_session_id = Some("019ef3a2-749c-7b52-b324-2c20cb0b2379".into());
        terminal.detected_agent = Some(Agent::Codex);
        terminal.state = AgentState::Working;

        state.handle_pane_died(pane_id);

        assert_eq!(state.workspaces.len(), 1);
        assert!(state.workspaces[0].pane_state(pane_id).is_some());
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.agent_session_id.as_deref(),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379")
        );
        assert_eq!(terminal.agent_session_agent, Some(Agent::Codex));
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn pane_died_preserves_agent_pane_from_ledger_when_terminal_session_is_missing() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        let workspace_id = state.workspaces[0].id.clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.state = AgentState::Working;
        state
            .agent_session_ledger
            .upsert(crate::persist::agent_ledger::AgentSessionLedgerEntry {
                pane_id: pane_id.raw(),
                terminal_id: terminal_id.to_string(),
                workspace_id: workspace_id.clone(),
                tab_id: format!("{workspace_id}:1"),
                cwd: terminal.cwd.clone(),
                agent: "codex".into(),
                session_id: "019ef3a2-749c-7b52-b324-2c20cb0b2379".into(),
                observed_at: 1,
                source: "test".into(),
                title: Some("restore pane".into()),
            });

        state.handle_pane_died(pane_id);

        assert_eq!(state.workspaces.len(), 1);
        assert!(state.workspaces[0].pane_state(pane_id).is_some());
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(
            terminal.agent_session_id.as_deref(),
            Some("019ef3a2-749c-7b52-b324-2c20cb0b2379")
        );
        assert_eq!(terminal.agent_session_agent, Some(Agent::Codex));
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn pane_died_unknown_pane_is_noop() {
        let mut state = app_with_workspaces(&["test"]);
        let fake_id = PaneId::from_raw(9999);

        state.handle_pane_died(fake_id);

        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn pane_died_unrelated_pane_preserves_selection() {
        // Two workspaces; user is selecting text in workspace 0.
        // A pane in workspace 1 dies — selection must be preserved.
        let mut state = app_with_workspaces(&["active", "bg"]);
        let active_pane = *state.workspaces[0].panes.keys().next().unwrap();
        let bg_pane = *state.workspaces[1].panes.keys().next().unwrap();

        state.selection = Some(crate::selection::Selection::anchor(active_pane, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(bg_pane);

        assert!(state.selection.is_some());
        assert!(state.selection_autoscroll.is_some());
    }

    #[test]
    fn pane_died_same_pane_clears_selection() {
        let mut state = app_with_workspaces(&["test"]);
        let first_id = state.workspaces[0].tabs[0].root_pane;
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);

        state.selection = Some(crate::selection::Selection::anchor(second_id, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(second_id);

        // first_id still alive, workspace stays, but selection was on the dying pane
        assert!(state.selection.is_none());
        assert!(state.selection_autoscroll.is_none());
        assert_eq!(state.workspaces[0].panes.len(), 1);
        assert_eq!(state.workspaces[0].panes.keys().next().unwrap(), &first_id);
    }

    #[test]
    fn state_changed_updates_pane() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
        });

        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
    }

    #[test]
    fn state_changed_idle_in_background_marks_unseen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        // First set it to Working
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        // Now transition to Idle while in background
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(!pane.seen);
    }

    #[test]
    fn active_tab_completion_marks_pane_seen() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(true);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Working;
        state.workspaces[0].panes.get_mut(&pane_id).unwrap().seen = false;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn initial_idle_in_background_stays_seen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn waiting_sound_plays_even_in_active_workspace() {
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Blocked),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn done_sound_only_plays_in_background() {
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Working, AgentState::Idle),
            Some(crate::sound::Sound::Done)
        );
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Idle),
            None
        );
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Unknown, AgentState::Idle),
            None
        );
    }

    #[test]
    fn notification_title_ignores_agent_osc_title_without_task_title() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.workspaces[1].custom_name = None;
        let pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[1]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(crate::detect::Agent::Codex);
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_pane_title(Some("planner".into()));

        assert_eq!(notification_title_for_pane(&state, 1, pane_id), "2 herdr");
    }

    #[test]
    fn notification_title_uses_agent_task_title_without_space_before_dash() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.workspaces[1].custom_name = None;
        let pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[1]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_agent_task_title(Some("planner".into()));

        assert_eq!(
            notification_title_for_pane(&state, 1, pane_id),
            "2 herdr-planner"
        );
    }

    #[test]
    fn notification_body_excerpt_uses_first_line_of_last_non_empty_block() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "old\n\n  Here is the answer  \nsecond line\n",
                None
            ),
            Some("Here is the answer".into())
        );
    }

    #[test]
    fn notification_body_excerpt_skips_trailing_codex_status_block() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "old\n\nAIがユーザーに言いたいことです。\n\n\
                 gpt-5.5 medium · HoelAI · main · Context\n\
                 59% left · 5h 96% left · weekly 87% left",
                None
            ),
            Some("AIがユーザーに言いたいことです。".into())
        );
    }

    #[test]
    fn notification_body_excerpt_skips_matching_notification_title() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "AIの本文\n\n5 HoelAI-⠸ HoelAI\n\
                 gpt-5.5 medium · HoelAI · main · Context\n\
                 59% left",
                Some("5 HoelAI-⠸ HoelAI")
            ),
            Some("AIの本文".into())
        );
    }

    #[test]
    fn notification_body_excerpt_skips_codex_status_prompt_and_turn_summary() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "  - 55_Podcast 全体: 約 430MB\n\
                   - 元WAV退避先: /Users/kazuph/Downloads/Kindle-work/retired_55_Podcast_wav_20260603\n\
                   - 退避WAV合計: 約 2.6GB\n\n\
                  変換後 .m4a は ffprobe で全16本の duration を確認済み、失敗 0 です。\n\n\
                 ─ Worked for 4m 12s • Local tools: 9 calls (138.2s) • WebSocket: 6 events send (17ms) • 273 events received (112.3s) ─\n\n\
                 › Improve documentation in @filename\n\n\
                   gpt-5.5 medium · HoelAI · main · Context 56% left · 5h 95% left · weekly 87% left",
                Some("4 HoelAI")
            ),
            Some("変換後 .m4a は ffprobe で全16本の duration を確認済み、失敗 0 です。".into())
        );
    }

    #[test]
    fn notification_body_excerpt_skips_claude_prompt_box_and_statusline() {
        // Real Claude Code bottom-of-screen layout: separator rows around the
        // composer prompt, followed by a custom statusline.
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "説明テキスト\n\n\
                 ─────────────────────────────\n\
                 ❯ \n\
                 ─────────────────────────────\n\
                 \u{20}\u{20}Fable 5 |  main +1722/-915 | ~/s/g/k/gtype\n\
                 \u{20}\u{20}¥11164 ctx ▓▓▓▓░░░░░░░░ 38%/1M\n\
                 \u{20}\u{20}⏵⏵ auto mode on · 1 shell · ← for agents",
                None
            ),
            Some("説明テキスト".into())
        );
    }

    #[test]
    fn notification_body_excerpt_returns_none_when_only_chrome_remains() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "─────────────────────────────\n\
                 ❯ \n\
                 ─────────────────────────────\n\
                 \u{20}\u{20}Fable 5 |  main +1722/-915 | ~/s/g/k/gtype",
                None
            ),
            None
        );
    }

    #[test]
    fn notification_body_excerpt_prefers_last_claude_response_marker() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "⏺ Bash(npx reviw REPORT.md --port 4990)\n\
                 \u{20}\u{20}⎿  Running in the background (↓ to manage)\n\n\
                 ⏺ マスター、Round 4 きたよ〜！今回は自信作💕 reviw開いてるから見て！\n\n\
                 \u{20}\u{20}Round 4 対応報告\n\n\
                 ✻ Crunched for 9m 24s · 1 shell still running\n\n\
                 ※ recap: gtypeのmacOS HUDとAndroid UI/アニメ刷新中。 (disable recaps in /config)\n\n\
                 \u{20}\u{20}8 tasks (0 done, 6 in progress, 2 open)\n\
                 \u{20}\u{20}◼ macOS: 録音中・APIリクエスト中のHUDアニメーションを改善\n\n\
                 ─────────────────────────────\n\
                 ❯ \n\
                 ─────────────────────────────\n\
                 \u{20}\u{20}Fable 5 |  main +1722/-915 | ~/s/g/k/gtype",
                Some("12 gtype")
            ),
            Some(
                "マスター、Round 4 きたよ〜！今回は自信作💕 reviw開いてるから見て！ Round 4 対応報告"
                    .into()
            )
        );
    }

    #[test]
    fn notification_body_excerpt_prefers_last_codex_response_marker() {
        assert_eq!(
            notification_body_excerpt_ignoring_chrome(
                "• Ran git status --short && git log -1 --oneline\n\
                 \u{20}\u{20}└ 9cf357c docs: add Gemma 4 12B benchmark results\n\n\
                 ─────────────────────────────\n\n\
                 • 完了です。ベンチ結果を README.md:305 に追記し、push 済みです。\n\n\
                 \u{20}\u{20}追記した実測値は、共有 API の起動中モデルで取得しました。\n\n\
                 ─ Worked for 3m 01s • Local tools: 22 calls (22.0s) ─────────────\n\n\
                 › Run /review on my current changes\n\n\
                 \u{20}\u{20}gpt-5.5 medium · qwen-3.5-chat · main · Context 71% left",
                Some("6 qwen-3.5-chat")
            ),
            Some(
                "完了です。ベンチ結果を README.md:305 に追記し、push 済みです。 追記した実測値は、共有 API の起動中モデルで取得しました。"
                    .into()
            )
        );
    }

    #[test]
    fn background_waiting_sets_attention_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "2 background");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn flapping_blocked_state_does_not_refire_attention_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });
        assert!(state.toast.is_some());
        state.toast = None;

        // Detection flaps working and immediately back to blocked.
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        assert_eq!(state.toast, None);
    }

    #[test]
    fn finished_toast_does_not_bury_recent_attention_toast() {
        let mut state = app_with_workspaces(&["active", "asking", "finishing"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let asking_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let finishing_pane_id = *state.workspaces[2].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: finishing_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: asking_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });
        let attention_toast = state.toast.clone().expect("attention toast");
        assert_eq!(attention_toast.kind, ToastKind::NeedsAttention);

        // Another pane finishing right after must not replace the question.
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: finishing_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        assert_eq!(state.toast, Some(attention_toast));
    }

    #[test]
    fn hook_reported_unknown_agent_sets_toast_title_from_label() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::HookStateReported {
            pane_id: bg_pane_id,
            source: "custom:hermes".into(),
            agent_label: "hermes".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: None,
            title: None,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "2 background");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn background_idle_sets_finished_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Droid),
            state: AgentState::Idle,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::Finished);
        assert_eq!(toast.title, "2 background");
        assert_eq!(toast.context, "background · 2");
        let target = toast.target.as_ref().expect("toast target");
        assert_eq!(&target.workspace_id, &state.workspaces[1].id);
        assert_eq!(target.pane_id, bg_pane_id);
    }

    #[test]
    fn background_toast_includes_tab_name_when_workspace_has_multiple_tabs() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[1].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[1].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "2 background");
        assert_eq!(toast.context, "background · 2 · logs");
    }

    #[test]
    fn background_tab_in_active_workspace_still_sets_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[0].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[0].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "1 active");
        assert_eq!(toast.context, "active · 1 · logs");
    }

    #[test]
    fn active_workspace_active_tab_does_not_set_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_workspace_active_tab_keeps_herdr_toast_suppressed_when_outer_terminal_is_unfocused() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(false);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_tab_suppression_preserves_unknown_focus_behavior() {
        assert!(active_tab_suppresses_notifications(true, None));
        assert!(active_tab_suppresses_notifications(true, Some(true)));
        assert!(!active_tab_suppresses_notifications(true, Some(false)));
        assert!(!active_tab_suppresses_notifications(false, None));
    }

    #[test]
    fn update_ready_sets_manual_update_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let updates = state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: crate::update::update_install_command().into(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.update_available.as_deref(), Some("0.5.0"));
        assert!(state.latest_release_notes_available);
        assert!(state.update_dismissed);
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(toast.kind, ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "v0.5.0 available");
        assert_eq!(
            toast.context,
            format!(
                "detach, then run `{}`",
                crate::update::update_install_command()
            )
        );
    }

    #[test]
    fn update_ready_uses_event_install_command_in_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "brew update && brew upgrade herdr".into(),
        });

        assert_eq!(
            state.update_install_command,
            "brew update && brew upgrade herdr"
        );
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(
            toast.context,
            "detach, then run `brew update && brew upgrade herdr`"
        );
    }

    #[test]
    fn toggle_zoom_works() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        assert!(!state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn toggle_zoom_single_pane_noop() {
        let mut state = app_with_workspaces(&["test"]);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn navigate_pane_changes_focus_while_zoomed() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, root);

        state.navigate_pane(NavDirection::Right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, right);
        assert!(state.view.pane_infos[0].inner_rect.x > state.view.pane_infos[0].rect.x);
    }

    #[test]
    fn close_pane_removes_from_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        assert_eq!(state.workspaces[0].panes.len(), 2);

        state.close_pane();
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn close_pane_removes_unattached_terminal_state() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();

        state.close_pane();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_pane_removes_pane_agent_ledger_entry() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0].layout.focus_pane(pane_id);
        state.ensure_test_terminals();
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        let workspace_id = state.workspaces[0].id.clone();
        state
            .agent_session_ledger
            .upsert(crate::persist::agent_ledger::AgentSessionLedgerEntry {
                pane_id: pane_id.raw(),
                terminal_id: terminal_id.to_string(),
                workspace_id: workspace_id.clone(),
                tab_id: format!("{workspace_id}:1"),
                cwd: state.terminals[&terminal_id].cwd.clone(),
                agent: "codex".into(),
                session_id: "019ef3a2-749c-7b52-b324-2c20cb0b2379".into(),
                observed_at: 1,
                source: "test".into(),
                title: None,
            });

        state.close_pane();

        assert!(state
            .agent_session_ledger
            .get(&workspace_id, &format!("{workspace_id}:1"), pane_id.raw())
            .is_none());
    }

    #[test]
    fn close_tab_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["test"]);
        let tab_idx = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        state.workspaces[0].switch_tab(tab_idx);
        let pane_id = state.workspaces[0].tabs[tab_idx].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();

        state.close_tab();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_workspace_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let terminal_id = state
            .terminal_id_for_pane(0, state.workspaces[0].tabs[0].root_pane)
            .unwrap();

        state.close_selected_workspace();

        assert!(!state.terminals.contains_key(&terminal_id));
    }

    #[test]
    fn close_workspace_removes_workspace_agent_ledger_entries() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        let workspace_id = state.workspaces[0].id.clone();
        state
            .agent_session_ledger
            .upsert(crate::persist::agent_ledger::AgentSessionLedgerEntry {
                pane_id: pane_id.raw(),
                terminal_id: terminal_id.to_string(),
                workspace_id: workspace_id.clone(),
                tab_id: format!("{workspace_id}:1"),
                cwd: state.terminals[&terminal_id].cwd.clone(),
                agent: "claude".into(),
                session_id: "11111111-2222-3333-4444-555555555555".into(),
                observed_at: 1,
                source: "test".into(),
                title: None,
            });

        state.close_selected_workspace();

        assert!(state
            .agent_session_ledger
            .entries
            .values()
            .all(|entry| entry.workspace_id != workspace_id));
    }

    #[test]
    fn close_tab_last_tab_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_tab();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
    }

    #[test]
    fn close_pane_last_pane_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_pane();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
    }
}
