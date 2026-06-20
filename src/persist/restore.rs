use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::PaneState;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::workspace::Workspace;

use super::{DirectionSnapshot, LayoutSnapshot, SessionSnapshot, TabSnapshot, WorkspaceSnapshot};

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    preserve_pane_ids: bool,
    default_shell: &str,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> (
    Vec<Workspace>,
    HashMap<TerminalId, TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
) {
    let mut workspaces = Vec::new();
    let mut terminals = HashMap::new();
    let mut terminal_runtimes = HashMap::new();
    for ws_snap in &snapshot.workspaces {
        if let Some((workspace, restored_terminals, restored_runtimes)) = restore_workspace(
            ws_snap,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            preserve_pane_ids,
            default_shell,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        ) {
            for terminal in restored_terminals {
                terminals.insert(terminal.id.clone(), terminal);
            }
            terminal_runtimes.extend(restored_runtimes);
            workspaces.push(workspace);
        }
    }
    (workspaces, terminals, terminal_runtimes)
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    preserve_pane_ids: bool,
    default_shell: &str,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<(
    Workspace,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
)> {
    let mut tabs = Vec::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_pane_number = 1;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let (tab, restored_terminals, restored_runtimes) = restore_tab(
            tab_snap,
            idx + 1,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            preserve_pane_ids,
            default_shell,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        )?;
        for pane_id in tab.layout.pane_ids() {
            let pane_number = if preserve_pane_ids {
                pane_id.raw() as usize
            } else {
                next_public_pane_number
            };
            public_pane_numbers.insert(pane_id, pane_number);
            next_public_pane_number = next_public_pane_number.max(pane_number.saturating_add(1));
        }
        terminals.extend(restored_terminals);
        terminal_runtimes.extend(restored_runtimes);
        tabs.push(tab);
    }

    if tabs.is_empty() {
        return None;
    }

    Some((
        Workspace {
            id: snap
                .id
                .clone()
                .unwrap_or_else(crate::workspace::generate_workspace_id),
            custom_name: snap.custom_name.clone(),
            section: snap.section,
            identity_cwd: snap.identity_cwd.clone(),
            cached_git_branch: None,
            cached_git_ahead_behind: None,
            cached_git_diff_stats: None,
            public_pane_numbers,
            next_public_pane_number,
            active_tab: snap.active_tab.min(tabs.len().saturating_sub(1)),
            tabs,
            #[cfg(test)]
            test_runtimes: HashMap::new(),
        },
        terminals,
        terminal_runtimes,
    ))
}

