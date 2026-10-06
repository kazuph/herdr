use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::super::{
    shell::ClientShellState,
    sidebar::{state, SidebarProjection},
    ClientEndpointStatus,
};
use super::{status_text, ChromeHit, ChromeTarget, ChromeView, ClientChrome};
use crate::config::WorkspacePanelDensityConfig;

struct Row {
    target: Option<ChromeTarget>,
    lines: Vec<Line<'static>>,
    style: Style,
    gap: u16,
    indented: bool,
    grouped_parent: bool,
}

fn machine_row(
    endpoint: &super::super::shell::ClientShellEndpoint,
    collapsed: bool,
    palette: &crate::app::state::Palette,
) -> Row {
    Row {
        target: Some(ChromeTarget::Machine(endpoint.endpoint_id.clone())),
        lines: vec![Line::from(Span::styled(
            format!(
                "{} {}: {}",
                if collapsed { "▸" } else { "▾" },
                endpoint.label,
                status_text(endpoint.status)
            ),
            Style::default().fg(if endpoint.status == ClientEndpointStatus::Online {
                palette.text
            } else {
                palette.overlay0
            }),
        ))],
        style: Style::default(),
        gap: 0,
        indented: false,
        grouped_parent: false,
    }
}

fn token_line(
    tokens: &[crate::ui::sidebar::tokens::ResolvedToken],
    width: u16,
    prefix: &str,
    status: crate::api::schema::AgentStatus,
    selected: bool,
    stale: bool,
    chrome: &ClientChrome,
) -> Line<'static> {
    let p = &chrome.settings.palette;
    let (state, seen) = state(status);
    let dim = if stale {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    let icon = crate::ui::state_summary_icon(state, seen, chrome.spinner_tick, p);
    let primary = Style::default()
        .fg(if selected { p.text } else { p.subtext0 })
        .add_modifier(dim);
    let secondary = Style::default().fg(p.overlay0).add_modifier(dim);
    let prefix_width = unicode_width::UnicodeWidthStr::width(prefix);
    let mut spans = if prefix.starts_with('▸') || prefix.starts_with('▾') {
        vec![
            Span::styled(
                prefix[..prefix.len() - 1].to_owned(),
                Style::default().fg(p.accent),
            ),
            Span::raw(" "),
        ]
    } else {
        vec![Span::styled(prefix.to_owned(), primary)]
    };
    spans.extend(crate::ui::sidebar::resolved_token_spans(
        tokens,
        icon,
        Style::default()
            .fg(crate::ui::state_label_color(state, seen, p))
            .add_modifier(Modifier::DIM),
        primary,
        secondary,
        secondary,
        p,
        usize::from(width).saturating_sub(prefix_width),
    ));
    Line::from(crate::ui::sidebar::clip_token_spans(
        spans,
        usize::from(width),
    ))
}

