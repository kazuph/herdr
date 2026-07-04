use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{agent_icon, state_label, state_label_color, state_summary_icon};
use super::widgets::panel_contrast_fg;
use crate::app::state::{
    AgentPanelScope, Palette, SidebarWidthPreset, SidebarWidthToggleRects, WorkspacePanelDensity,
};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub global_pane_id: String,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub custom_status: Option<String>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_current_workspace_idx(app: &AppState) -> Option<usize> {
    if matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameWorkspace
            | Mode::RenamePane
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ConfirmDanger
            | Mode::ContextMenu
            | Mode::Settings
            | Mode::GlobalMenu
            | Mode::KeybindHelp
            | Mode::ProductAnnouncement
    ) {
        Some(app.selected)
    } else {
        app.active
    }
}

fn agent_panel_toggle_label(scope: AgentPanelScope) -> &'static str {
    match scope {
        AgentPanelScope::CurrentWorkspace => "[current]",
        AgentPanelScope::AllWorkspaces => "[all]",
        AgentPanelScope::SortedAllWorkspaces => "[sort]",
    }
}

pub(crate) fn workspace_section_is_expanded(
    app: &AppState,
    section: crate::workspace::WorkspaceSection,
) -> bool {
    !app.collapsed_workspace_sections.contains(&section)
}

pub(crate) fn workspace_effective_section(
    app: &AppState,
    ws_idx: usize,
) -> crate::workspace::WorkspaceSection {
    app.workspaces
        .get(ws_idx)
        .map(|ws| ws.section)
        .unwrap_or_default()
}

fn workspace_is_in_expanded_section(app: &AppState, ws_idx: usize) -> bool {
    ws_idx < app.workspaces.len()
        && workspace_section_is_expanded(app, workspace_effective_section(app, ws_idx))
}

pub(crate) fn sectioned_workspace_indices(
    app: &AppState,
) -> Vec<(crate::workspace::WorkspaceSection, Vec<usize>)> {
    crate::workspace::WorkspaceSection::ALL
        .into_iter()
        .filter_map(|section| {
            let indices = section_indices(app, section);
            (!indices.is_empty()).then_some((section, indices))
        })
        .collect()
}

fn sidebar_workspace_sections(
    app: &AppState,
) -> Vec<(crate::workspace::WorkspaceSection, Vec<usize>)> {
    sectioned_workspace_indices(app)
}

fn section_indices(app: &AppState, section: crate::workspace::WorkspaceSection) -> Vec<usize> {
    (0..app.workspaces.len())
        .filter(|idx| workspace_effective_section(app, *idx) == section)
        .collect()
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, scope: AgentPanelScope) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_toggle_label(scope);
    let width = label.chars().count() as u16;
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

fn workspace_panel_density_label(density: WorkspacePanelDensity) -> &'static str {
    match density {
        WorkspacePanelDensity::Full => "[full]",
        WorkspacePanelDensity::Slim => "[slim]",
    }
}

pub(crate) fn workspace_panel_density_toggle_rect(
    area: Rect,
    density: WorkspacePanelDensity,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let label = workspace_panel_density_label(density);
    let width = label.chars().count() as u16;
    Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1)
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    let entries: Vec<AgentPanelEntry> = match app.agent_panel_scope {
        AgentPanelScope::CurrentWorkspace => {
            let Some(ws_idx) = agent_panel_current_workspace_idx(app) else {
                return Vec::new();
            };
            if !workspace_is_in_expanded_section(app, ws_idx) {
                return Vec::new();
            }
            let Some(ws) = app.workspaces.get(ws_idx) else {
                return Vec::new();
            };
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(|detail| AgentPanelEntry {
                    global_pane_id: pane_global_id(detail.pane_id),
                    ws_idx,
                    tab_idx: detail.tab_idx,
                    pane_id: detail.pane_id,
                    primary_label: detail.label,
                    primary_tab_label: None,
                    agent_label: None,
                    state: detail.state,
                    seen: detail.seen,
                    custom_status: detail.custom_status,
                })
                .collect()
        }
        AgentPanelScope::AllWorkspaces | AgentPanelScope::SortedAllWorkspaces => app
            .workspaces
            .iter()
            .enumerate()
            .filter(|(ws_idx, _)| workspace_is_in_expanded_section(app, *ws_idx))
            .flat_map(|(ws_idx, ws)| {
                let multi_tab = ws.tabs.len() > 1;
                let workspace_label = ws.display_name_from(&app.terminals, &app.terminal_runtimes);
                ws.pane_details(&app.terminals)
                    .into_iter()
                    .map(move |detail| AgentPanelEntry {
                        global_pane_id: pane_global_id(detail.pane_id),
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: multi_tab.then_some(detail.tab_label),
                        agent_label: Some(detail.agent_label),
                        state: detail.state,
                        seen: detail.seen,
                        custom_status: detail.custom_status,
                    })
            })
            .collect(),
    };

    if matches!(app.agent_panel_scope, AgentPanelScope::SortedAllWorkspaces) {
        let mut sortable = entries
            .into_iter()
            .enumerate()
            .map(|(idx, entry)| (agent_sort_bucket(entry.state, entry.seen), idx, entry))
            .collect::<Vec<_>>();
        sortable.sort_by_key(|(bucket, idx, _)| (*bucket, *idx));
        return sortable.into_iter().map(|(_, _, entry)| entry).collect();
    }

    entries
}

