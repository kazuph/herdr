//! Sidebar workspace drag in the endpoint client: reorder a space within its
//! section and move it to another section, as the fork sidebar always allowed.
//! The order and sections stay server-owned; the drop issues the same
//! `workspace.set_section` / `workspace.move` methods the legacy TUI used.
use super::super::chrome::{ChromeTarget, ChromeView};
use super::super::ResourceKey;
use super::*;
use crate::workspace::WorkspaceSection;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Rect;

/// Same threshold as the legacy sidebar: one cell of movement starts a drag.
const DRAG_THRESHOLD: u16 = 1;

pub(super) struct WorkspaceDrag {
    key: ResourceKey,
    start: (u16, u16),
    dragging: bool,
    drop: Option<Drop>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Drop {
    /// Section the space lands in; `None` where the sidebar shows no sections.
    pub(crate) section: Option<WorkspaceSection>,
    /// Server workspace index to insert before, as `workspace.move` takes it.
    pub(crate) insert_index: Option<usize>,
    /// Sidebar row of the insertion line.
    pub(crate) indicator_row: Option<u16>,
}

#[derive(Clone, Debug)]
pub(crate) enum Slot {
    Header {
        rect: Rect,
        section: WorkspaceSection,
    },
    Card {
        rect: Rect,
        id: String,
        section: Option<WorkspaceSection>,
    },
}

impl Slot {
    fn rect(&self) -> Rect {
        match self {
            Self::Header { rect, .. } | Self::Card { rect, .. } => *rect,
        }
    }
}

/// Where a drop at `row` lands, given the endpoint's visible headers and cards
/// (in drawing order) and the server's workspace order.
pub(crate) fn drop_at(slots: &[Slot], order: &[String], body: Rect, row: u16) -> Option<Drop> {
    if row < body.y || row >= body.bottom() || slots.is_empty() {
        return None;
    }
    let position = |id: &str| order.iter().position(|candidate| candidate == id);
    let anchor = slots
        .iter()
        .rposition(|slot| slot.rect().y <= row)
        .unwrap_or(0);
    let cards_of = |section: Option<WorkspaceSection>| {
        slots
            .iter()
            .filter_map(move |slot| match slot {
                Slot::Card {
                    rect,
                    id,
                    section: card_section,
                } if *card_section == section => Some((*rect, id.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    match &slots[anchor] {
        Slot::Header { section, .. } => {
            let first = cards_of(Some(*section)).first().copied();
            Some(Drop {
                section: Some(*section),
                insert_index: first.and_then(|(_, id)| position(id)),
                indicator_row: first.map(|(rect, _)| rect.y),
            })
        }
        Slot::Card { rect, id, section } => {
            let cards = cards_of(*section);
            let index = cards.iter().position(|(_, card)| card == id)?;
            let before = row < rect.bottom() && row - rect.y < rect.height.div_ceil(2);
            if before {
                Some(Drop {
                    section: *section,
                    insert_index: position(id),
                    indicator_row: Some(if index == 0 {
                        rect.y
                    } else {
                        rect.y.saturating_sub(1)
                    }),
                })
            } else {
                let insert_index = match cards.get(index + 1) {
                    Some((_, next)) => position(next),
                    None => position(id).map(|at| at + 1),
                };
                Some(Drop {
                    section: *section,
                    insert_index,
                    indicator_row: Some(rect.bottom()).filter(|row| *row < body.bottom()),
                })
            }
        }
    }
}

/// A left press on a space card: remember it so a move turns it into a drag.
pub(super) fn press(frontend: &mut ClientFrontend, key: &ResourceKey, column: u16, row: u16) {
    frontend.workspace_drag = Some(WorkspaceDrag {
        key: key.clone(),
        start: (column, row),
        dragging: false,
        drop: None,
    });
}

fn slots(
    frontend: &ClientFrontend,
    view: &ChromeView,
    endpoint_id: &ClientEndpointId,
) -> Vec<Slot> {
    let endpoint = frontend.runtime.shell.endpoint(endpoint_id);
    let section_of = |id: &str| {
        endpoint
            .and_then(|endpoint| endpoint.cache.displayed_workspace_facts(id))
            .and_then(|facts| facts.section)
    };
    let mut slots = view
        .hits
        .iter()
        .filter(|hit| view.workspace_body.intersects(hit.rect))
        .filter_map(|hit| match &hit.target {
            ChromeTarget::WorkspaceSection(id, section) if id == endpoint_id => {
                Some(Slot::Header {
                    rect: hit.rect,
                    section: *section,
                })
            }
            ChromeTarget::Workspace(key) if &key.endpoint == endpoint_id => Some(Slot::Card {
                rect: hit.rect,
                id: key.id.clone(),
                section: section_of(&key.id),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    slots.sort_by_key(|slot| slot.rect().y);
    slots
}

fn order(frontend: &ClientFrontend, endpoint_id: &ClientEndpointId) -> Vec<String> {
    frontend
        .runtime
        .shell
        .endpoint(endpoint_id)
        .and_then(|endpoint| endpoint.cache.snapshot())
        .map(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .map(|workspace| workspace.workspace_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Drag and release while a space press is held. Returns true when consumed.
pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    view: &ChromeView,
    mouse: crossterm::event::MouseEvent,
) -> io::Result<bool> {
    let Some(drag) = frontend.workspace_drag.as_ref() else {
        return Ok(false);
    };
    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => {
            let moved = mouse
                .column
                .abs_diff(drag.start.0)
                .max(mouse.row.abs_diff(drag.start.1));
            let dragging = drag.dragging || moved >= DRAG_THRESHOLD;
            let drop = dragging
                .then(|| {
                    let endpoint = drag.key.endpoint.clone();
                    drop_at(
                        &slots(frontend, view, &endpoint),
                        &order(frontend, &endpoint),
                        view.workspace_body,
                        mouse.row,
                    )
                })
                .flatten();
            if let Some(drag) = frontend.workspace_drag.as_mut() {
                drag.dragging = dragging;
                drag.drop = drop;
            }
            frontend.force_redraw = true;
            Ok(true)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(drag) = frontend.workspace_drag.take() else {
                return Ok(false);
            };
            frontend.force_redraw = true;
            if !drag.dragging {
                return Ok(true);
            }
            if let Some(drop) = drag.drop {
                // The press showed the space; its activation may still be in
                // flight, so the drop waits for the endpoint's input lease.
                frontend.pending_workspace_drop = Some((drag.key, drop, std::time::Instant::now()));
                observe(frontend)?;
            }
            Ok(true)
        }
        MouseEventKind::Moved => Ok(false),
        _ => {
            frontend.workspace_drag = None;
            Ok(false)
        }
    }
}

/// The methods a drop issues, in order: section first, then the position.
pub(crate) fn drop_methods(
    workspace_id: &str,
    current_section: Option<WorkspaceSection>,
    order: &[String],
    drop: &Drop,
) -> Vec<crate::api::schema::Method> {
    use crate::api::schema as api;
    let mut methods = Vec::new();
    let section_changes = drop.section.is_some() && drop.section != current_section;
    if let Some(section) = drop.section.filter(|_| section_changes) {
        methods.push(api::Method::WorkspaceSetSection(
            api::WorkspaceSetSectionParams {
                workspace_id: workspace_id.to_owned(),
                section,
            },
        ));
    }
    let source = order.iter().position(|id| id == workspace_id);
    if let (Some(source), Some(insert_index)) = (source, drop.insert_index) {
        let unchanged = insert_index == source || insert_index == source + 1;
        if !unchanged {
            methods.push(api::Method::WorkspaceMove(api::WorkspaceMoveParams {
                workspace_id: workspace_id.to_owned(),
                insert_index,
            }));
        }
    }
    methods
}

/// How long a drop may wait for the dragged space's endpoint to take input.
const DROP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Issue a waiting drop once its endpoint is shown and holds the input lease.
pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some((key, _, since)) = frontend.pending_workspace_drop.as_ref() else {
        return Ok(());
    };
    let ready = key.endpoint == frontend.runtime.shell.active_endpoint_id
        && frontend.runtime.input_lease_current();
    if !ready {
        if since.elapsed() > DROP_WAIT {
            frontend.pending_workspace_drop = None;
            frontend.notice =
                Some("the space could not be moved: its machine is not ready".to_owned());
        }
        return Ok(());
    }
    if let Some((key, drop, _)) = frontend.pending_workspace_drop.take() {
        apply(frontend, &key, drop)?;
    }
    Ok(())
}

fn apply(frontend: &mut ClientFrontend, key: &ResourceKey, drop: Drop) -> io::Result<()> {
    let current = frontend
        .runtime
        .shell
        .endpoint(&key.endpoint)
        .and_then(|endpoint| endpoint.cache.displayed_workspace_facts(&key.id))
        .and_then(|facts| facts.section);
    let order = order(frontend, &key.endpoint);
    for method in drop_methods(&key.id, current, &order, &drop) {
        match frontend.runtime.issue_method(method) {
            Ok(update) => {
                frontend.update(update)?;
            }
            Err(error) => {
                frontend.notice = Some(error);
                break;
            }
        }
    }
    Ok(())
}

/// The insertion line across the spaces list while a drag is over a slot.
pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, view: &ChromeView) {
    let Some(row) = frontend
        .workspace_drag
        .as_ref()
        .filter(|drag| drag.dragging)
        .and_then(|drag| drag.drop.as_ref())
        .and_then(|drop| drop.indicator_row)
    else {
        return;
    };
    let body = view.workspace_body;
    if row < body.y || row >= body.bottom() {
        return;
    }
    let style = ratatui::style::Style::default().fg(frontend.chrome.settings.palette.accent);
    let buf = frame.buffer_mut();
    for x in body.x..body.right() {
        buf[(x, row)].set_symbol("─");
        buf[(x, row)].set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(y: u16, height: u16, id: &str, section: Option<WorkspaceSection>) -> Slot {
        Slot::Card {
            rect: Rect::new(0, y, 30, height),
            id: id.into(),
            section,
        }
    }

    fn header(y: u16, section: WorkspaceSection) -> Slot {
        Slot::Header {
            rect: Rect::new(0, y, 30, 1),
            section,
        }
    }

    #[test]
    fn drop_reorders_within_a_section_and_moves_between_sections() {
        let [first, second, ..] = WorkspaceSection::ALL;
        let body = Rect::new(0, 2, 30, 20);
        let order = ["s1", "s2", "s3"].map(String::from);
        let slots = vec![
            header(2, first),
            card(4, 2, "s1", Some(first)),
            card(7, 2, "s2", Some(first)),
            header(10, second),
            card(12, 2, "s3", Some(second)),
        ];
        // Upper half of the second card: before it.
        assert_eq!(
            drop_at(&slots, &order, body, 7),
            Some(Drop {
                section: Some(first),
                insert_index: Some(1),
                indicator_row: Some(6),
            })
        );
        // Lower half of the last card of a section: after it, before the next section's first.
        assert_eq!(
            drop_at(&slots, &order, body, 8).map(|drop| drop.insert_index),
            Some(Some(2))
        );
        // Another section's header: into that section, before its first card.
        assert_eq!(
            drop_at(&slots, &order, body, 10),
            Some(Drop {
                section: Some(second),
                insert_index: Some(2),
                indicator_row: Some(12),
            })
        );
        // Below the last card: after it.
        assert_eq!(
            drop_at(&slots, &order, body, 18).map(|drop| drop.insert_index),
            Some(Some(3))
        );
        // Outside the spaces list: nothing.
        assert_eq!(drop_at(&slots, &order, body, 1), None);
    }

    #[test]
    fn slim_single_row_cards_drop_before_the_card_under_the_pointer() {
        let body = Rect::new(0, 0, 30, 10);
        let order = ["a", "b"].map(String::from);
        let slots = vec![card(0, 1, "a", None), card(1, 1, "b", None)];
        assert_eq!(
            drop_at(&slots, &order, body, 1).map(|drop| drop.insert_index),
            Some(Some(1))
        );
        assert_eq!(
            drop_at(&slots, &order, body, 2).map(|drop| drop.insert_index),
            Some(Some(2))
        );
    }

    #[test]
    fn drop_methods_set_the_section_then_move_and_skip_no_ops() {
        use crate::api::schema as api;
        let [first, second, ..] = WorkspaceSection::ALL;
        let order = ["s1", "s2", "s3"].map(String::from);
        let drop = |section, insert_index| Drop {
            section,
            insert_index,
            indicator_row: None,
        };
        assert_eq!(
            drop_methods("s1", Some(first), &order, &drop(Some(second), Some(3))),
            vec![
                api::Method::WorkspaceSetSection(api::WorkspaceSetSectionParams {
                    workspace_id: "s1".into(),
                    section: second,
                }),
                api::Method::WorkspaceMove(api::WorkspaceMoveParams {
                    workspace_id: "s1".into(),
                    insert_index: 3,
                }),
            ]
        );
        // Dropping right where it already is changes nothing.
        assert!(drop_methods("s2", Some(first), &order, &drop(Some(first), Some(1))).is_empty());
        assert!(drop_methods("s2", Some(first), &order, &drop(Some(first), Some(2))).is_empty());
        // Same section, new place: a move only.
        assert_eq!(
            drop_methods("s3", Some(first), &order, &drop(Some(first), Some(0))).len(),
            1
        );
    }
}