fn workspace_rows(
    chrome: &ClientChrome,
    shell: &ClientShellState,
    width: u16,
    force_expanded: bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for endpoint in &shell.endpoints {
        let collapsed = chrome.collapsed_machines.contains(&endpoint.endpoint_id);
        if endpoint.endpoint_id != super::super::ClientEndpointId::Local {
            rows.push(machine_row(endpoint, collapsed, &chrome.settings.palette));
        }
        if collapsed || endpoint.status == ClientEndpointStatus::Disabled {
            continue;
        }
        let Some(snapshot) = endpoint.cache.snapshot() else {
            continue;
        };
        let projection = SidebarProjection {
            endpoint: &endpoint.endpoint_id,
            snapshot,
            jobs: endpoint.jobs.for_snapshot(snapshot),
        };
        let memberships = snapshot
            .workspaces
            .iter()
            .map(|workspace| {
                workspace
                    .worktree
                    .as_ref()
                    .map(|space| (space.key.as_str(), space.is_linked_worktree))
            })
            .collect::<Vec<_>>();
        let visible = if let Some(selected) = &chrome.navigate_selection {
            (selected.endpoint == endpoint.endpoint_id)
                .then(|| {
                    snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace_id == selected.id)
                })
                .flatten()
        } else {
            (endpoint.endpoint_id == shell.active_endpoint_id)
                .then(|| {
                    snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.focused)
                })
                .flatten()
        };
        let entries = crate::ui::sidebar::workspace_group_entries(
            &memberships,
            visible,
            force_expanded,
            |key| {
                chrome
                    .collapsed_groups
                    .contains(&(endpoint.endpoint_id.clone(), key.to_owned()))
            },
        );
        for (entry_index, entry) in entries.iter().enumerate() {
            let crate::ui::sidebar::WorkspaceListEntry::Workspace { ws_idx, indented } = *entry;
            let workspace = &snapshot.workspaces[ws_idx];
            let parent = workspace.worktree.as_ref().filter(|space| {
                !indented
                    && !space.is_linked_worktree
                    && snapshot
                        .workspaces
                        .iter()
                        .filter(|member| {
                            member
                                .worktree
                                .as_ref()
                                .is_some_and(|membership| membership.key == space.key)
                        })
                        .count()
                        >= 2
            });
            let group_collapsed = parent.is_some_and(|space| {
                chrome
                    .collapsed_groups
                    .contains(&(endpoint.endpoint_id.clone(), space.key.clone()))
            });
            let mut displayed = workspace.clone();
            if let Some(space) = parent.filter(|_| group_collapsed) {
                displayed.agent_status = snapshot
                    .workspaces
                    .iter()
                    .filter(|member| {
                        member
                            .worktree
                            .as_ref()
                            .is_some_and(|membership| membership.key == space.key)
                    })
                    .map(|member| member.agent_status)
                    .max_by_key(|status| {
                        let (state, seen) = state(*status);
                        crate::ui::sidebar::workspace_attention_priority(state, seen)
                    })
                    .unwrap_or(workspace.agent_status);
            }
            let selected = chrome.navigate_selection.as_ref().map_or(
                endpoint.endpoint_id == shell.active_endpoint_id && workspace.focused,
                |selected| {
                    selected.endpoint == endpoint.endpoint_id
                        && selected.id == workspace.workspace_id
                },
            );
            let stale = endpoint.status != ClientEndpointStatus::Online;
            let compact = chrome.settings.sidebar_collapsed;
            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let tokens = projection.workspace_rows(
                &chrome.settings.spaces,
                &displayed,
                None,
                indented,
                usize::MAX,
                now_unix_ms,
            );
            let height = if compact {
                1
            } else {
                match chrome.settings.density {
                    WorkspacePanelDensityConfig::Slim => 2,
                    WorkspacePanelDensityConfig::Full => {
                        tokens.len().max(3).min(u16::MAX as usize) as u16
                    }
                }
            };
            let mut lines = Vec::new();
            for index in 0..usize::from(height) {
                if compact {
                    let (state, seen) = state(workspace.agent_status);
                    let (icon, style) = crate::ui::state_summary_icon(
                        state,
                        seen,
                        chrome.spinner_tick,
                        &chrome.settings.palette,
                    );
                    lines.push(Line::from(vec![
                        Span::raw(format!(" {}", workspace.number)),
                        Span::styled(icon, style),
                    ]));
                } else {
                    // Match the fork card indentation and visible-height job placement.
                    let resolved = projection.workspace_rows(
                        &chrome.settings.spaces,
                        &displayed,
                        None,
                        indented,
                        usize::from(height),
                        now_unix_ms,
                    );
                    lines.push(token_line(
                        resolved.get(index).map_or(&[], Vec::as_slice),
                        width,
                        if index == 0 {
                            if indented {
                                "   "
                            } else if parent.is_some() {
                                if group_collapsed {
                                    "▸ "
                                } else {
                                    "▾ "
                                }
                            } else {
                                " "
                            }
                        } else if indented {
                            "     "
                        } else {
                            "   "
                        },
                        displayed.agent_status,
                        selected,
                        stale,
                        chrome,
                    ));
                }
            }
            rows.push(Row {
                target: Some(ChromeTarget::Workspace(
                    projection.key(&workspace.workspace_id),
                )),
                lines,
                style: Style::default().bg(if selected {
                    chrome.settings.palette.surface_dim
                } else {
                    chrome.settings.palette.surface0
                }),
                gap: if compact
                    || entry_index + 1 == entries.len()
                    || (indented
                        && crate::ui::sidebar::next_entry_is_indented_workspace(
                            &entries,
                            entry_index,
                        ))
                {
                    0
                } else {
                    chrome.settings.spaces.row_gap
                },
                indented,
                grouped_parent: parent.is_some(),
            });
        }
    }
    rows
}