fn agent_sort_bucket(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 0,
        (AgentState::Idle, false) => 1,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 3,
        (AgentState::Unknown, _) => 4,
    }
}

fn pane_global_id(pane_id: crate::layout::PaneId) -> String {
    format!("%{}", pane_id.raw())
}

fn truncate_text(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

fn format_agent_panel_primary_label(entry: &AgentPanelEntry, max_width: usize) -> String {
    let Some(tab_label) = entry.primary_tab_label.as_deref() else {
        return truncate_text(&entry.primary_label, max_width);
    };

    let separator = " · ";
    let separator_width = separator.chars().count();
    if max_width <= separator_width + 2 {
        return truncate_text(
            &format!("{}{}{}", entry.primary_label, separator, tab_label),
            max_width,
        );
    }

    let available = max_width.saturating_sub(separator_width);
    let min_tab = 4.min(available.saturating_sub(1)).max(1);
    let preferred_workspace = ((available * 2) / 3).max(1);
    let mut workspace_budget = preferred_workspace
        .min(available.saturating_sub(min_tab))
        .max(1);
    let mut tab_budget = available.saturating_sub(workspace_budget);

    let workspace_len = entry.primary_label.chars().count();
    let tab_len = tab_label.chars().count();

    if workspace_len < workspace_budget {
        let spare = workspace_budget - workspace_len;
        workspace_budget = workspace_len;
        tab_budget = (tab_budget + spare).min(available.saturating_sub(workspace_budget));
    }
    if tab_len < tab_budget {
        let spare = tab_budget - tab_len;
        tab_budget = tab_len;
        workspace_budget = (workspace_budget + spare).min(available.saturating_sub(tab_budget));
    }

    format!(
        "{}{}{}",
        truncate_text(&entry.primary_label, workspace_budget),
        separator,
        truncate_text(tab_label, tab_budget)
    )
}

fn agent_panel_id_label(entry: &AgentPanelEntry) -> String {
    entry.global_pane_id.clone()
}

fn workspace_row_height(app: &AppState, _ws: &crate::workspace::Workspace) -> u16 {
    if app.workspace_panel_density == WorkspacePanelDensity::Full {
        2
    } else {
        1
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let mut truncated = text.chars().take(max_width - 1).collect::<String>();
    truncated.push('…');
    truncated
}

pub(super) fn workspace_upstream_labels(ws: &crate::workspace::Workspace) -> Vec<(String, bool)> {
    ws.git_ahead_behind()
        .map(|(ahead, behind)| {
            let mut parts = Vec::new();
            if ahead > 0 {
                parts.push((format!("↑{}", ahead), true));
            }
            if behind > 0 {
                parts.push((format!("↓{}", behind), false));
            }
            parts
        })
        .unwrap_or_default()
}

pub(super) fn workspace_diff_labels(ws: &crate::workspace::Workspace) -> Vec<(String, bool)> {
    ws.git_diff_stats()
        .map(|(additions, deletions)| {
            let mut parts = Vec::new();
            if additions > 0 {
                parts.push((format!("+{}", additions), true));
            }
            if deletions > 0 {
                parts.push((format!("-{}", deletions), false));
            }
            parts
        })
        .unwrap_or_default()
}

pub(super) fn push_git_labels(
    spans: &mut Vec<Span<'static>>,
    upstream_labels: Vec<(String, bool)>,
    diff_labels: Vec<(String, bool)>,
    p: &Palette,
) {
    let mut needs_separator = false;
    for (label, is_ahead) in upstream_labels {
        let color = if is_ahead { p.green } else { p.red };
        if needs_separator {
            spans.push(Span::styled(" ", Style::default()));
        }
        spans.push(Span::styled(label, Style::default().fg(color)));
        needs_separator = true;
    }
    for (label, is_addition) in diff_labels {
        let color = if is_addition { p.green } else { p.red };
        if needs_separator {
            spans.push(Span::styled(" ", Style::default()));
        }
        spans.push(Span::styled(label, Style::default().fg(color)));
        needs_separator = true;
    }
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let mut skipped = 0usize;
    for (section, indices) in sidebar_workspace_sections(app) {
        if used_rows.saturating_add(2) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(2);

        if !workspace_section_is_expanded(app, section) {
            continue;
        }

        for ws_idx in indices {
            if skipped < scroll {
                skipped += 1;
                continue;
            }
            let ws = &app.workspaces[ws_idx];
            let needed = workspace_row_height(app, ws).saturating_add(1);
            if used_rows.saturating_add(needed) > body.height {
                return visible;
            }
            used_rows = used_rows.saturating_add(needed);
            visible += 1;
        }
    }
    visible
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let viewport_rows = workspace_list_visible_count(app, area, app.workspace_scroll);
    let total_rows: usize = sidebar_workspace_sections(app)
        .into_iter()
        .filter(|(section, _)| workspace_section_is_expanded(app, *section))
        .map(|(_, indices)| indices.len())
        .sum();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.workspace_scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS + 1 {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

pub(crate) fn workspace_section_new_button_rect(header: Rect) -> Rect {
    const LABEL: &str = "[new]";
    let width = LABEL.len() as u16;
    if header.width < width + 1 || header.height == 0 {
        return Rect::default();
    }
    Rect::new(
        header.x + header.width.saturating_sub(width),
        header.y,
        width,
        1,
    )
}

fn sidebar_width_toggle_footer_rect(area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    )
}

pub(crate) fn sidebar_width_toggle_rects(area: Rect) -> SidebarWidthToggleRects {
    let footer = sidebar_width_toggle_footer_rect(area);
    if footer.width == 0 {
        return SidebarWidthToggleRects::default();
    }

    let button_w = 8;
    if footer.width < button_w {
        return SidebarWidthToggleRects::default();
    }
    SidebarWidthToggleRects {
        button: Rect::new(footer.x, footer.y, button_w, 1),
    }
}

fn agent_panel_visible_count(area: Rect) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height < 2 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    while used_rows.saturating_add(2) <= body.height {
        used_rows = used_rows.saturating_add(2);
        visible += 1;
        if used_rows < body.height {
            used_rows = used_rows.saturating_add(1);
        }
    }
    visible
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let viewport_rows = agent_panel_visible_count(area);
    let total_rows = agent_panel_entries(app).len();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.agent_panel_scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

pub(crate) fn compute_workspace_section_header_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceSectionHeaderArea> {
    compute_workspace_list_areas(app, area).1
}

fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (
    Vec<crate::app::state::WorkspaceCardArea>,
    Vec<crate::app::state::WorkspaceSectionHeaderArea>,
) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let mut section_areas = Vec::new();
    let mut skipped = 0usize;

    for (section, indices) in sidebar_workspace_sections(app) {
        if row_y >= body_bottom {
            break;
        }
        section_areas.push(crate::app::state::WorkspaceSectionHeaderArea {
            section,
            rect: Rect::new(body.x, row_y, body.width, 1),
        });
        row_y = row_y.saturating_add(2);

        if !workspace_section_is_expanded(app, section) {
            continue;
        }

        for ws_idx in indices {
            if skipped < app.workspace_scroll {
                skipped += 1;
                continue;
            }
            let ws = &app.workspaces[ws_idx];
            let row_height = workspace_row_height(app, ws);
            if row_y.saturating_add(row_height).saturating_add(1) > body_bottom {
                return (cards, section_areas);
            }
            cards.push(crate::app::state::WorkspaceCardArea {
                ws_idx,
                rect: Rect::new(body.x, row_y, body.width, row_height),
            });
            row_y = row_y.saturating_add(row_height + 1);
        }
    }

    (cards, section_areas)
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_summary_icon(agg_state, agg_seen, app.spinner_tick, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };
    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        if let Some(ws_idx) = detail_ws_idx {
            if let Some(ws) = app.workspaces.get(ws_idx) {
                for (detail_idx, detail) in ws.pane_details(&app.terminals).iter().enumerate() {
                    let y = detail_content_area.y + detail_idx as u16;
                    if y >= detail_content_area.y + detail_content_area.height {
                        break;
                    }
                    let pane_num = ws
                        .public_pane_number(detail.pane_id)
                        .unwrap_or(detail_idx + 1);
                    let pane_style = Style::default().fg(p.overlay0);
                    let (icon, icon_style) =
                        agent_icon(detail.state, detail.seen, app.spinner_tick, p);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("{pane_num}"), pane_style),
                            Span::styled(" ", pane_style),
                            Span::styled(icon, icon_style),
                        ])),
                        Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
                    );
                }
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
}

