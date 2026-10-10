//! Fixed upstream machine projection with the fork's mobile rows and section ownership.
use super::super::{ClientEndpointStatus, FocusTarget, ResourceKey};
use super::*;
use crate::app::NavigateAction;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub(super) struct Mobile {
    scroll: usize,
    selected: Option<ResourceKey>,
    selected_owner: Option<Owner>,
    presented: Option<Presented>,
}
pub(super) struct Presented {
    screen: Rect,
    scroll: usize,
    active_endpoint: ClientEndpointId,
    close: Rect,
    targets: Vec<(Rect, Target, Owner)>,
}
struct Owner {
    endpoint: ClientEndpointId,
    generation: Option<u64>,
    snapshot: Option<(String, u64)>,
}
impl Owner {
    fn capture(frontend: &ClientFrontend, endpoint: ClientEndpointId) -> Self {
        let state = frontend.runtime.shell.endpoint(&endpoint);
        Self {
            generation: state.and_then(|state| state.generation),
            snapshot: state
                .and_then(|state| state.cache.snapshot())
                .map(|snapshot| (snapshot.boot_id.clone(), snapshot.revision)),
            endpoint,
        }
    }
    fn matches(&self, frontend: &ClientFrontend) -> bool {
        let current = Self::capture(frontend, self.endpoint.clone());
        self.generation == current.generation && self.snapshot == current.snapshot
    }
}
#[derive(Clone)]
enum Target {
    Machine(ClientEndpointId),
    Workspace(ResourceKey),
    Agent(ResourceKey),
    Tab(ResourceKey),
    Section(ClientEndpointId, crate::workspace::WorkspaceSection),
    NewWorkspace,
    NewTab,
    Menu(crate::app::GlobalMenuAction),
}
enum Kind {
    Title(String),
    Action(&'static str),
    Item(Line<'static>, Option<String>, Color),
}
struct Row {
    kind: Kind,
    target: Option<Target>,
}
impl Row {
    fn height(&self) -> usize {
        if matches!(&self.kind, Kind::Item(_, Some(_), _)) {
            2
        } else {
            1
        }
    }
    fn title(text: impl Into<String>, target: Option<Target>) -> Self {
        Self {
            kind: Kind::Title(text.into()),
            target,
        }
    }
}
fn screen(frontend: &ClientFrontend) -> Rect {
    Rect::new(0, 0, frontend.cols, frontend.rows)
}
fn areas(frontend: &ClientFrontend) -> crate::ui::MobileSwitcherAreas {
    crate::ui::mobile_switcher_areas_for_screen(screen(frontend))
}
fn compact_tab(snapshot: &wire::ClientShellSnapshot, workspace: &str) -> String {
    let Some(workspace_state) = snapshot
        .workspaces
        .iter()
        .find(|state| state.workspace_id == workspace)
    else {
        return String::new();
    };
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|t| t.workspace_id == workspace)
        .collect::<Vec<_>>();
    let Some((index, tab)) = tabs
        .iter()
        .enumerate()
        .find(|(_, t)| t.tab_id == workspace_state.active_tab_id)
    else {
        return String::new();
    };
    if tabs.len() <= 1 {
        format!("tab {}", tab.label)
    } else {
        format!("tab {} · {}/{}", tab.label, index + 1, tabs.len())
    }
}
fn icon(status: crate::api::schema::AgentStatus, frontend: &ClientFrontend) -> (String, Style) {
    let (state, seen) = super::super::sidebar::state(status);
    let (text, style) = crate::ui::state_summary_icon(
        state,
        seen,
        frontend.chrome.spinner_tick,
        &frontend.chrome.settings.palette,
    );
    (text.to_string(), style)
}
fn item(
    frontend: &ClientFrontend,
    title: String,
    detail: Option<String>,
    status: crate::api::schema::AgentStatus,
    selected: bool,
    active: bool,
    stale: bool,
    target: Target,
    width: u16,
    indented: bool,
    connector: &str,
) -> Row {
    let p = &frontend.chrome.settings.palette;
    let bg = crate::ui::mobile_switcher_item_bg(selected, active, p);
    let (dot, style) = icon(status, frontend);
    let dim = if stale {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    Row {
        kind: Kind::Item(
            Line::from(vec![
                Span::styled(
                    format!("  {connector}"),
                    Style::default().fg(p.overlay0).bg(bg),
                ),
                Span::styled(dot, style.bg(bg).add_modifier(dim)),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    crate::ui::truncate_end(
                        &title,
                        usize::from(width.saturating_sub(if indented { 8 } else { 5 })),
                    ),
                    Style::default()
                        .fg(if stale { p.overlay0 } else { p.text })
                        .bg(bg)
                        .add_modifier(Modifier::BOLD | dim),
                ),
            ]),
            detail.map(|s| crate::ui::truncate_end(&s, usize::from(width))),
            bg,
        ),
        target: Some(target),
    }
}
fn rows(frontend: &ClientFrontend, width: u16) -> Vec<Row> {
    let shell = &frontend.runtime.shell;
    let p = &frontend.chrome.settings.palette;
    let mut rows = Vec::new();
    if shell.endpoints.len() > 1 {
        rows.push(Row::title("machines", None));
        for endpoint in &shell.endpoints {
            let (symbol, state, color) = match endpoint.status {
                ClientEndpointStatus::Connecting => ("◐", "connecting", p.yellow),
                ClientEndpointStatus::Online => ("●", "online", p.green),
                ClientEndpointStatus::Reconnecting => ("◐", "reconnecting", p.yellow),
                ClientEndpointStatus::AwaitingApproval => {
                    ("◌", "approval pending - tap to retry", p.yellow)
                }
                ClientEndpointStatus::Attention => ("!", "attention", p.red),
                ClientEndpointStatus::Disabled => ("·", "disabled", p.overlay0),
            };
            rows.push(Row {
                kind: Kind::Item(
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(symbol, Style::default().fg(color)),
                        Span::styled(
                            format!(" {}", endpoint.label),
                            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Some(format!("    {state}")),
                    p.panel_bg,
                ),
                target: Some(Target::Machine(endpoint.endpoint_id.clone())),
            });
        }
    }
    let agents = frontend.chrome.agent_targets(shell, width);
    let view_label = shell
        .endpoint(&shell.active_endpoint_id)
        .and_then(|e| e.cache.snapshot())
        .and_then(|s| s.agent_view_label.as_deref());
    if !agents.is_empty() || view_label.is_some() {
        rows.push(Row::title(
            view_label.map_or_else(|| "agents".into(), |s| format!("agents · {s}")),
            None,
        ));
        if agents.is_empty() {
            rows.push(Row {
                kind: Kind::Item(
                    Line::from(Span::styled(
                        "  no matching agents",
                        Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                    )),
                    None,
                    Color::Reset,
                ),
                target: None,
            });
        }
        for key in agents {
            let Some(endpoint) = shell.endpoint(&key.endpoint) else {
                continue;
            };
            let Some(snapshot) = endpoint.cache.snapshot() else {
                continue;
            };
            let Some(agent) = snapshot.agents.iter().find(|a| a.pane_id == key.id) else {
                continue;
            };
            let Some(workspace) = snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == agent.workspace_id)
            else {
                continue;
            };
            let mut details = Vec::new();
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|t| t.workspace_id == agent.workspace_id)
                .collect::<Vec<_>>();
            if let Some(tab) = tabs
                .iter()
                .find(|t| t.tab_id == agent.tab_id)
                .filter(|t| t.custom_label || tabs.len() > 1)
            {
                details.push(tab.label.clone());
            }
            let (state, seen) = super::super::sidebar::state(agent.agent_status);
            let status = crate::ui::agent_panel_status_key(state, seen);
            details.push(
                agent
                    .state_labels
                    .iter()
                    .find(|(k, _)| k == status)
                    .map_or_else(
                        || crate::ui::state_label(state, seen).to_string(),
                        |(_, s)| s.clone(),
                    ),
            );
            if let Some(label) = agent
                .display_agent
                .as_ref()
                .or(agent.name.as_ref())
                .or(agent.agent.as_ref())
            {
                details.push(label.clone());
            }
            rows.push(item(
                frontend,
                format!("{} · {}", endpoint.label, workspace.label),
                Some(format!("  {}", details.join(" · "))),
                agent.agent_status,
                false,
                key.endpoint == shell.active_endpoint_id && agent.focused,
                endpoint.status != ClientEndpointStatus::Online,
                Target::Agent(key),
                width,
                false,
                "",
            ));
        }
    }
    rows.push(Row::title("spaces", None));
    rows.push(Row {
        kind: Kind::Action("+ new workspace"),
        target: Some(Target::NewWorkspace),
    });
    for endpoint in &shell.endpoints {
        let Some(snapshot) = endpoint.cache.snapshot() else {
            continue;
        };
        let memberships = snapshot
            .workspaces
            .iter()
            .map(|w| {
                w.worktree
                    .as_ref()
                    .map(|g| (g.key.as_str(), g.is_linked_worktree))
            })
            .collect::<Vec<_>>();
        let entries =
            crate::ui::sidebar::workspace_group_entries(&memberships, None, true, |_| false)
                .into_iter()
                .map(|entry| match entry {
                    crate::ui::WorkspaceListEntry::Workspace { ws_idx, indented } => {
                        (ws_idx, indented)
                    }
                })
                .collect::<Vec<_>>();
        let provided = snapshot.workspaces.iter().all(|w| {
            endpoint
                .cache
                .displayed_workspace_facts(&w.workspace_id)
                .and_then(|f| f.section)
                .is_some()
        });
        let sections = if provided {
            crate::workspace::WorkspaceSection::ALL
                .into_iter()
                .map(Some)
                .collect::<Vec<_>>()
        } else {
            vec![None]
        };
        for section in sections {
            let entries = entries
                .iter()
                .filter(|e| {
                    section.is_none()
                        || endpoint
                            .cache
                            .displayed_workspace_facts(&snapshot.workspaces[e.0].workspace_id)
                            .and_then(|f| f.section)
                            == section
                })
                .collect::<Vec<_>>();
            if entries.is_empty() {
                continue;
            }
            if let Some(section) = section {
                let collapsed = frontend
                    .chrome
                    .collapsed_sections
                    .contains(&(endpoint.endpoint_id.clone(), section));
                rows.push(Row::title(
                    format!("{} {}", if collapsed { "▸" } else { "▾" }, section.label()),
                    Some(Target::Section(endpoint.endpoint_id.clone(), section)),
                ));
                if collapsed {
                    continue;
                }
            }
            for (index, entry) in entries.iter().enumerate() {
                let w = &snapshot.workspaces[entry.0];
                let key = ResourceKey {
                    endpoint: endpoint.endpoint_id.clone(),
                    id: w.workspace_id.clone(),
                };
                let selected =
                    frontend.mobile.as_ref().and_then(|m| m.selected.as_ref()) == Some(&key);
                let name = if entry.1 {
                    crate::ui::sidebar::grouped_child_display_label(
                        &w.label,
                        w.branch.as_deref(),
                        w.custom_label,
                    )
                } else {
                    w.label.clone()
                };
                let last = !entries.get(index + 1).is_some_and(|e| e.1);
                let connector = if !entry.1 {
                    ""
                } else if last {
                    "└─ "
                } else {
                    "├─ "
                };
                let prefix = if !entry.1 {
                    "  "
                } else if last {
                    "       "
                } else {
                    "  │    "
                };
                rows.push(item(
                    frontend,
                    format!("{} · {name}", endpoint.label),
                    Some(format!(
                        "{prefix}{} · {}",
                        w.branch.as_deref().unwrap_or("shell"),
                        compact_tab(snapshot, &w.workspace_id)
                    )),
                    w.agent_status,
                    selected,
                    endpoint.endpoint_id == shell.active_endpoint_id && w.focused,
                    endpoint.status != ClientEndpointStatus::Online,
                    Target::Workspace(key),
                    width,
                    entry.1,
                    connector,
                ));
            }
        }
    }
    if let Some(snapshot) = shell
        .endpoint(&shell.active_endpoint_id)
        .and_then(|e| e.cache.snapshot())
    {
        if let Some(workspace) = snapshot.focused_workspace_id.as_ref() {
            rows.push(Row::title("tabs", None));
            rows.push(Row {
                kind: Kind::Action("+ new tab"),
                target: Some(Target::NewTab),
            });
            for (index, tab) in snapshot
                .tabs
                .iter()
                .filter(|t| &t.workspace_id == workspace)
                .enumerate()
            {
                let bg = crate::ui::mobile_switcher_item_bg(false, tab.focused, p);
                let text = if tab.custom_label {
                    format!("{} · {}", index + 1, tab.label)
                } else {
                    format!("tab {}", tab.label)
                };
                rows.push(Row {
                    kind: Kind::Item(
                        Line::from(Span::styled(
                            format!(
                                "  {}",
                                crate::ui::truncate_end(
                                    &text,
                                    usize::from(width.saturating_sub(3))
                                )
                            ),
                            Style::default()
                                .fg(p.text)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        )),
                        None,
                        bg,
                    ),
                    target: Some(Target::Tab(ResourceKey {
                        endpoint: shell.active_endpoint_id.clone(),
                        id: tab.tab_id.clone(),
                    })),
                });
            }
        }
    }
    rows.push(Row::title("menu", None));
    for action in crate::app::global_menu_actions_for(notes::available(frontend)) {
        rows.push(Row {
            kind: Kind::Item(
                Line::from(Span::styled(
                    format!("  {}", crate::app::global_menu_action_label(action)),
                    Style::default().fg(p.overlay1).bg(p.panel_bg),
                )),
                None,
                p.panel_bg,
            ),
            target: Some(Target::Menu(action)),
        });
    }
    rows
}
pub(super) fn open(frontend: &mut ClientFrontend) {
    selection::clear(frontend);
    frontend.prefix = false;
    let selected = frontend
        .runtime
        .shell
        .endpoint(&frontend.runtime.shell.active_endpoint_id)
        .and_then(|e| e.cache.snapshot())
        .and_then(|s| s.focused_workspace_id.as_ref())
        .map(|id| ResourceKey {
            endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
            id: id.clone(),
        });
    let selected_owner = selected
        .as_ref()
        .map(|key| Owner::capture(frontend, key.endpoint.clone()));
    frontend.mobile = Some(Mobile {
        scroll: 0,
        selected,
        selected_owner,
        presented: None,
    });
    frontend.force_redraw = true;
}
pub(super) fn action(frontend: &mut ClientFrontend, action: NavigateAction) -> bool {
    if action != NavigateAction::WorkspacePicker || frontend.cols == 0 {
        return false;
    }
    open(frontend);
    true
}
fn narrow(frontend: &ClientFrontend) -> bool {
    frontend.cols > 0 && frontend.cols <= frontend.chrome.settings.mobile_width_threshold
}