pub(super) fn visual_workspace_targets(
    chrome: &ClientChrome,
    shell: &ClientShellState,
    width: u16,
    mobile: bool,
) -> Vec<super::super::ResourceKey> {
    let rows = workspace_rows(chrome, shell, width, mobile);
    let mut result = Vec::new();
    for endpoint in &shell.endpoints {
        let keys = rows
            .iter()
            .filter_map(|row| match &row.target {
                Some(ChromeTarget::Workspace(key)) if key.endpoint == endpoint.endpoint_id => {
                    Some(key)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let provided = keys.iter().all(|key| {
            endpoint
                .cache
                .displayed_workspace_facts(&key.id)
                .and_then(|fact| fact.section)
                .is_some()
        });
        if provided && endpoint.endpoint_id.is_local() {
            for section in crate::workspace::WorkspaceSection::ALL {
                if chrome
                    .collapsed_sections
                    .contains(&(endpoint.endpoint_id.clone(), section))
                {
                    continue;
                }
                result.extend(
                    keys.iter()
                        .filter(|key| {
                            endpoint
                                .cache
                                .displayed_workspace_facts(&key.id)
                                .and_then(|fact| fact.section)
                                == Some(section)
                        })
                        .map(|key| (*key).clone()),
                );
            }
        } else {
            result.extend(keys.into_iter().cloned());
        }
    }
    result
}

enum WorkspaceHeader {
    Machine(usize),
    Section(
        super::super::ClientEndpointId,
        crate::workspace::WorkspaceSection,
    ),
    None,
}

fn place_workspace_sections(
    chrome: &mut ClientChrome,
    shell: &ClientShellState,
    area: Rect,
    view: &mut ChromeView,
) -> usize {
    if chrome.settings.sidebar_collapsed {
        return place(
            workspace_rows(chrome, shell, area.width, false),
            area,
            &mut chrome.workspace_scroll,
            view,
        );
    }
    let rows = workspace_rows(chrome, shell, area.width, false);
    let mut blocks = Vec::new();
    let mut owners = Vec::new();
    for endpoint in &shell.endpoints {
        let cards = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match &row.target {
                Some(ChromeTarget::Workspace(key)) if key.endpoint == endpoint.endpoint_id => {
                    Some((index, key))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if endpoint.endpoint_id != super::super::ClientEndpointId::Local {
            let Some(row) = rows.iter().position(|row| matches!(&row.target, Some(ChromeTarget::Machine(id)) if id == &endpoint.endpoint_id)) else { continue; };
            let (header_offset, header_rows) =
                crate::ui::sidebar::workspace_section_header_geometry(blocks.is_empty());
            let len = cards.len();
            blocks.push(crate::ui::sidebar::WorkspaceLayoutBlock {
                header_rows,
                header_offset,
                expanded: !chrome.collapsed_machines.contains(&endpoint.endpoint_id),
                entries: cards
                    .iter()
                    .enumerate()
                    .map(
                        |(index, (card, _))| crate::ui::sidebar::WorkspaceSectionEntry {
                            key: *card,
                            height: rows[*card].lines.len() as u16,
                            indented: rows[*card].indented,
                            gap: if index + 1 < len { rows[*card].gap } else { 0 },
                        },
                    )
                    .collect(),
            });
            owners.push(WorkspaceHeader::Machine(row));
            continue;
        }
        if cards.is_empty() {
            continue;
        }
        let provided = cards.iter().all(|(_, key)| {
            endpoint
                .cache
                .displayed_workspace_facts(&key.id)
                .and_then(|facts| facts.section)
                .is_some()
        });
        let sections = if provided {
            crate::workspace::WorkspaceSection::ALL
                .into_iter()
                .filter_map(|section| {
                    let cards = cards
                        .iter()
                        .filter(|(_, key)| {
                            endpoint
                                .cache
                                .displayed_workspace_facts(&key.id)
                                .and_then(|facts| facts.section)
                                == Some(section)
                        })
                        .map(|(index, _)| *index)
                        .collect::<Vec<_>>();
                    (!cards.is_empty()).then_some((Some(section), cards))
                })
                .collect::<Vec<_>>()
        } else {
            vec![(None, cards.iter().map(|(index, _)| *index).collect())]
        };
        for (section_index, (section, cards)) in sections.into_iter().enumerate() {
            let (header_offset, header_rows) = section.map_or((0, 0), |_| {
                crate::ui::sidebar::workspace_section_header_geometry(section_index == 0)
            });
            let expanded = section.is_none_or(|section| {
                !chrome
                    .collapsed_sections
                    .contains(&(endpoint.endpoint_id.clone(), section))
            });
            let len = cards.len();
            blocks.push(crate::ui::sidebar::WorkspaceLayoutBlock {
                header_rows,
                header_offset,
                expanded,
                entries: cards
                    .into_iter()
                    .enumerate()
                    .map(|(index, card)| crate::ui::sidebar::WorkspaceSectionEntry {
                        key: card,
                        height: rows[card].lines.len() as u16,
                        indented: rows[card].indented,
                        gap: if index + 1 < len { rows[card].gap } else { 0 },
                    })
                    .collect(),
            });
            owners.push(section.map_or(WorkspaceHeader::None, |section| {
                WorkspaceHeader::Section(endpoint.endpoint_id.clone(), section)
            }));
        }
    }
    let slim = chrome.settings.density == WorkspacePanelDensityConfig::Slim;
    let count = blocks
        .iter()
        .filter(|block| block.expanded)
        .map(|block| block.entries.len())
        .sum::<usize>();
    let max_scroll = (0..count)
        .find(|start| {
            start.saturating_add(
                crate::ui::sidebar::workspace_block_layout(area, *start, slim, &blocks)
                    .0
                    .len(),
            ) >= count
        })
        .unwrap_or(count.saturating_sub(1));
    chrome.workspace_scroll = chrome.workspace_scroll.min(max_scroll);
    let selected = chrome.navigate_selection.clone().or_else(|| {
        shell
            .endpoint(&shell.active_endpoint_id)
            .and_then(|endpoint| endpoint.cache.snapshot())
            .and_then(|snapshot| snapshot.focused_workspace_id.as_ref())
            .map(|id| super::super::ResourceKey {
                endpoint: shell.active_endpoint_id.clone(),
                id: id.clone(),
            })
    });
    if selected != chrome.last_workspace_selection {
        chrome.last_workspace_selection = selected.clone();
        if let Some(selected) = &selected {
            if let Some(target) = rows.iter().position(
                |row| matches!(&row.target, Some(ChromeTarget::Workspace(key)) if key == selected),
            ) {
                let entry_index = blocks
                    .iter()
                    .filter(|block| block.expanded)
                    .flat_map(|block| block.entries.iter())
                    .position(|entry| entry.key == target);
                let mut cards = crate::ui::sidebar::workspace_block_layout(
                    area,
                    chrome.workspace_scroll,
                    slim,
                    &blocks,
                )
                .0;
                if !cards.iter().any(|(index, _)| *index == target) {
                    if let Some(index) =
                        entry_index.filter(|index| *index < chrome.workspace_scroll)
                    {
                        chrome.workspace_scroll = index;
                    } else {
                        while !cards.iter().any(|(index, _)| *index == target)
                            && chrome.workspace_scroll < max_scroll
                        {
                            chrome.workspace_scroll += 1;
                            cards = crate::ui::sidebar::workspace_block_layout(
                                area,
                                chrome.workspace_scroll,
                                slim,
                                &blocks,
                            )
                            .0;
                            if cards.is_empty() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    let (cards, headers) =
        crate::ui::sidebar::workspace_block_layout(area, chrome.workspace_scroll, slim, &blocks);
    for (index, rect) in cards {
        let row = &rows[index];
        view.backgrounds.push((rect, row.style));
        if let Some(target) = &row.target {
            if row.grouped_parent {
                if let ChromeTarget::Workspace(key) = target {
                    view.hits.push(ChromeHit {
                        rect: Rect::new(rect.x, rect.y, 1, 1),
                        target: ChromeTarget::WorkspaceGroup(key.clone()),
                    });
                }
            }
            view.hits.push(ChromeHit {
                rect,
                target: target.clone(),
            });
        }
        for (offset, line) in row.lines.iter().take(usize::from(rect.height)).enumerate() {
            view.lines.push((
                Rect::new(rect.x, rect.y + offset as u16, rect.width, 1),
                line.clone(),
            ));
        }
        if matches!(&row.target, Some(ChromeTarget::Workspace(key)) if Some(key) == selected.as_ref())
        {
            view.workspace_selection_band = Some(rect);
        }
    }
    for (index, rect) in headers {
        match &owners[index] {
            WorkspaceHeader::Machine(row) => {
                if let Some(ChromeTarget::Machine(id)) = &rows[*row].target {
                    let Some(endpoint) = shell.endpoint(id) else {
                        continue;
                    };
                    let expanded = !chrome.collapsed_machines.contains(id);
                    let new = crate::ui::sidebar::workspace_section_new_button_rect(rect);
                    let label = Rect::new(
                        rect.x,
                        rect.y,
                        if new.is_empty() {
                            rect.width
                        } else {
                            new.x.saturating_sub(rect.x)
                        },
                        rect.height,
                    );
                    view.hits.push(ChromeHit {
                        rect: label,
                        target: ChromeTarget::Machine(id.clone()),
                    });
                    if !new.is_empty() {
                        view.hits.push(ChromeHit {
                            rect: new,
                            target: ChromeTarget::NewWorkspaceInSection(
                                id.clone(),
                                crate::workspace::WorkspaceSection::None,
                            ),
                        });
                    }
                    view.machine_headers
                        .push((rect, endpoint.label.clone(), expanded));
                }
            }
            WorkspaceHeader::Section(endpoint, section) => {
                let expanded = !chrome
                    .collapsed_sections
                    .contains(&(endpoint.clone(), *section));
                let new = crate::ui::sidebar::workspace_section_new_button_rect(rect);
                let label = Rect::new(
                    rect.x,
                    rect.y,
                    if new.is_empty() {
                        rect.width
                    } else {
                        new.x.saturating_sub(rect.x)
                    },
                    rect.height,
                );
                view.hits.push(ChromeHit {
                    rect: label,
                    target: ChromeTarget::WorkspaceSection(endpoint.clone(), *section),
                });
                if !new.is_empty() {
                    view.hits.push(ChromeHit {
                        rect: new,
                        target: ChromeTarget::NewWorkspaceInSection(endpoint.clone(), *section),
                    });
                }
                view.section_headers.push((
                    crate::app::state::WorkspaceSectionHeaderArea {
                        section: *section,
                        rect,
                    },
                    expanded,
                ));
            }
            WorkspaceHeader::None => {}
        }
    }
    max_scroll
}

fn agent_rows(chrome: &ClientChrome, shell: &ClientShellState, width: u16) -> Vec<Row> {
    let mut rows = Vec::new();
    let multiple = shell.endpoints.len() > 1;
    for endpoint in &shell.endpoints {
        let collapsed = chrome.collapsed_machines.contains(&endpoint.endpoint_id);
        if multiple {
            rows.push(machine_row(endpoint, collapsed, &chrome.settings.palette));
        }
        if collapsed || endpoint.status == ClientEndpointStatus::Disabled {
            continue;
        }
        let Some(snapshot) = endpoint.cache.snapshot() else {
            continue;
        };
        let projection = SidebarProjection {
            endpoint: &endpoint.endpoint_id,
            snapshot,
            jobs: endpoint.jobs.for_snapshot(snapshot),
        };
        for id in &snapshot.agent_order {
            let Some(agent) = snapshot.agents.iter().find(|agent| &agent.pane_id == id) else {
                continue;
            };
            let Some(tokens) = projection.agent_rows(&chrome.settings.agents, agent) else {
                continue;
            };
            let selected = endpoint.endpoint_id == shell.active_endpoint_id && agent.focused;
            let stale = endpoint.status != ClientEndpointStatus::Online;
            let lines = tokens
                .iter()
                .map(|tokens| {
                    token_line(
                        tokens,
                        width,
                        " ",
                        agent.agent_status,
                        selected,
                        stale,
                        chrome,
                    )
                })
                .collect();
            rows.push(Row {
                target: Some(ChromeTarget::Agent(projection.key(&agent.pane_id))),
                lines,
                style: Style::default().bg(if selected {
                    chrome.settings.palette.surface_dim
                } else {
                    chrome.settings.palette.surface0
                }),
                gap: chrome.settings.agents.row_gap,
                indented: false,
                grouped_parent: false,
            });
        }
    }
    rows
}

pub(super) fn agent_targets(
    chrome: &ClientChrome,
    shell: &ClientShellState,
    width: u16,
) -> Vec<super::super::ResourceKey> {
    agent_rows(chrome, shell, width)
        .into_iter()
        .filter_map(|row| match row.target {
            Some(ChromeTarget::Agent(key)) => Some(key),
            _ => None,
        })
        .collect()
}

pub(super) fn ensure_agent_visible(
    chrome: &mut ClientChrome,
    shell: &ClientShellState,
    area: Rect,
    key: &super::super::ResourceKey,
) {
    if area.is_empty() || chrome.settings.sidebar_collapsed {
        return;
    }
    let rows = agent_rows(chrome, shell, area.width);
    let total = rows
        .iter()
        .map(|row| row.lines.len() + usize::from(row.gap))
        .sum::<usize>();
    let maximum = total.saturating_sub(usize::from(area.height));
    let mut start = 0usize;
    for row in rows {
        let end = start + row.lines.len();
        if matches!(&row.target, Some(ChromeTarget::Agent(target)) if target == key) {
            if start < chrome.agent_scroll {
                chrome.agent_scroll = start.min(maximum);
            } else if end > chrome.agent_scroll.saturating_add(usize::from(area.height)) {
                chrome.agent_scroll = end.saturating_sub(usize::from(area.height)).min(maximum);
            }
            break;
        }
        start = end + usize::from(row.gap);
    }
}

fn place(rows: Vec<Row>, area: Rect, scroll: &mut usize, view: &mut ChromeView) -> usize {
    let total = rows
        .iter()
        .map(|row| row.lines.len().saturating_add(usize::from(row.gap)))
        .sum::<usize>();
    let max_scroll = total.saturating_sub(usize::from(area.height));
    *scroll = (*scroll).min(max_scroll);
    let mut offset = 0usize;
    for row in rows {
        let end = offset + row.lines.len();
        if end > *scroll && offset < scroll.saturating_add(usize::from(area.height)) {
            let begin = offset.max(*scroll);
            let visible_end = end.min(scroll.saturating_add(usize::from(area.height)));
            let rect = Rect::new(
                area.x,
                area.y + (begin - *scroll) as u16,
                area.width,
                (visible_end - begin) as u16,
            );
            view.backgrounds.push((rect, row.style));
            if let Some(target) = row.target {
                view.hits.push(ChromeHit { rect, target });
            }
            for index in begin..visible_end {
                view.lines.push((
                    Rect::new(area.x, area.y + (index - *scroll) as u16, area.width, 1),
                    row.lines[index - offset].clone(),
                ));
            }
        }
        offset = end.saturating_add(usize::from(row.gap));
    }
    max_scroll
}

pub(super) fn compute(chrome: &mut ClientChrome, shell: &ClientShellState, view: &mut ChromeView) {
    let area = view.layout.sidebar;
    if area.is_empty() {
        return;
    }
    view.backgrounds
        .push((area, Style::default().bg(chrome.settings.palette.panel_bg)));
    let (spaces, agents) = if chrome.settings.sidebar_collapsed {
        let (spaces, _, agents) = crate::ui::sidebar::collapsed_sidebar_sections(area);
        (spaces, agents)
    } else {
        let (spaces, agents) = crate::ui::sidebar::expanded_sidebar_sections(
            area,
            chrome.settings.sidebar_section_split,
        );
        if chrome.settings.mouse_capture && spaces.height > 0 {
            let footer = Rect::new(spaces.x, spaces.y + spaces.height - 1, spaces.width, 1);
            view.menu_launcher = Rect::new(footer.x, footer.y, 6u16.min(footer.width.max(1)), 1);
            let new_width = 5u16.min(footer.width.max(1));
            let new_rect = Rect::new(
                footer.x + footer.width.saturating_sub(new_width),
                footer.y,
                new_width,
                1,
            );
            for (rect, label, target) in [
                (view.menu_launcher, "[menu]", ChromeTarget::GlobalMenu),
                (new_rect, "[new]", ChromeTarget::NewWorkspace),
            ] {
                view.lines.push((
                    rect,
                    Line::from(Span::styled(
                        label,
                        Style::default().fg(chrome.settings.palette.overlay0),
                    )),
                ));
                view.hits.push(ChromeHit { rect, target });
            }
        }
        // Existing fork spaces header2/footer1 and agents header3.
        view.lines.push((
            Rect::new(spaces.x, spaces.y, spaces.width, spaces.height.min(1)),
            Line::from(" spaces"),
        ));
        view.lines
            .extend(crate::ui::sidebar::sidebar_detail_header_lines(
                agents,
                chrome.detail_view,
                &chrome.settings.palette,
            ));
        if agents.height >= 3 {
            let (agents_tab, jobs_tab) = crate::ui::sidebar::sidebar_detail_tab_rects(agents);
            for (rect, tab) in [
                (agents_tab, crate::app::state::SidebarDetailView::Agents),
                (jobs_tab, crate::app::state::SidebarDetailView::Jobs),
            ] {
                if rect != Rect::default() {
                    view.hits.push(ChromeHit {
                        rect,
                        target: ChromeTarget::DetailTab(tab),
                    });
                }
            }
        }
        (
            Rect::new(
                spaces.x,
                spaces.y.saturating_add(spaces.height.min(2)),
                spaces.width,
                spaces.height.saturating_sub(3),
            ),
            crate::ui::sidebar::agent_panel_body_rect(agents, false),
        )
    };
    view.workspace_body = spaces;
    view.detail_body = agents;
    view.workspace_max_scroll = place_workspace_sections(chrome, shell, spaces, view);
    if !chrome.settings.sidebar_collapsed
        && chrome.detail_view == crate::app::state::SidebarDetailView::Jobs
    {
        if let Some(jobs) = shell
            .endpoint(&shell.active_endpoint_id)
            .and_then(|endpoint| {
                endpoint
                    .cache
                    .snapshot()
                    .and_then(|snapshot| endpoint.jobs.for_snapshot(snapshot))
            })
        {
            chrome.jobs_scroll = chrome.jobs_scroll.min(jobs.jobs.len().saturating_sub(1));
            view.agent_max_scroll = jobs.jobs.len().saturating_sub(1);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if jobs.jobs.is_empty() && !agents.is_empty() {
                view.lines.push((
                    Rect::new(agents.x, agents.y, agents.width, 1),
                    Line::from(Span::styled(
                        " no background jobs",
                        Style::default().fg(chrome.settings.palette.overlay0),
                    )),
                ));
            }
            for (rect, index) in crate::ui::sidebar::jobs_panel_rows_in_body(
                jobs.jobs.len(),
                chrome.jobs_scroll,
                agents,
            ) {
                let job = &jobs.jobs[index];
                view.hits.push(super::ChromeHit {
                    rect,
                    target: super::ChromeTarget::Job(super::super::ResourceKey {
                        endpoint: shell.active_endpoint_id.clone(),
                        id: job.id.clone(),
                    }),
                });
                view.lines.push((
                    rect,
                    crate::ui::sidebar::job_panel_line(job.into(), now, &chrome.settings.palette),
                ));
            }
        }
    } else {
        view.agent_max_scroll = place(
            agent_rows(chrome, shell, agents.width),
            agents,
            &mut chrome.agent_scroll,
            view,
        );
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use crate::client::endpoint::{chrome::ChromeSettings, ClientEndpointId};

    #[test]
    fn endpoint_chrome_managed_group_keeps_parent_order_child_tokens_and_active_exception() {
        let config = crate::config::Config::default();
        let mut chrome = ClientChrome::new(ChromeSettings::from_config(
            &config,
            crate::app::state::Palette::catppuccin(),
            None,
        ));
        let mut snapshot: crate::protocol::endpoint_wire::ClientShellSnapshot =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
            )))
            .unwrap();
        let mut parent = snapshot.workspaces[0].clone();
        parent.workspace_id = "opaque parent".into();
        parent.label = "ROOT".into();
        parent.focused = false;
        parent.agent_status = crate::api::schema::AgentStatus::Idle;
        parent.worktree = Some(crate::protocol::endpoint_wire::ClientShellWorktree {
            key: "opaque repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let mut child = parent.clone();
        child.workspace_id = "opaque child".into();
        child.label = "auto label".into();
        child.custom_label = false;
        child.branch = Some("worktree/short-child".into());
        child.focused = true;
        child.agent_status = crate::api::schema::AgentStatus::Blocked;
        child.worktree.as_mut().unwrap().is_linked_worktree = true;
        let mut other = child.clone();
        other.workspace_id = "opaque other".into();
        other.focused = false;
        other.branch = Some("worktree/other-child".into());
        snapshot.focused_workspace_id = Some(child.workspace_id.clone());
        // A child preceding its parent still emits the parent first.
        snapshot.workspaces = vec![child.clone(), other, parent];
        let mut shell = ClientShellState::new();
        shell.begin_connection(&ClientEndpointId::Local, 1);
        assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot));
        let targets = |rows: &[Row]| {
            rows.iter()
                .filter_map(|row| match &row.target {
                    Some(ChromeTarget::Workspace(key)) => Some(key.id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let rows = workspace_rows(&chrome, &shell, 26, false);
        assert_eq!(
            targets(&rows),
            ["opaque parent", "opaque child", "opaque other"]
        );
        assert!(rows[1].indented);
        assert_eq!(rows[1].gap, 0);
        let child_text = rows[1]
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(child_text.contains("short-child"));
        assert!(!child_text.contains("worktree/"));
        // A different endpoint using the same opaque key does not collapse Local.
        chrome
            .collapsed_groups
            .insert((ClientEndpointId::Ssh("other".into()), "opaque repo".into()));
        assert_eq!(
            targets(&workspace_rows(&chrome, &shell, 26, false)).len(),
            3
        );
        chrome
            .collapsed_groups
            .insert((ClientEndpointId::Local, "opaque repo".into()));
        let rows = workspace_rows(&chrome, &shell, 26, false);
        assert_eq!(targets(&rows), ["opaque parent", "opaque child"]);
        assert_eq!(rows[0].lines[0].spans[0].content, "▸");
        assert_eq!(
            rows[0].lines[0].spans[0].style.fg,
            Some(chrome.settings.palette.accent)
        );
        let view = chrome.compute_view(&shell, 160, 40);
        let chevron = view.hits.iter().find(|hit| matches!(&hit.target, ChromeTarget::WorkspaceGroup(key) if key.id == "opaque parent")).unwrap();
        assert_eq!((chevron.rect.width, chevron.rect.height), (1, 1));
        assert!(matches!(
            chrome.hit(&view, chevron.rect.x, chevron.rect.y),
            Some(ChromeTarget::WorkspaceGroup(_))
        ));
        assert!(matches!(
            chrome.hit(&view, chevron.rect.x + 1, chevron.rect.y),
            Some(ChromeTarget::Workspace(_))
        ));
        let endpoint = shell.endpoint_mut(&ClientEndpointId::Local).unwrap();
        let mut updated = endpoint.cache.snapshot().unwrap().clone();
        updated.revision += 1;
        for workspace in &mut updated.workspaces {
            workspace.focused = false;
        }
        updated.focused_workspace_id = None;
        assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, updated));
        assert_eq!(
            targets(&workspace_rows(&chrome, &shell, 26, false)),
            ["opaque parent"]
        );
        let ids = |keys: Vec<super::super::super::ResourceKey>| {
            keys.into_iter().map(|key| key.id).collect::<Vec<_>>()
        };
        assert_eq!(
            ids(chrome.visual_workspace_targets(&shell, 160)),
            ["opaque parent"]
        );
        assert_eq!(
            ids(chrome.visual_workspace_targets(&shell, chrome.settings.mobile_width_threshold)),
            ["opaque parent", "opaque child", "opaque other"]
        );
        let mut projected = crate::protocol::endpoint_projection::SnapshotJson::from(
            shell
                .endpoint(&ClientEndpointId::Local)
                .unwrap()
                .cache
                .snapshot()
                .unwrap()
                .clone(),
        );
        projected.revision += 1;
        for (id, section) in [
            ("opaque parent", crate::workspace::WorkspaceSection::None),
            ("opaque child", crate::workspace::WorkspaceSection::Favorite),
            ("opaque other", crate::workspace::WorkspaceSection::Work),
        ] {
            projected.workspace_facts.insert(
                id.into(),
                crate::protocol::endpoint_projection::WorkspaceFacts {
                    git_space: None,
                    section: Some(section),
                },
            );
        }
        assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, projected));
        chrome.collapsed_groups.clear();
        assert_eq!(
            ids(chrome.visual_workspace_targets(&shell, 160)),
            ["opaque child", "opaque other", "opaque parent"]
        );
        chrome.collapsed_sections.insert((
            ClientEndpointId::Local,
            crate::workspace::WorkspaceSection::Favorite,
        ));
        assert_eq!(
            ids(chrome.visual_workspace_targets(&shell, 160)),
            ["opaque other", "opaque parent"]
        );
        chrome
            .collapsed_groups
            .insert((ClientEndpointId::Local, "opaque repo".into()));
        assert_eq!(
            ids(chrome.visual_workspace_targets(&shell, chrome.settings.mobile_width_threshold)),
            ["opaque other", "opaque parent"]
        );
        chrome.collapsed_machines.insert(ClientEndpointId::Local);
        assert!(chrome.visual_workspace_targets(&shell, 160).is_empty());
    }
}
