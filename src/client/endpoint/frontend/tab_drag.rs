//! Tab bar drag in the endpoint client: reorder a space's tabs by dragging a
//! tab chip, as the in-process tab bar always allowed. The order stays
//! server-owned; the drop issues the same `tab.move` the legacy TUI used.
use super::super::chrome::{ChromeTarget, ChromeView};
use super::super::ResourceKey;
use super::*;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Rect;

/// Same threshold as the legacy tab bar: one cell of movement starts a drag.
const DRAG_THRESHOLD: u16 = 1;

pub(super) struct TabDrag {
    key: ResourceKey,
    start: (u16, u16),
    dragging: bool,
    drop: Option<Drop>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Drop {
    /// Tab index in the space to insert before, as `tab.move` takes it.
    pub(crate) insert_index: usize,
    /// Column of the insertion line in the tab bar.
    pub(crate) indicator_x: u16,
}

/// Where a drop at `column` lands among the visible tab chips (drawing order)
/// given the space's full tab order. The left half of a chip inserts before
/// it, the right half after it.
pub(crate) fn drop_at(chips: &[(Rect, String)], order: &[String], column: u16) -> Option<Drop> {
    let position = |id: &str| order.iter().position(|candidate| candidate == id);
    let (last_rect, last_id) = chips.last()?;
    for (rect, id) in chips {
        if column < rect.x + rect.width.div_ceil(2) {
            return Some(Drop {
                insert_index: position(id)?,
                indicator_x: rect.x.saturating_sub(1),
            });
        }
    }
    Some(Drop {
        insert_index: position(last_id)? + 1,
        indicator_x: last_rect.right(),
    })
}

/// The `tab.move` a drop issues, or nothing when the tab would stay put.
pub(crate) fn drop_method(
    tab_id: &str,
    order: &[String],
    drop: Drop,
) -> Option<crate::api::schema::Method> {
    let source = order.iter().position(|id| id == tab_id)?;
    let unchanged = drop.insert_index == source || drop.insert_index == source + 1;
    (!unchanged).then(|| {
        crate::api::schema::Method::TabMove(crate::api::schema::TabMoveParams {
            tab_id: tab_id.to_owned(),
            insert_index: drop.insert_index,
        })
    })
}

/// A left press on a tab chip: remember it so a move turns it into a drag.
pub(super) fn press(frontend: &mut ClientFrontend, key: &ResourceKey, column: u16, row: u16) {
    frontend.tab_drag = Some(TabDrag {
        key: key.clone(),
        start: (column, row),
        dragging: false,
        drop: None,
    });
}

fn chips(view: &ChromeView, endpoint_id: &ClientEndpointId) -> Vec<(Rect, String)> {
    let mut chips = view
        .hits
        .iter()
        .filter_map(|hit| match &hit.target {
            ChromeTarget::Tab(key) if &key.endpoint == endpoint_id => {
                Some((hit.rect, key.id.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    chips.sort_by_key(|(rect, _)| rect.x);
    chips
}

/// The dragged tab's space's tabs, in server order.
fn order(frontend: &ClientFrontend, key: &ResourceKey) -> Vec<String> {
    let Some(snapshot) = frontend
        .runtime
        .shell
        .endpoint(&key.endpoint)
        .and_then(|endpoint| endpoint.cache.snapshot())
    else {
        return Vec::new();
    };
    let Some(workspace) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == key.id)
        .map(|tab| tab.workspace_id.clone())
    else {
        return Vec::new();
    };
    snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace)
        .map(|tab| tab.tab_id.clone())
        .collect()
}

/// Drag and release while a tab press is held. Returns true when consumed.
pub(super) fn mouse(
    frontend: &mut ClientFrontend,
    view: &ChromeView,
    mouse: crossterm::event::MouseEvent,
) -> io::Result<bool> {
    let Some(drag) = frontend.tab_drag.as_ref() else {
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
                    drop_at(
                        &chips(view, &drag.key.endpoint),
                        &order(frontend, &drag.key),
                        mouse.column,
                    )
                })
                .flatten();
            if let Some(drag) = frontend.tab_drag.as_mut() {
                drag.dragging = dragging;
                drag.drop = drop;
            }
            frontend.force_redraw = true;
            Ok(true)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(drag) = frontend.tab_drag.take() else {
                return Ok(false);
            };
            frontend.force_redraw = true;
            if !drag.dragging {
                return Ok(true);
            }
            if let Some(drop) = drag.drop {
                frontend.pending_tab_drop = Some((drag.key, drop, std::time::Instant::now()));
                observe(frontend)?;
            }
            Ok(true)
        }
        MouseEventKind::Moved => Ok(false),
        _ => {
            frontend.tab_drag = None;
            Ok(false)
        }
    }
}

/// How long a drop may wait for the dragged tab's endpoint to take input.
const DROP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Issue a waiting drop once its endpoint is shown and holds the input lease.
pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some((key, _, since)) = frontend.pending_tab_drop.as_ref() else {
        return Ok(());
    };
    let ready = key.endpoint == frontend.runtime.shell.active_endpoint_id
        && frontend.runtime.input_lease_current();
    if !ready {
        if since.elapsed() > DROP_WAIT {
            frontend.pending_tab_drop = None;
            frontend.notice =
                Some("the tab could not be moved: its machine is not ready".to_owned());
        }
        return Ok(());
    }
    if let Some((key, drop, _)) = frontend.pending_tab_drop.take() {
        let order = order(frontend, &key);
        if let Some(method) = drop_method(&key.id, &order, drop) {
            match frontend.runtime.issue_method(method) {
                Ok(update) => {
                    frontend.update(update)?;
                }
                Err(error) => frontend.notice = Some(error),
            }
        }
    }
    Ok(())
}