pub(super) fn sync_selection(frontend: &mut ClientFrontend) {
    frontend.chrome.navigate_selection = frontend
        .mobile
        .as_ref()
        .and_then(|state| state.selected.clone());
}

fn capture_selected_owner(frontend: &mut ClientFrontend) {
    let owner = frontend
        .mobile
        .as_ref()
        .and_then(|state| state.selected.as_ref())
        .map(|key| Owner::capture(frontend, key.endpoint.clone()));
    if let Some(state) = &mut frontend.mobile {
        state.selected_owner = owner;
    }
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, terminal_area: Rect) {
    let Some(mobile) = &frontend.mobile else {
        return;
    };
    if !narrow(frontend) {
        crate::ui::render_navigate_overlay_from(
            frame,
            terminal_area,
            &frontend.chrome.settings.palette,
            &frontend.keybinds.keybinds,
            false,
        );
        return;
    }
    let areas = areas(frontend);
    let content = crate::ui::mobile_switcher_content_rect(areas.viewport);
    let rows = rows(frontend, content.width);
    let total = rows.iter().map(Row::height).sum::<usize>();
    let scroll = mobile
        .scroll
        .min(total.saturating_sub(usize::from(areas.viewport.height)));
    let p = &frontend.chrome.settings.palette;
    crate::ui::render_mobile_panel_frame(p, frame, screen(frontend), areas);
    crate::ui::render_mobile_scrollbar(
        frame,
        areas.viewport,
        total,
        usize::from(areas.viewport.height),
        scroll,
        p,
    );
    let mut y = 0;
    for row in rows {
        let height = row.height();
        match row.kind {
            Kind::Title(text) => crate::ui::render_mobile_section_title(
                frame,
                areas.viewport,
                content,
                y,
                scroll,
                &text,
                p,
            ),
            Kind::Action(text) => crate::ui::render_mobile_action_row(
                frame,
                areas.viewport,
                content,
                y,
                scroll,
                text,
                p,
            ),
            Kind::Item(title, Some(detail), bg) => crate::ui::render_mobile_two_line_item(
                frame,
                areas.viewport,
                content,
                y,
                scroll,
                bg,
                title,
                detail,
                p.overlay0,
            ),
            Kind::Item(title, None, bg) => crate::ui::render_mobile_one_line_item(
                frame,
                areas.viewport,
                content,
                y,
                scroll,
                bg,
                title,
            ),
        }
        y += height;
    }
}
pub(super) fn presentation(frontend: &ClientFrontend) -> Option<Presented> {
    if !narrow(frontend) {
        return None;
    }
    let mobile = frontend.mobile.as_ref()?;
    let areas = areas(frontend);
    let content = crate::ui::mobile_switcher_content_rect(areas.viewport);
    let rows = rows(frontend, content.width);
    let total = rows.iter().map(Row::height).sum::<usize>();
    let scroll = mobile
        .scroll
        .min(total.saturating_sub(usize::from(areas.viewport.height)));
    let mut targets = Vec::new();
    let mut y = 0;
    for row in rows {
        let bottom = y + row.height();
        if let Some(target) = row.target {
            let top = y.max(scroll);
            let end = bottom.min(scroll + usize::from(content.height));
            if end > top && !content.is_empty() {
                let endpoint = match &target {
                    Target::Machine(id) | Target::Section(id, _) => id.clone(),
                    Target::Workspace(key) | Target::Agent(key) | Target::Tab(key) => {
                        key.endpoint.clone()
                    }
                    _ => frontend.runtime.shell.active_endpoint_id.clone(),
                };
                targets.push((
                    Rect::new(
                        content.x,
                        content.y + (top - scroll) as u16,
                        content.width,
                        (end - top) as u16,
                    ),
                    target,
                    Owner::capture(frontend, endpoint),
                ));
            }
        }
        y = bottom;
    }
    Some(Presented {
        screen: screen(frontend),
        scroll: mobile.scroll,
        active_endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
        close: areas.close,
        targets,
    })
}
pub(super) fn commit_presentation(frontend: &mut ClientFrontend, presented: Option<Presented>) {
    if let Some(mobile) = &mut frontend.mobile {
        mobile.presented = presented;
    }
}
fn activate(frontend: &mut ClientFrontend, target: Target) -> io::Result<()> {
    match target {
        Target::Section(endpoint, section) => {
            if !frontend
                .chrome
                .collapsed_sections
                .remove(&(endpoint.clone(), section))
            {
                frontend
                    .chrome
                    .collapsed_sections
                    .insert((endpoint, section));
            }
            frontend.persist_chrome_preferences();
        }
        Target::Menu(action) => {
            frontend.mobile = None;
            menu::apply_action(frontend, action)?;
        }
        Target::NewWorkspace | Target::NewTab => {
            frontend.mobile = None;
            let action = if matches!(target, Target::NewTab) {
                NavigateAction::NewTab
            } else {
                NavigateAction::NewWorkspace
            };
            modal::action(frontend, action)?;
        }
        target => {
            let (endpoint, target) = match target {
                Target::Machine(id) => (id, None),
                Target::Workspace(key) => (key.endpoint, Some(FocusTarget::Workspace(key.id))),
                Target::Agent(key) => (key.endpoint, Some(FocusTarget::Pane(key.id))),
                Target::Tab(key) => (key.endpoint, Some(FocusTarget::Tab(key.id))),
                _ => unreachable!("navigation target"),
            };
            frontend.mobile = None;
            let update = frontend.runtime.activate(endpoint, target, Instant::now());
            frontend.update(update)?;
        }
    }
    frontend.force_redraw = true;
    Ok(())
}
pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if frontend.mobile.is_none() {
        return Ok(false);
    }
    sync_selection(frontend);
    if !narrow(frontend) {
        if let RawInputEvent::Mouse(mouse) = event {
            let view =
                frontend
                    .chrome
                    .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
            // Navigate owns terminal input; sidebar and tab clicks retain their existing handlers.
            return Ok(!view
                .layout
                .sidebar
                .contains((mouse.column, mouse.row).into())
                && !view
                    .layout
                    .tab_bar
                    .contains((mouse.column, mouse.row).into()));
        }
    }
    let areas = areas(frontend);
    let content = crate::ui::mobile_switcher_content_rect(areas.viewport);
    let rows = rows(frontend, content.width);
    let max = rows
        .iter()
        .map(Row::height)
        .sum::<usize>()
        .saturating_sub(usize::from(areas.viewport.height));
    match event {
        RawInputEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                if let Some(mobile) = &mut frontend.mobile {
                    mobile.scroll = mobile.scroll.saturating_sub(2);
                }
            }
            MouseEventKind::ScrollDown => {
                let Some(mobile) = frontend.mobile.as_mut() else {
                    return Ok(true);
                };
                mobile.scroll = mobile.scroll.saturating_add(2).min(max);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(presented) = frontend
                    .mobile
                    .as_ref()
                    .and_then(|mobile| mobile.presented.as_ref())
                else {
                    return Ok(true);
                };
                if presented.screen != screen(frontend)
                    || presented.active_endpoint != frontend.runtime.shell.active_endpoint_id
                    || frontend
                        .mobile
                        .as_ref()
                        .is_some_and(|mobile| mobile.scroll != presented.scroll)
                {
                    return Ok(true);
                }
                if presented.close.contains((mouse.column, mouse.row).into()) {
                    frontend.mobile = None;
                } else {
                    let target = presented
                        .targets
                        .iter()
                        .find(|(rect, _, _)| rect.contains((mouse.column, mouse.row).into()))
                        .filter(|(_, _, owner)| owner.matches(frontend))
                        .map(|(_, target, _)| target.clone());
                    if let Some(target) = target {
                        activate(frontend, target)?;
                    }
                }
            }
            _ => {}
        },
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
            if key.code == KeyCode::Esc
                || crate::config::terminal_key_matches_combo(*key, frontend.keybinds.prefix)
            {
                frontend.mobile = None;
            } else if frontend
                .keybinds
                .keybinds
                .navigate
                .workspace_up
                .matches_direct_key(*key)
                || frontend
                    .keybinds
                    .keybinds
                    .navigate
                    .workspace_down
                    .matches_direct_key(*key)
            {
                let targets = frontend
                    .chrome
                    .visual_workspace_targets(&frontend.runtime.shell, frontend.cols);
                let Some(mobile) = frontend.mobile.as_mut() else {
                    return Ok(true);
                };
                let index = targets
                    .iter()
                    .position(|key| Some(key) == mobile.selected.as_ref())
                    .unwrap_or(0);
                let down = frontend
                    .keybinds
                    .keybinds
                    .navigate
                    .workspace_down
                    .matches_direct_key(*key);
                let index = if down {
                    index.saturating_add(1).min(targets.len().saturating_sub(1))
                } else {
                    index.saturating_sub(1)
                };
                mobile.selected = targets.get(index).cloned();
                if let Some(selected) = &mobile.selected {
                    let mut y = 0;
                    for row in &rows {
                        if matches!(&row.target,Some(Target::Workspace(key)) if key==selected) {
                            if y < mobile.scroll {
                                mobile.scroll = y;
                            } else if y + row.height() > mobile.scroll + usize::from(content.height)
                            {
                                mobile.scroll =
                                    (y + row.height()).saturating_sub(usize::from(content.height));
                            }
                            break;
                        }
                        y += row.height();
                    }
                }
                mobile.scroll = mobile.scroll.min(max);
                capture_selected_owner(frontend);
            } else {
                let reserved = match key.code {
                    KeyCode::Enter if key.modifiers.is_empty() => frontend
                        .mobile
                        .as_ref()
                        .filter(|state| {
                            state
                                .selected_owner
                                .as_ref()
                                .is_some_and(|owner| owner.matches(frontend))
                        })
                        .and_then(|m| m.selected.clone())
                        .map(Target::Workspace),
                    KeyCode::Char(c @ '1'..='9') if key.modifiers.is_empty() => frontend
                        .chrome
                        .visual_workspace_targets(&frontend.runtime.shell, frontend.cols)
                        .get((c as usize) - ('1' as usize))
                        .cloned()
                        .map(Target::Workspace),
                    _ => None,
                };
                if let Some(target) = reserved {
                    activate(frontend, target)?;
                } else {
                    let navigate = &frontend.keybinds.keybinds.navigate;
                    let reserved = if key.modifiers.is_empty() {
                        match key.code {
                            KeyCode::Tab => Some(NavigateAction::CyclePaneNext),
                            KeyCode::BackTab => Some(NavigateAction::CyclePanePrevious),
                            KeyCode::Left => Some(NavigateAction::FocusPaneLeft),
                            KeyCode::Right => Some(NavigateAction::FocusPaneRight),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let action = reserved
                        .or_else(|| {
                            if navigate.pane_left.matches_direct_key(*key) {
                                Some(NavigateAction::FocusPaneLeft)
                            } else if navigate.pane_down.matches_direct_key(*key) {
                                Some(NavigateAction::FocusPaneDown)
                            } else if navigate.pane_up.matches_direct_key(*key) {
                                Some(NavigateAction::FocusPaneUp)
                            } else if navigate.pane_right.matches_direct_key(*key) {
                                Some(NavigateAction::FocusPaneRight)
                            } else {
                                None
                            }
                        })
                        .or_else(|| {
                            crate::app::navigation_action_for_bindings(
                                &frontend.keybinds.keybinds,
                                *key,
                                crate::app::BindingDispatch::Prefix,
                            )
                        });
                    if action.is_none_or(custom::indexed) && custom::key(frontend, *key, true)? {
                        frontend.mobile = None;
                    } else if let Some(action) = action {
                        let keep = matches!(
                            action,
                            NavigateAction::CyclePaneNext
                                | NavigateAction::CyclePanePrevious
                                | NavigateAction::FocusPaneLeft
                                | NavigateAction::FocusPaneDown
                                | NavigateAction::FocusPaneUp
                                | NavigateAction::FocusPaneRight
                        );
                        if !keep {
                            frontend.mobile = None;
                        }
                        input::run_navigation_action(frontend, *key, true, Some(action))?;
                    }
                }
            }
        }
        RawInputEvent::OuterFocusLost | RawInputEvent::OuterFocusGained => return Ok(false),
        _ => {}
    }
    frontend.force_redraw = true;
    Ok(true)
}

pub(super) fn graphics_rect(frontend: &ClientFrontend, terminal_area: Rect) -> Option<Rect> {
    frontend.mobile.as_ref()?;
    if narrow(frontend) {
        Some(screen(frontend))
    } else {
        Some(Rect::new(
            terminal_area.x,
            terminal_area.y + terminal_area.height.saturating_sub(1),
            terminal_area.width,
            terminal_area.height.min(1),
        ))
    }
}