fn workspace_drop_indicator_row_for_section(
    app: &AppState,
    area: Rect,
    section: crate::workspace::WorkspaceSection,
    insert_idx: usize,
) -> Option<u16> {
    let cards = app
        .view
        .workspace_card_areas
        .iter()
        .copied()
        .filter(|card| workspace_effective_section(app, card.ws_idx) == section)
        .collect::<Vec<_>>();
    workspace_drop_indicator_row(&cards, area, insert_idx)
}

pub(super) fn render_sidebar(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    render_workspace_list(app, frame, ws_area, is_navigating);
    render_agent_detail(app, frame, detail_area);
    render_selection_copy_status(app, frame, area);
    render_sidebar_toggle(app, frame, area, false, p);
}

fn render_selection_copy_status(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(status) = app.selection_copy_status else {
        return;
    };
    if area.width <= 1 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    let line_label = if status.line_count == 1 {
        "line"
    } else {
        "lines"
    };
    let message = format!(" Copied {} {line_label}", status.line_count);
    let rect = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width.saturating_sub(1),
        1,
    );
    let buf = frame.buffer_mut();
    for x in rect.x..rect.x + rect.width {
        buf[(x, rect.y)].set_style(Style::default().bg(p.surface_dim));
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&message, rect.width as usize),
            Style::default()
                .fg(p.green)
                .bg(p.surface_dim)
                .add_modifier(Modifier::BOLD),
        )),
        rect,
    );
}