/// The insertion line in the tab bar while a drag is over the tabs.
pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, view: &ChromeView) {
    let Some(drop) = frontend
        .tab_drag
        .as_ref()
        .filter(|drag| drag.dragging)
        .and_then(|drag| drag.drop)
    else {
        return;
    };
    let Some(row) = view
        .hits
        .iter()
        .find(|hit| matches!(hit.target, ChromeTarget::Tab(_)))
        .map(|hit| hit.rect.y)
    else {
        return;
    };
    let area = frame.area();
    if drop.indicator_x >= area.right() || row >= area.bottom() {
        return;
    }
    let style = ratatui::style::Style::default().fg(frontend.chrome.settings.palette.accent);
    let buf = frame.buffer_mut();
    buf[(drop.indicator_x, row)].set_symbol("│");
    buf[(drop.indicator_x, row)].set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(x: u16, width: u16, id: &str) -> (Rect, String) {
        (Rect::new(x, 0, width, 1), id.into())
    }

    #[test]
    fn drop_lands_before_or_after_the_chip_under_the_pointer() {
        let order = ["t1", "t2", "t3"].map(String::from);
        let chips = vec![chip(0, 8, "t1"), chip(9, 8, "t2"), chip(18, 8, "t3")];
        // Left half of the second chip: before it.
        assert_eq!(
            drop_at(&chips, &order, 10),
            Some(Drop {
                insert_index: 1,
                indicator_x: 8
            })
        );
        // Right half of the second chip: before the third.
        assert_eq!(drop_at(&chips, &order, 14).map(|d| d.insert_index), Some(2));
        // Past the last chip: after it.
        assert_eq!(
            drop_at(&chips, &order, 40),
            Some(Drop {
                insert_index: 3,
                indicator_x: 26
            })
        );
        assert_eq!(drop_at(&[], &order, 3), None);
    }

    #[test]
    fn drop_method_moves_and_skips_dropping_in_place() {
        let order = ["t1", "t2", "t3"].map(String::from);
        let drop = |insert_index| Drop {
            insert_index,
            indicator_x: 0,
        };
        assert_eq!(
            drop_method("t1", &order, drop(3)),
            Some(crate::api::schema::Method::TabMove(
                crate::api::schema::TabMoveParams {
                    tab_id: "t1".into(),
                    insert_index: 3,
                }
            ))
        );
        assert_eq!(drop_method("t2", &order, drop(1)), None);
        assert_eq!(drop_method("t2", &order, drop(2)), None);
        assert!(drop_method("t3", &order, drop(0)).is_some());
        assert_eq!(drop_method("t9", &order, drop(0)), None);
    }
}