fn restore_tab(
    snap: &TabSnapshot,
    number: usize,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    preserve_pane_ids: bool,
    default_shell: &str,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<(
    crate::workspace::Tab,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
)> {
    let (node, saved_id_for_pane) = if preserve_pane_ids {
        (restore_node_preserving_ids(&snap.layout), HashMap::new())
    } else {
        let (node, id_map) = restore_node_remapped(&snap.layout);
        let reverse_id_map: HashMap<PaneId, u32> = id_map
            .iter()
            .map(|(&old_id, &new_id)| (new_id, old_id))
            .collect();
        (node, reverse_id_map)
    };
    let pane_ids = collect_pane_ids(&node);

    let mut panes = HashMap::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    for id in &pane_ids {
        let saved_id = saved_id_for_pane
            .get(id)
            .copied()
            .unwrap_or_else(|| id.raw());
        let saved_cwd = snap
            .panes
            .get(&saved_id)
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let cwd = if saved_cwd.exists() {
            saved_cwd
        } else {
            warn!(
                cwd = %saved_cwd.display(),
                "saved pane cwd does not exist, falling back to HOME"
            );
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"));
            if home.exists() {
                home
            } else {
                PathBuf::from("/")
            }
        };

        let saved_label = snap.panes.get(&saved_id).and_then(|p| p.label.clone());
        let saved_agent_name = snap.panes.get(&saved_id).and_then(|p| p.agent_name.clone());
        let saved_agent_restore = snap
            .panes
            .get(&saved_id)
            .and_then(|p| p.agent_restore.clone());

        match TerminalRuntime::spawn(
            *id,
            rows,
            cols,
            cwd.clone(),
            scrollback_limit_bytes,
            host_terminal_theme,
            default_shell,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        ) {
            Ok(runtime) => {
                let terminal_id = TerminalId::alloc();
                let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone());
                if let Some(label) = saved_label {
                    terminal.set_manual_label(label);
                }
                if let Some(agent_name) = saved_agent_name {
                    terminal.set_agent_name(agent_name);
                }
                if let Some(agent_restore) = saved_agent_restore {
                    terminal.pending_restore = Some(crate::terminal::PendingAgentRestore {
                        agent: agent_restore.agent,
                        session_id: agent_restore.session_id,
                    });
                }
                panes.insert(*id, PaneState::new(terminal_id.clone()));
                terminal_runtimes.insert(terminal_id, runtime);
                terminals.push(terminal);
            }
            Err(e) => {
                error!(
                    tab = ?snap.custom_name,
                    pane_id = id.raw(),
                    err = %e,
                    "failed to restore pane, skipping"
                );
            }
        }
    }

    if panes.is_empty() {
        warn!(
            tab = ?snap.custom_name,
            "no panes could be restored for tab, dropping it"
        );
        return None;
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(node) = prune_restored_node(node, &surviving) else {
        warn!(
            tab = ?snap.custom_name,
            "restored tab lost all panes after pruning missing layout nodes"
        );
        return None;
    };
    let pane_ids = collect_pane_ids(&node);
    let focus = resolve_restored_pane(snap.focused, &surviving, &pane_ids)?;
    let root_pane = resolve_restored_pane(snap.root_pane, &surviving, &pane_ids)?;
    let layout = TileLayout::from_saved(node, focus);

    Some((
        crate::workspace::Tab {
            custom_name: snap.custom_name.clone(),
            number,
            root_pane,
            layout,
            panes,
            #[cfg(test)]
            runtimes: HashMap::new(),
            zoomed: snap.zoomed,
            events,
            render_notify,
            render_dirty,
        },
        terminals,
        terminal_runtimes,
    ))
}

pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_id: Option<u32>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_id
        .map(PaneId::from_raw)
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

/// Restore a layout tree with the saved PaneIds intact.
pub(super) fn restore_node_preserving_ids(snap: &LayoutSnapshot) -> Node {
    restore_inner(snap)
}

/// Restore a layout tree for duplication by assigning fresh PaneIds.
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

fn restore_inner(snap: &LayoutSnapshot) -> Node {
    match snap {
        LayoutSnapshot::Pane(id) => {
            let pane_id = PaneId::from_raw(*id);
            PaneId::reserve_next_after(pane_id);
            Node::Pane(pane_id)
        }
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = restore_inner(first);
            let second_node = restore_inner(second);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_node_preserves_pane_ids() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(0))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(1))),
                second: Box::new(Node::Pane(PaneId::from_raw(2))),
            }),
        };

        let snap = super::super::snapshot::capture_node(&node);
        let restored = restore_node_preserving_ids(&snap);

        let ids: Vec<u32> = collect_pane_ids(&restored)
            .into_iter()
            .map(|id| id.raw())
            .collect();
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };
        let surviving = std::collections::HashSet::from([keep]);

        let pruned = prune_restored_node(node, &surviving).expect("remaining pane should survive");

        assert!(matches!(pruned, Node::Pane(id) if id == keep));
    }

    #[test]
    fn resolve_restored_pane_prefers_surviving_saved_id_and_falls_back_to_first_remaining() {
        let first = PaneId::from_raw(21);
        let surviving = std::collections::HashSet::from([first]);
        let pane_ids = vec![first];

        assert_eq!(
            resolve_restored_pane(Some(21), &surviving, &pane_ids),
            Some(first)
        );
        assert_eq!(
            resolve_restored_pane(Some(22), &surviving, &pane_ids),
            Some(first)
        );
    }
}