fn render_workspace_list(app: &AppState, frame: &mut Frame, area: Rect, is_navigating: bool) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            source_ws_idx,
            insert_idx: Some(insert_idx),
            target_section,
            ..
        }) => {
            let section =
                target_section.unwrap_or_else(|| workspace_effective_section(app, *source_ws_idx));
            workspace_drop_indicator_row_for_section(app, area, section, *insert_idx)
        }
        _ => None,
    };
    let target_section = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            target_section: Some(section),
            ..
        }) => Some(*section),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let toggle_rect = workspace_panel_density_toggle_rect(area, app.workspace_panel_density);
        if toggle_rect != Rect::default() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    workspace_panel_density_label(app.workspace_panel_density),
                    Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Right),
                toggle_rect,
            );
        }
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;
    for section_area in &app.view.workspace_section_header_areas {
        let expanded = workspace_section_is_expanded(app, section_area.section);
        let arrow = if expanded { "▾" } else { "▸" };
        let targeted = target_section == Some(section_area.section);
        let style = if targeted {
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
        };
        if targeted {
            for x in section_area.rect.x..section_area.rect.x + section_area.rect.width {
                frame.buffer_mut()[(x, section_area.rect.y)].set_style(style);
            }
        }
        let new_rect = workspace_section_new_button_rect(section_area.rect);
        let label_width = if new_rect == Rect::default() {
            section_area.rect.width
        } else {
            new_rect
                .x
                .saturating_sub(section_area.rect.x)
                .saturating_sub(1)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(arrow, style),
                Span::styled(" ", style),
                Span::styled(
                    truncate_to_width(section_area.section.label(), label_width as usize),
                    style,
                ),
            ])),
            Rect::new(
                section_area.rect.x,
                section_area.rect.y,
                label_width,
                section_area.rect.height,
            ),
        );
        if new_rect != Rect::default() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "[new]",
                    Style::default()
                        .fg(p.text)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                )),
                new_rect,
            );
        }
    }

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        if is_active {
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                buf[(card.rect.x, y)].set_symbol("▌");
                buf[(card.rect.x, y)].set_style(Style::default().fg(p.accent));
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) = state_summary_icon(agg_state, agg_seen, app.spinner_tick, p);
        let display_name = ws.display_name_from(&app.terminals, &app.terminal_runtimes);
        let workspace_number = format!("{} ", i + 1);
        let content_rect = Rect::new(
            card.rect.x.saturating_add(1),
            row_y,
            card.rect.width.saturating_sub(1),
            1,
        );
        let mut line1 = vec![
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(
                workspace_number.clone(),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ),
            Span::styled(display_name.clone(), name_style),
        ];
        if row_height == 1 {
            if let Some(branch) = ws.branch() {
                let mut upstream_labels = workspace_upstream_labels(ws);
                let mut diff_labels = workspace_diff_labels(ws);
                let prefix_width = 2 + workspace_number.chars().count();
                // Keep the last column free for the scrollbar overlay.
                let available = (content_rect.width as usize)
                    .saturating_sub(prefix_width + 1)
                    .max(1);
                // The workspace name always wins the row; git metadata only
                // gets leftover width, shedding diff stats first, then
                // upstream arrows, then the branch itself.
                let name_width = display_name.chars().count().min(available);
                line1[3] = Span::styled(truncate_to_width(&display_name, name_width), name_style);
                let remaining = available.saturating_sub(name_width);

                let labels_width = |labels: &[(String, bool)]| -> usize {
                    labels
                        .iter()
                        .map(|(label, _)| label.chars().count() + 1)
                        .sum()
                };
                const MIN_BRANCH_WIDTH: usize = 5;
                let min_branch = branch.chars().count().min(MIN_BRANCH_WIDTH) + 1;
                if labels_width(&upstream_labels) + labels_width(&diff_labels) + min_branch
                    > remaining
                {
                    diff_labels.clear();
                }
                if labels_width(&upstream_labels) + min_branch > remaining {
                    upstream_labels.clear();
                }
                let branch_budget = remaining
                    .saturating_sub(labels_width(&upstream_labels) + labels_width(&diff_labels))
                    .saturating_sub(1);
                let branch_display = if branch_budget + 1 >= min_branch {
                    truncate_to_width(&branch, branch_budget)
                } else {
                    String::new()
                };
                if !upstream_labels.is_empty() || !diff_labels.is_empty() {
                    line1.push(Span::styled(" ", Style::default()));
                }
                push_git_labels(&mut line1, upstream_labels, diff_labels, p);
                if !branch_display.is_empty() {
                    let branch_color = if selected || is_active {
                        p.mauve
                    } else {
                        p.overlay0
                    };
                    line1.push(Span::styled(" ", Style::default()));
                    line1.push(Span::styled(
                        branch_display,
                        Style::default().fg(branch_color),
                    ));
                }
            }
        }

        frame.render_widget(Paragraph::new(Line::from(line1)), content_rect);

        if row_height > 1 && row_y + 1 < list_bottom {
            let mut spans = vec![Span::styled("    ", Style::default())];
            if let Some(branch) = ws.branch() {
                let upstream_labels = workspace_upstream_labels(ws);
                let diff_labels = workspace_diff_labels(ws);
                let reserved = upstream_labels
                    .iter()
                    .chain(diff_labels.iter())
                    .map(|(label, _)| label.chars().count())
                    .sum::<usize>()
                    + upstream_labels.len()
                    + diff_labels.len();
                let max_branch_len = (content_rect.width as usize).saturating_sub(4 + reserved);
                let branch_display = truncate_to_width(&branch, max_branch_len);
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                let has_labels = !upstream_labels.is_empty() || !diff_labels.is_empty();
                push_git_labels(&mut spans, upstream_labels, diff_labels, p);
                if has_labels {
                    spans.push(Span::styled(" ", Style::default()));
                }
                spans.push(Span::styled(
                    branch_display,
                    Style::default().fg(branch_color),
                ));
            } else {
                let label_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                spans.push(Span::styled("nogit", Style::default().fg(label_color)));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(content_rect.x, row_y + 1, content_rect.width, 1),
            );
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled("[new]", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("[menu]", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled(
                "[menu]",
                Style::default().fg(p.overlay0),
            )])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

fn render_agent_detail(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_scope);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_toggle_label(app.agent_panel_scope),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries(app);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        render_sidebar_width_toggle(app, frame, area);
        return;
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for detail in details.iter().skip(app.agent_panel_scroll) {
        if row_y.saturating_add(1) >= body_bottom {
            break;
        }

        // Check if this agent entry corresponds to the active session
        let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);

        let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
        let label_color = state_label_color(detail.state, detail.seen, p);
        let label = state_label(detail.state, detail.seen);

        let row_style = if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };

        let name_style = if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        let status_style = if is_active {
            Style::default().fg(label_color)
        } else {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        };
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

        let id_label = agent_panel_id_label(detail);
        let id_width = id_label.chars().count();
        let primary_label = format_agent_panel_primary_label(
            detail,
            (body.width as usize).saturating_sub(4 + id_width),
        );
        let name_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(
                id_label,
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
            Span::styled(primary_label, name_style),
        ]);
        frame.render_widget(
            Paragraph::new(name_line).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        let mut status_spans = vec![
            Span::styled("   ", Style::default()),
            Span::styled(label, status_style),
        ];
        if let Some(agent_label) = &detail.agent_label {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(agent_label, agent_style));
        }
        if let Some(custom_status) = &detail.custom_status {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(custom_status.clone(), agent_style));
        }
        frame.render_widget(
            Paragraph::new(Line::from(status_spans)).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        if row_y < body_bottom {
            row_y += 1;
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
    render_sidebar_width_toggle(app, frame, area);
}

fn render_sidebar_width_toggle(app: &AppState, frame: &mut Frame, area: Rect) {
    let footer = sidebar_width_toggle_footer_rect(area);
    if footer == Rect::default() {
        return;
    }
    let p = &app.palette;
    let rect = app.view.sidebar_width_toggle_rects.button;
    if rect == Rect::default() {
        return;
    }
    let preset = current_sidebar_width_preset(app);
    let style = Style::default()
        .fg(p.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    frame.render_widget(
        Paragraph::new(Span::styled(preset.button_label(), style)),
        rect,
    );
}

fn current_sidebar_width_preset(app: &AppState) -> SidebarWidthPreset {
    let narrow = SidebarWidthPreset::Narrow.width(app);
    let normal = SidebarWidthPreset::Normal.width(app);
    if app.sidebar_width <= narrow {
        SidebarWidthPreset::Narrow
    } else if app.sidebar_width <= normal {
        SidebarWidthPreset::Normal
    } else {
        SidebarWidthPreset::Wide
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    if !collapsed {
        return;
    }
    let toggle_area = collapsed_sidebar_toggle_rect(area);
    if toggle_area == Rect::default() {
        return;
    }
    let icon_style = if app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled("»", icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    #[test]
    fn collapsed_workspace_section_hides_its_agents() {
        let mut app = crate::app::state::AppState::test_new();
        let mut favorite = Workspace::test_new("herdr");
        favorite.section = crate::workspace::WorkspaceSection::Favorite;
        let favorite_pane = favorite.tabs[0].root_pane;
        let mut work = Workspace::test_new("work");
        work.section = crate::workspace::WorkspaceSection::Work;
        let work_pane = work.tabs[0].root_pane;

        app.workspaces = vec![favorite, work];
        app.ensure_test_terminals();
        for (ws_idx, pane_id) in [(0, favorite_pane), (1, work_pane)] {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;
        app.collapsed_workspace_sections
            .insert(crate::workspace::WorkspaceSection::Favorite);

        let entries = agent_panel_entries(&app);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].primary_label, "work");
    }

    #[test]
    fn collapsed_workspace_section_renders_header_without_cards() {
        let mut app = crate::app::state::AppState::test_new();
        let mut fav = Workspace::test_new("fav");
        fav.section = crate::workspace::WorkspaceSection::Favorite;
        let mut docs = Workspace::test_new("docs");
        docs.section = crate::workspace::WorkspaceSection::Favorite;
        app.workspaces = vec![fav, docs];
        app.ensure_test_terminals();
        app.collapsed_workspace_sections
            .insert(crate::workspace::WorkspaceSection::Favorite);

        let area = Rect::new(0, 0, 32, 10);
        let cards = compute_workspace_card_areas(&app, area);
        let sections = compute_workspace_section_header_areas(&app, area);

        assert!(cards.is_empty());
        assert_eq!(sections.len(), 1);
        assert_eq!(
            sections[0].section,
            crate::workspace::WorkspaceSection::Favorite
        );
    }

    #[test]
    fn workspace_section_header_leaves_blank_row_and_cards_keep_full_width_hit_area() {
        let mut app = crate::app::state::AppState::test_new();
        let mut work = Workspace::test_new("work");
        work.section = crate::workspace::WorkspaceSection::Work;
        app.workspaces = vec![work];
        app.ensure_test_terminals();

        let area = Rect::new(0, 0, 32, 24);
        let cards = compute_workspace_card_areas(&app, area);
        let sections = compute_workspace_section_header_areas(&app, area);

        let work_section = sections
            .iter()
            .find(|section| section.section == crate::workspace::WorkspaceSection::Work)
            .expect("work section header");
        assert!(!sections
            .iter()
            .any(|section| section.section == crate::workspace::WorkspaceSection::Personal));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].rect.x, work_section.rect.x);
        assert_eq!(cards[0].rect.width, work_section.rect.width);
        assert_eq!(cards[0].rect.y, work_section.rect.y + 2);
    }

    #[test]
    fn favorite_section_renders_above_work_section() {
        let mut app = crate::app::state::AppState::test_new();
        let mut work = Workspace::test_new("work");
        work.section = crate::workspace::WorkspaceSection::Work;
        let mut favorite = Workspace::test_new("favorite");
        favorite.section = crate::workspace::WorkspaceSection::Favorite;
        app.workspaces = vec![work, favorite];
        app.ensure_test_terminals();

        let sections = compute_workspace_section_header_areas(&app, Rect::new(0, 0, 32, 24));
        let favorite_y = sections
            .iter()
            .find(|section| section.section == crate::workspace::WorkspaceSection::Favorite)
            .expect("favorite header")
            .rect
            .y;
        let work_y = sections
            .iter()
            .find(|section| section.section == crate::workspace::WorkspaceSection::Work)
            .expect("work header")
            .rect
            .y;

        assert!(favorite_y < work_y);
    }

    #[test]
    fn sidebar_renders_selection_copy_status_at_bottom() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.selection_copy_status = Some(crate::app::state::SelectionCopyStatus { line_count: 3 });
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 12));

        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, frame, app.view.sidebar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let y = app.view.sidebar_rect.y + app.view.sidebar_rect.height - 1;
        let row = (app.view.sidebar_rect.x..app.view.sidebar_rect.x + app.view.sidebar_rect.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>();

        assert!(row.contains("Copied 3 lines"));
    }

    #[test]
    fn sorted_agent_panel_entries_group_attention_then_working_then_seen_idle() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("seen"),
            Workspace::test_new("working"),
            Workspace::test_new("blocked"),
            Workspace::test_new("done"),
        ];
        app.ensure_test_terminals();

        let cases = [
            (0, AgentState::Idle, true),
            (1, AgentState::Working, true),
            (2, AgentState::Blocked, true),
            (3, AgentState::Idle, false),
        ];
        for (ws_idx, state, seen) in cases {
            let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.state = state;
            terminal.detected_agent = Some(Agent::Claude);
            app.workspaces[ws_idx].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap()
                .seen = seen;
        }
        app.agent_panel_scope = AgentPanelScope::SortedAllWorkspaces;

        let labels = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["blocked", "done", "working", "seen"]);
    }

    #[test]
    fn workspace_panel_uses_two_rows_in_full_density() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        let workspace_id = app.workspaces[0].id.clone();
        let resolved_identity_cwd = app.workspaces[0].resolved_identity_cwd().unwrap();
        app.apply_workspace_git_statuses(vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            branch: Some("main".into()),
            ahead_behind: None,
            diff_stats: None,
        }]);

        let area = Rect::new(0, 0, 24, 24);
        app.workspace_panel_density = WorkspacePanelDensity::Full;
        let full_cards = compute_workspace_card_areas(&app, area);
        app.workspace_panel_density = WorkspacePanelDensity::Slim;
        let slim_cards = compute_workspace_card_areas(&app, area);

        assert_eq!(full_cards[0].rect.height, 2);
        assert_eq!(full_cards[1].rect.height, 2);
        assert_eq!(slim_cards[0].rect.height, 1);
        assert_eq!(slim_cards[1].rect.y, slim_cards[0].rect.y + 2);
    }

    #[test]
    fn workspace_density_toggle_aligns_with_workspace_panel_title() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.workspace_panel_density = WorkspacePanelDensity::Slim;
        app.mouse_capture = false;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 12));

        let ws_area = workspace_list_rect(app.view.sidebar_rect, app.sidebar_section_split);
        let toggle = workspace_panel_density_toggle_rect(ws_area, app.workspace_panel_density);
        let spaces_header = app
            .view
            .workspace_section_header_areas
            .iter()
            .find(|section| section.section == crate::workspace::WorkspaceSection::None)
            .expect("spaces section header")
            .rect;
        assert_eq!(toggle.y, ws_area.y);
        assert_ne!(toggle.y, spaces_header.y);

        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, frame, app.view.sidebar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let title_row = (ws_area.x..ws_area.x + ws_area.width)
            .map(|x| buffer[(x, ws_area.y)].symbol())
            .collect::<String>();
        let section_row = (ws_area.x..ws_area.x + ws_area.width)
            .map(|x| buffer[(x, spaces_header.y)].symbol())
            .collect::<String>();

        assert!(title_row.contains("spaces"), "row: {title_row:?}");
        assert!(title_row.contains("[slim]"), "row: {title_row:?}");
        assert!(!section_row.contains("[slim]"), "row: {section_row:?}");
    }

    #[test]
    fn full_workspace_panel_renders_nogit_label_for_non_git_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.workspaces[0].cached_git_branch = None;
        app.ensure_test_terminals();
        app.workspace_panel_density = WorkspacePanelDensity::Full;
        app.mouse_capture = false;

        let area = Rect::new(0, 0, 32, 6);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 32, 2),
        }];
        let backend = ratatui::backend::TestBackend::new(32, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, frame, area, false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..32).map(|x| buffer[(x, 3)].symbol()).collect::<String>();

        assert!(row.contains("nogit"));
    }

    #[test]
    fn workspace_reorder_indicator_renders_at_target_section_bottom() {
        let mut app = crate::app::state::AppState::test_new();
        let source = Workspace::test_new("source");
        let mut work_a = Workspace::test_new("work-a");
        work_a.section = crate::workspace::WorkspaceSection::Work;
        let mut work_b = Workspace::test_new("work-b");
        work_b.section = crate::workspace::WorkspaceSection::Work;
        app.workspaces = vec![source, work_a, work_b];
        app.ensure_test_terminals();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 30));

        let work_cards = app
            .view
            .workspace_card_areas
            .iter()
            .copied()
            .filter(|card| {
                workspace_effective_section(&app, card.ws_idx)
                    == crate::workspace::WorkspaceSection::Work
            })
            .collect::<Vec<_>>();
        let last = work_cards.last().expect("work card");
        let insert_idx = last.ws_idx + 1;
        let indicator_row = last.rect.y + last.rect.height;
        app.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                insert_idx: Some(insert_idx),
                target_section: Some(crate::workspace::WorkspaceSection::Work),
            },
        });

        let area = workspace_list_rect(app.view.sidebar_rect, app.sidebar_section_split);
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, frame, area, false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (area.x..area.x + area.width)
            .map(|x| buffer[(x, indicator_row)].symbol())
            .collect::<String>();

        assert!(row.contains("─"));
    }

    #[test]
    fn slim_workspace_panel_keeps_git_details_on_name_row() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.workspace_panel_density = WorkspacePanelDensity::Slim;
        app.mouse_capture = false;
        let workspace_id = app.workspaces[0].id.clone();
        let resolved_identity_cwd = app.workspaces[0].resolved_identity_cwd().unwrap();
        app.apply_workspace_git_statuses(vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            branch: Some("main".into()),
            ahead_behind: Some((2, 1)),
            diff_stats: Some((123, 11)),
        }]);

        let area = Rect::new(0, 0, 32, 6);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 32, 1),
        }];
        let backend = ratatui::backend::TestBackend::new(32, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, frame, area, false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..32).map(|x| buffer[(x, 2)].symbol()).collect::<String>();

        assert!(row.contains("one"));
        assert_eq!(buffer[(1, 2)].symbol(), "·");
        assert_eq!(buffer[(2, 2)].symbol(), " ");
        assert_eq!(buffer[(3, 2)].symbol(), "1");
        assert!(row.contains("1 "));
        assert!(row.contains("main"));
        assert!(row.contains("↑2"));
        assert!(row.contains("↓1"));
        assert!(row.contains("+123"));
        assert!(row.contains("-11"));
        assert!(row.find("↑2").unwrap() < row.find("main").unwrap());
        assert!(row.find("-11").unwrap() < row.find("main").unwrap());
    }

    #[test]
    fn slim_workspace_panel_prefers_name_over_diff_stats_when_narrow() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("very-long-space-name")];
        app.ensure_test_terminals();
        app.workspace_panel_density = WorkspacePanelDensity::Slim;
        app.mouse_capture = false;
        let workspace_id = app.workspaces[0].id.clone();
        let resolved_identity_cwd = app.workspaces[0].resolved_identity_cwd().unwrap();
        app.apply_workspace_git_statuses(vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            branch: Some("main".into()),
            ahead_behind: Some((2, 1)),
            diff_stats: Some((203, 31)),
        }]);

        let area = Rect::new(0, 0, 28, 6);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 28, 1),
        }];
        let backend = ratatui::backend::TestBackend::new(28, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, frame, area, false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..28).map(|x| buffer[(x, 2)].symbol()).collect::<String>();

        assert!(row.contains("very-long-space-name"), "row: {row:?}");
        assert!(!row.contains("+203"), "row: {row:?}");
        assert!(!row.contains("-31"), "row: {row:?}");
    }

    #[test]
    fn full_workspace_panel_diff_labels_start_after_indent() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.workspace_panel_density = WorkspacePanelDensity::Full;
        app.mouse_capture = false;
        let workspace_id = app.workspaces[0].id.clone();
        let resolved_identity_cwd = app.workspaces[0].resolved_identity_cwd().unwrap();
        app.apply_workspace_git_statuses(vec![crate::workspace::WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            branch: Some("main".into()),
            ahead_behind: None,
            diff_stats: Some((123, 11)),
        }]);

        let area = Rect::new(0, 0, 32, 6);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 32, 2),
        }];
        let backend = ratatui::backend::TestBackend::new(32, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_list(&app, frame, area, false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..32).map(|x| buffer[(x, 3)].symbol()).collect::<String>();

        let content = row.strip_prefix(' ').unwrap_or(&row);
        assert!(content.starts_with("    +123 -11 main"));
        assert!(!content.starts_with("     +123"));
    }

    #[test]
    fn agent_panel_renders_global_pane_ids_only() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("agents");
        let pane_id = workspace.tabs[0].root_pane;
        let second_pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        for pane_id in [pane_id, second_pane_id] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 48, 10);
        let backend = ratatui::backend::TestBackend::new(48, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_agent_detail(&app, frame, area))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first_row = (0..48).map(|x| buffer[(x, 3)].symbol()).collect::<String>();
        let second_row = (0..48).map(|x| buffer[(x, 6)].symbol()).collect::<String>();

        assert!(first_row.contains(&format!("%{}", pane_id.raw())));
        assert!(!first_row.contains("1-1"));
        assert!(second_row.contains(&format!("%{}", second_pane_id.raw())));
        assert!(!second_row.contains("1-2"));
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn all_workspaces_primary_label_truncates_workspace_and_tab() {
        let entry = AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            global_pane_id: "%1".into(),
            primary_label: "agent-browser".into(),
            primary_tab_label: Some("test-escalation".into()),
            agent_label: Some("claude".into()),
            state: AgentState::Idle,
            seen: true,
            custom_status: None,
        };

        let label = format_agent_panel_primary_label(&entry, 18);

        assert_eq!(label, "agent-bro… · test…");
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }
}
