//! Client-only Navigator presentation; resources and metadata remain endpoint-owned.
use super::super::{FocusTarget, ResourceKey};
use super::*;
use crate::app::state::{NavigatorRow, NavigatorState, NavigatorStateFilter};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashSet;

#[derive(Clone, PartialEq, Eq)]
enum Kind {
    Workspace,
    Tab,
    Pane,
}
#[derive(Clone, PartialEq, Eq)]
struct Target {
    key: ResourceKey,
    kind: Kind,
    generation: Option<u64>,
    boot: String,
    detail: String,
}
pub(super) struct Navigator {
    state: NavigatorState,
    expanded: HashSet<ResourceKey>,
    selected: Option<Target>,
    presented: Option<Presented>,
}
pub(super) struct Presented {
    screen: Rect,
    body: Rect,
    search: Rect,
    popup: Rect,
    scroll: usize,
    rows: Vec<NavigatorRow<Target>>,
    revisions: Vec<(ClientEndpointId, Option<u64>, String, u64)>,
}
fn area(frontend: &ClientFrontend) -> Rect {
    Rect::new(0, 0, frontend.cols, frontend.rows)
}
fn body(frontend: &ClientFrontend) -> Rect {
    crate::ui::navigator_body_rect(crate::ui::navigator_inner_rect(
        crate::ui::navigator_popup_rect(area(frontend)),
    ))
}
fn label(state: crate::detect::AgentState, seen: bool) -> &'static str {
    crate::app::state_label_text(state, seen)
}
fn matches(state: &NavigatorState, row: &NavigatorRow<Target>) -> bool {
    match state.state_filter {
        Some(NavigatorStateFilter::Blocked) => row.status == crate::detect::AgentState::Blocked,
        Some(NavigatorStateFilter::Working) => row.status == crate::detect::AgentState::Working,
        Some(NavigatorStateFilter::Idle) => {
            row.status == crate::detect::AgentState::Idle && row.seen
        }
        Some(NavigatorStateFilter::Done) => {
            row.status == crate::detect::AgentState::Idle && !row.seen
        }
        None => crate::app::state::text_matches_query(
            &state.query.trim().to_lowercase(),
            &row.search_text,
        ),
    }
}
fn activity(
    endpoint: &super::super::shell::ClientShellEndpoint,
    snapshot: &wire::ClientShellSnapshot,
    workspace: &str,
    tab: Option<&str>,
) -> String {
    let panes = snapshot
        .panes
        .iter()
        .filter(|pane| pane.workspace_id == workspace && tab.is_none_or(|tab| pane.tab_id == tab));
    let (mut blocked, mut working, mut done) = (0, 0, 0);
    for pane in panes {
        let state = endpoint
            .cache
            .displayed_pane_facts(&pane.pane_id)
            .map(|facts| {
                if facts.state == crate::api::schema::AgentStatus::Idle && !facts.seen {
                    crate::api::schema::AgentStatus::Done
                } else {
                    facts.state
                }
            })
            .or_else(|| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane.pane_id)
                    .map(|agent| agent.agent_status)
            });
        match state {
            Some(crate::api::schema::AgentStatus::Blocked) => blocked += 1,
            Some(crate::api::schema::AgentStatus::Working) => working += 1,
            Some(crate::api::schema::AgentStatus::Done) => done += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if blocked > 0 {
        parts.push(format!("{blocked} blocked"));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if done > 0 {
        parts.push(format!("{done} done"));
    }
    parts.join(" · ")
}
fn rows(frontend: &ClientFrontend) -> Vec<NavigatorRow<Target>> {
    let Some(nav) = &frontend.navigator else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for endpoint in &frontend.runtime.shell.endpoints {
        let Some(snapshot) = endpoint.cache.snapshot() else {
            continue;
        };
        let target = |id: &str, kind, detail| Target {
            key: ResourceKey {
                endpoint: endpoint.endpoint_id.clone(),
                id: id.to_owned(),
            },
            kind,
            generation: endpoint.generation,
            boot: snapshot.boot_id.clone(),
            detail,
        };
        let current = endpoint.endpoint_id == frontend.runtime.shell.active_endpoint_id;
        for workspace in &snapshot.workspaces {
            let panes = snapshot
                .panes
                .iter()
                .filter(|pane| pane.workspace_id == workspace.workspace_id)
                .count();
            let meta = activity(endpoint, snapshot, &workspace.workspace_id, None);
            let (status, seen) = super::super::sidebar::state(workspace.agent_status);
            let detail = if meta.is_empty() {
                format!("{} · {panes} panes", workspace.label)
            } else {
                format!("{} · {panes} panes · {meta}", workspace.label)
            };
            let key = ResourceKey {
                endpoint: endpoint.endpoint_id.clone(),
                id: workspace.workspace_id.clone(),
            };
            let expanded = !nav.state.query.trim().is_empty()
                || nav.state.state_filter.is_some()
                || nav.expanded.contains(&key);
            let workspace_row = NavigatorRow {
                target: target(&workspace.workspace_id, Kind::Workspace, detail),
                depth: 0,
                label: format!("{} ({panes})", workspace.label),
                search_text: format!("{} {meta}", workspace.label).to_lowercase(),
                meta,
                status,
                seen,
                is_current: current && workspace.focused,
                is_workspace: true,
                is_tab: false,
                expanded,
            };
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace.workspace_id)
                .collect::<Vec<_>>();
            let multi = tabs.len() > 1;
            let mut children = Vec::new();
            for tab in tabs {
                let tab_panes = snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.tab_id == tab.tab_id)
                    .collect::<Vec<_>>();
                let activity = activity(
                    endpoint,
                    snapshot,
                    &workspace.workspace_id,
                    Some(&tab.tab_id),
                );
                let meta = if activity.is_empty() {
                    format!("{} panes", tab_panes.len())
                } else {
                    format!("{} panes · {activity}", tab_panes.len())
                };
                let (status, seen) = super::super::sidebar::state(tab.agent_status);
                let tab_row = NavigatorRow {
                    target: target(
                        &tab.tab_id,
                        Kind::Tab,
                        format!(
                            "{} · tab: {} · {} panes · {meta}",
                            workspace.label,
                            tab.label,
                            tab_panes.len()
                        ),
                    ),
                    depth: 1,
                    label: tab.label.clone(),
                    search_text: format!("{} {meta}", tab.label).to_lowercase(),
                    meta,
                    status,
                    seen,
                    is_current: false,
                    is_workspace: false,
                    is_tab: true,
                    expanded: true,
                };
                let tab_matches = multi && matches(&nav.state, &tab_row);
                let mut pane_rows = Vec::new();
                for pane in tab_panes {
                    let facts = endpoint.cache.displayed_pane_facts(&pane.pane_id);
                    let agent = snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.pane_id == pane.pane_id);
                    let pane_label = facts
                        .and_then(|facts| {
                            facts
                                .effective_title
                                .clone()
                                .or_else(|| facts.manual_label.clone())
                                .or_else(|| facts.agent_name.clone())
                                .or_else(|| facts.agent_label.clone())
                                .or_else(|| crate::app::launch_label(facts.launch_argv.as_ref()))
                                .or_else(|| Some(format!("pane {}", facts.number)))
                        })
                        .or_else(|| pane.label.clone())
                        .or_else(|| agent.and_then(|agent| agent.title.clone()));
                    // Missing display facts must not remove an addressable pane or invent its number.
                    let pane_label = pane_label.unwrap_or_default();
                    let (status, seen) = facts
                        .map(|facts| (super::super::sidebar::state(facts.state).0, facts.seen))
                        .unwrap_or_else(|| {
                            agent
                                .map(|agent| super::super::sidebar::state(agent.agent_status))
                                .unwrap_or((crate::detect::AgentState::Unknown, true))
                        });
                    let agent_label = facts
                        .and_then(|facts| {
                            facts
                                .display_agent
                                .clone()
                                .or_else(|| facts.agent_name.clone())
                                .or_else(|| facts.agent_label.clone())
                        })
                        .or_else(|| {
                            agent.and_then(|agent| {
                                agent.display_agent.clone().or_else(|| agent.agent.clone())
                            })
                        });
                    let status_label = facts
                        .and_then(|facts| facts.state_labels.get(label(status, seen)).cloned())
                        .unwrap_or_else(|| label(status, seen).to_owned());
                    let meta = agent_label
                        .as_ref()
                        .map(|agent| format!("{agent} · {status_label}"))
                        .unwrap_or_else(|| "shell".into());
                    let mut detail = vec![workspace.label.clone()];
                    if multi {
                        detail.push(format!("tab: {}", tab.label));
                    }
                    if let Some(facts) = facts {
                        detail.push(format!("pane {}", facts.number));
                        if let Some(title) = &facts.title {
                            detail.push(title.clone());
                        }
                    }
                    if let Some(agent) = agent_label {
                        detail.push(agent);
                        detail.push(status_label);
                    } else {
                        detail.push("shell".into());
                    }
                    let row = NavigatorRow {
                        target: target(&pane.pane_id, Kind::Pane, detail.join(" · ")),
                        depth: if multi { 2 } else { 1 },
                        search_text: format!("{pane_label} {meta}").to_lowercase(),
                        label: pane_label,
                        meta,
                        status,
                        seen,
                        is_current: current && pane.focused,
                        is_workspace: false,
                        is_tab: false,
                        expanded: false,
                    };
                    if (nav.state.state_filter.is_none() && tab_matches)
                        || matches(&nav.state, &row)
                    {
                        pane_rows.push(row);
                    }
                }
                if multi && (tab_matches || !pane_rows.is_empty()) {
                    children.push(tab_row);
                }
                children.extend(pane_rows);
            }
            if matches(&nav.state, &workspace_row) || !children.is_empty() {
                rows.push(workspace_row);
                if expanded {
                    rows.extend(children);
                }
            }
        }
    }
    rows
}
pub(super) fn open(frontend: &mut ClientFrontend) {
    selection::clear(frontend);
    frontend.prefix = false;
    frontend.mobile = None;
    let expanded = frontend
        .runtime
        .shell
        .endpoints
        .iter()
        .flat_map(|endpoint| {
            endpoint
                .cache
                .snapshot()
                .into_iter()
                .flat_map(move |snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .map(move |workspace| ResourceKey {
                            endpoint: endpoint.endpoint_id.clone(),
                            id: workspace.workspace_id.clone(),
                        })
                })
        })
        .collect();
    frontend.navigator = Some(Navigator {
        state: NavigatorState::default(),
        expanded,
        selected: None,
        presented: None,
    });
    let rows = rows(frontend);
    let index = rows
        .iter()
        .position(|row| row.target.kind == Kind::Pane && row.is_current)
        .or_else(|| rows.iter().position(|row| row.is_current))
        .unwrap_or(0);
    choose(frontend, index);
    frontend.force_redraw = true;
}
fn choose(frontend: &mut ClientFrontend, index: usize) {
    let rows = rows(frontend);
    let height = usize::from(body(frontend).height);
    let Some(nav) = &mut frontend.navigator else {
        return;
    };
    nav.state.selected = index.min(rows.len().saturating_sub(1));
    nav.selected = rows.get(nav.state.selected).map(|row| row.target.clone());
    if height == 0 {
        nav.state.scroll = 0;
    } else {
        if nav.state.selected < nav.state.scroll {
            nav.state.scroll = nav.state.selected;
        } else if nav.state.selected >= nav.state.scroll.saturating_add(height) {
            nav.state.scroll = nav.state.selected.saturating_add(1).saturating_sub(height);
        }
        nav.state.scroll = nav.state.scroll.min(rows.len().saturating_sub(height));
    }
}
fn valid(frontend: &ClientFrontend, target: &Target) -> bool {
    let Some(endpoint) = frontend.runtime.shell.endpoint(&target.key.endpoint) else {
        return false;
    };
    let Some(generation) = target
        .generation
        .filter(|generation| Some(*generation) == endpoint.generation)
    else {
        return false;
    };
    let Some(snapshot) = endpoint
        .cache
        .live_snapshot(generation)
        .filter(|snapshot| snapshot.boot_id == target.boot)
    else {
        return false;
    };
    match target.kind {
        Kind::Workspace => snapshot
            .workspaces
            .iter()
            .any(|row| row.workspace_id == target.key.id),
        Kind::Tab => snapshot.tabs.iter().any(|row| row.tab_id == target.key.id),
        Kind::Pane => snapshot
            .panes
            .iter()
            .any(|row| row.pane_id == target.key.id),
    }
}
fn accept(frontend: &mut ClientFrontend, target: Target) -> io::Result<()> {
    if !valid(frontend, &target) {
        return Ok(());
    }
    let focus = match target.kind {
        Kind::Workspace => FocusTarget::Workspace(target.key.id),
        Kind::Tab => FocusTarget::Tab(target.key.id),
        Kind::Pane => FocusTarget::Pane(target.key.id),
    };
    let update = frontend
        .runtime
        .activate(target.key.endpoint, Some(focus), Instant::now());
    frontend.update(update)?;
    frontend.navigator = None;
    Ok(())
}
fn toggle(frontend: &mut ClientFrontend, target: &Target) {
    if target.kind != Kind::Workspace || !valid(frontend, target) {
        return;
    }
    if let Some(nav) = &mut frontend.navigator {
        if !nav.expanded.remove(&target.key) {
            nav.expanded.insert(target.key.clone());
        }
        let selected = nav.state.selected;
        choose(frontend, selected);
    }
}
pub(super) fn observe(frontend: &mut ClientFrontend) {
    let rows = rows(frontend);
    let Some(nav) = &mut frontend.navigator else {
        return;
    };
    if let Some(target) = &nav.selected {
        if let Some(index) = rows.iter().position(|row| {
            row.target.key == target.key
                && row.target.kind == target.kind
                && row.target.generation == target.generation
                && row.target.boot == target.boot
        }) {
            nav.state.selected = index;
            nav.selected = Some(rows[index].target.clone());
        }
    }
}
pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(nav) = &frontend.navigator else {
        return;
    };
    let rows = rows(frontend);
    let popup = crate::ui::navigator_popup_rect(area(frontend));
    let inner = crate::ui::navigator_inner_rect(popup);
    let detail = nav
        .selected
        .as_ref()
        .map(|target| target.detail.as_str())
        .unwrap_or_default();
    crate::ui::render_navigator_from(
        &crate::ui::NavigatorRender {
            palette: &frontend.chrome.settings.palette,
            navigator: &nav.state,
            spinner_tick: frontend.chrome.spinner_tick,
            rows: &rows,
            pane_count: frontend
                .runtime
                .shell
                .endpoints
                .iter()
                .filter_map(|endpoint| endpoint.cache.snapshot())
                .map(|snapshot| snapshot.panes.len())
                .sum(),
            detail,
            popup,
            search: crate::ui::navigator_search_rect(inner),
            body: crate::ui::navigator_body_rect(inner),
            detail_area: crate::ui::navigator_detail_rect(inner),
            footer: crate::ui::navigator_footer_rect(inner),
        },
        frame,
    );
}
pub(super) fn presentation(frontend: &ClientFrontend) -> Option<Presented> {
    let nav = frontend.navigator.as_ref()?;
    let popup = crate::ui::navigator_popup_rect(area(frontend));
    Some(Presented {
        screen: area(frontend),
        body: body(frontend),
        search: crate::ui::navigator_search_rect(crate::ui::navigator_inner_rect(popup)),
        popup,
        scroll: nav.state.scroll,
        rows: rows(frontend),
        revisions: frontend
            .runtime
            .shell
            .endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint.cache.snapshot().map(|snapshot| {
                    (
                        endpoint.endpoint_id.clone(),
                        endpoint.generation,
                        snapshot.boot_id.clone(),
                        snapshot.revision,
                    )
                })
            })
            .collect(),
    })
}
pub(super) fn commit(frontend: &mut ClientFrontend, presented: Option<Presented>) {
    if let Some(nav) = &mut frontend.navigator {
        nav.presented = presented;
    }
}
pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    let Some(nav) = &frontend.navigator else {
        return Ok(false);
    };
    match event {
        RawInputEvent::OuterFocusLost | RawInputEvent::OuterFocusGained => return Ok(false),
        RawInputEvent::Paste(text) if nav.state.search_focused => {
            let nav = frontend.navigator.as_mut().expect("Navigator checked");
            nav.state.state_filter = None;
            nav.state.query.push_str(text);
            let index = nav.state.selected;
            choose(frontend, index);
        }
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
            let height = body(frontend).height;
            let count = rows(frontend).len();
            let nav = frontend.navigator.as_mut().expect("Navigator checked");
            let action =
                crate::app::navigator_key_action(&mut nav.state, key.as_key_event(), height, count);
            let index = nav.state.selected;
            let target = nav.selected.clone();
            match action {
                crate::app::NavigatorKeyAction::None => {}
                crate::app::NavigatorKeyAction::Close => frontend.navigator = None,
                crate::app::NavigatorKeyAction::Accept => {
                    if let Some(target) = target {
                        accept(frontend, target)?;
                    }
                }
                crate::app::NavigatorKeyAction::Clamp
                | crate::app::NavigatorKeyAction::EnsureVisible => choose(frontend, index),
                crate::app::NavigatorKeyAction::Move(delta) => choose(
                    frontend,
                    (index as isize + delta).clamp(0, count.saturating_sub(1) as isize) as usize,
                ),
                crate::app::NavigatorKeyAction::ToggleWorkspace => {
                    if let Some(target) = target {
                        toggle(frontend, &target);
                    }
                }
            }
        }
        RawInputEvent::Mouse(mouse) => {
            let Some(presented) = &nav.presented else {
                return Ok(true);
            };
            if presented.screen != area(frontend)
                || presented.scroll != nav.state.scroll
                || presented
                    .revisions
                    .iter()
                    .any(|(id, generation, boot, revision)| {
                        frontend.runtime.shell.endpoint(id).is_none_or(|endpoint| {
                            endpoint.generation != *generation
                                || endpoint.cache.snapshot().is_none_or(|snapshot| {
                                    snapshot.boot_id != *boot || snapshot.revision != *revision
                                })
                        })
                    })
            {
                return Ok(true);
            }
            let popup = presented.popup;
            let search = presented.search;
            let body = presented.body;
            let target = mouse
                .row
                .checked_sub(body.y)
                .and_then(|y| presented.rows.get(presented.scroll + usize::from(y)))
                .map(|row| row.target.clone());
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if search.contains((mouse.column, mouse.row).into()) =>
                {
                    if let Some(nav) = &mut frontend.navigator {
                        nav.state.search_focused = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left)
                    if body.contains((mouse.column, mouse.row).into()) =>
                {
                    if let Some(target) = target {
                        if mouse.column <= body.x.saturating_add(3)
                            && target.kind == Kind::Workspace
                        {
                            if valid(frontend, &target) {
                                choose(
                                    frontend,
                                    presented.scroll + usize::from(mouse.row - body.y),
                                );
                                toggle(frontend, &target);
                            }
                        } else {
                            accept(frontend, target)?;
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left)
                    if !popup.contains((mouse.column, mouse.row).into()) =>
                {
                    frontend.navigator = None
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let down = mouse.kind == MouseEventKind::ScrollDown;
                    let count = rows(frontend).len();
                    if let Some(nav) = &mut frontend.navigator {
                        nav.state.scroll = if down {
                            nav.state
                                .scroll
                                .saturating_add(3)
                                .min(count.saturating_sub(usize::from(body.height)))
                        } else {
                            nav.state.scroll.saturating_sub(3)
                        };
                        let index = nav.state.scroll;
                        choose(frontend, index);
                    }
                }
                MouseEventKind::Moved if body.contains((mouse.column, mouse.row).into()) => {
                    choose(frontend, presented.scroll + usize::from(mouse.row - body.y))
                }
                _ => {}
            }
        }
        _ => {}
    }
    frontend.force_redraw = true;
    Ok(true)
}

#[cfg(test)]
pub(super) fn test_rows(frontend: &ClientFrontend) -> (usize, usize) {
    let rows = rows(frontend);
    (
        rows.len(),
        rows.iter()
            .filter(|row| row.target.kind == Kind::Pane)
            .count(),
    )
}
#[cfg(test)]
pub(super) fn test_query(frontend: &ClientFrontend) -> &str {
    frontend
        .navigator
        .as_ref()
        .map(|nav| nav.state.query.as_str())
        .unwrap_or_default()
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<Rect> {
    frontend.navigator.as_ref()?;
    Some(crate::ui::navigator_popup_rect(area(frontend)))
}
