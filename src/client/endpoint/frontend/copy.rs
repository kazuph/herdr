//! Copy presentation is client-owned; retained text stays on its qualified endpoint.
use super::*;
use crate::api::schema as api;
use crate::input::TerminalKey;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) endpoint: ClientEndpointId,
    pub(super) generation: u64,
    pub(super) boot: String,
    pub(super) pane: String,
}

impl Owner {
    pub(super) fn exists(&self, frontend: &ClientFrontend) -> bool {
        frontend
            .runtime
            .shell
            .endpoint(&self.endpoint)
            .and_then(|endpoint| endpoint.cache.live_snapshot(self.generation))
            .is_some_and(|snapshot| {
                snapshot.boot_id == self.boot
                    && snapshot.panes.iter().any(|pane| pane.pane_id == self.pane)
            })
    }
    pub(super) fn pane<'a>(
        &self,
        frontend: &'a ClientFrontend,
    ) -> Option<&'a wire::PaneSurfacePane> {
        self.visible_pane(frontend).filter(|pane| pane.focused)
    }
    pub(super) fn visible_pane<'a>(
        &self,
        frontend: &'a ClientFrontend,
    ) -> Option<&'a wire::PaneSurfacePane> {
        if frontend.runtime.shell.active_endpoint_id != self.endpoint || !self.exists(frontend) {
            return None;
        }
        frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()?
            .panes
            .iter()
            .find(|pane| pane.pane_id == self.pane)
    }
}

enum Operation {
    Motion,
    Search,
    Copy,
    Scroll { offset: u64, exit: bool },
}

pub(super) struct CopyMode {
    owner: Owner,
    cursor: api::PaneTextPoint,
    anchor: Option<(api::PaneTextPoint, bool)>,
    entry_offset: u64,
    revision: u64,
    geometry: (u16, u16),
    prompt: Option<(api::PaneCopySearchDirection, String)>,
    query: String,
    direction: Option<api::PaneCopySearchDirection>,
    matches: Vec<api::PaneTextRange>,
    current: Option<usize>,
    current_global: Option<usize>,
    total: usize,
    pending: Option<(String, Operation)>,
    waiting_scroll: Option<(u64, bool)>,
    keys: VecDeque<TerminalKey>,
    exit_requested: bool,
}

pub(super) struct DeferredAction {
    owner: Owner,
    key: TerminalKey,
    prefixed: bool,
}

pub(super) fn before_action(
    frontend: &mut ClientFrontend,
    key: TerminalKey,
    prefixed: bool,
    action: crate::app::NavigateAction,
) -> io::Result<bool> {
    if !active(frontend) || crate::app::copy_mode_survives_prefix_action(action) {
        return Ok(false);
    }
    before_custom(frontend, key, prefixed)
}

pub(super) fn before_custom(
    frontend: &mut ClientFrontend,
    key: TerminalKey,
    prefixed: bool,
) -> io::Result<bool> {
    if frontend.copy_action.is_some() {
        return Ok(true);
    }
    let Some(mode) = frontend.copy_mode.as_mut() else {
        return Ok(false);
    };
    frontend.copy_action = Some(DeferredAction {
        owner: mode.owner.clone(),
        key,
        prefixed,
    });
    mode.anchor = None;
    mode.keys.clear();
    mode.exit_requested = true;
    observe(frontend)?;
    resume_action(frontend)?;
    Ok(true)
}

pub(super) fn resume_action(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(action) = frontend.copy_action.as_ref() else {
        return Ok(());
    };
    if !action.owner.exists(frontend) {
        frontend.copy_action = None;
        return Ok(());
    }
    if frontend.copy_mode.is_some() {
        return Ok(());
    }
    let action = frontend.copy_action.take();
    if let Some(action) = action {
        if action.owner.pane(frontend).is_some() && frontend.runtime.input_lease_current() {
            frontend.prefix = action.prefixed;
            input::shell_key(frontend, action.key)?;
        }
    }
    Ok(())
}

fn issue(
    frontend: &mut ClientFrontend,
    method: api::Method,
    operation: Operation,
) -> io::Result<()> {
    match frontend.runtime.issue_method_with_id(method) {
        Ok((id, update)) => {
            if let Some(mode) = frontend.copy_mode.as_mut() {
                mode.pending = Some((id, operation));
            }
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn enter(frontend: &mut ClientFrontend) {
    if !frontend.runtime.input_lease_current() {
        return;
    }
    if active(frontend) {
        return;
    }
    let id = &frontend.runtime.shell.active_endpoint_id;
    let Some(endpoint) = frontend.runtime.shell.endpoint(id) else {
        return;
    };
    let Some(generation) = endpoint.generation else {
        return;
    };
    let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
        return;
    };
    let Some(surface) = frontend.runtime.shell.pane_surface.as_ref() else {
        return;
    };
    let Some(pane) = surface.panes.iter().find(|pane| pane.focused) else {
        return;
    };
    let Some(scroll) = pane.scroll else {
        return;
    };
    let inner = pane.inner_rect;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top = scroll
        .max_offset_from_bottom
        .saturating_sub(scroll.offset_from_bottom);
    let cursor = surface
        .frame
        .cursor
        .as_ref()
        .filter(|cursor| {
            cursor.visible
                && cursor.x >= inner.x
                && cursor.x < inner.x.saturating_add(inner.width)
                && cursor.y >= inner.y
                && cursor.y < inner.y.saturating_add(inner.height)
        })
        .map(|cursor| api::PaneTextPoint {
            row: top
                .saturating_add(u64::from(cursor.y - inner.y))
                .min(u64::from(u32::MAX)) as u32,
            col: cursor.x - inner.x,
        })
        .unwrap_or(api::PaneTextPoint {
            row: top
                .saturating_add(u64::from(inner.height - 1))
                .min(u64::from(u32::MAX)) as u32,
            col: 0,
        });
    frontend.copy_mode = Some(CopyMode {
        owner: Owner {
            endpoint: id.clone(),
            generation,
            boot: snapshot.boot_id.clone(),
            pane: pane.pane_id.clone(),
        },
        cursor,
        anchor: None,
        entry_offset: scroll.offset_from_bottom,
        revision: pane.content_revision,
        geometry: (inner.width, inner.height),
        prompt: None,
        query: String::new(),
        direction: None,
        matches: Vec::new(),
        current: None,
        current_global: None,
        total: 0,
        pending: None,
        waiting_scroll: None,
        keys: VecDeque::new(),
        exit_requested: false,
    });
}

fn scroll(frontend: &mut ClientFrontend, offset: u64, exit: bool) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    let Some(pane) = mode.owner.pane(frontend) else {
        return Ok(());
    };
    let Some(metrics) = pane.scroll else {
        return Ok(());
    };
    let offset = offset.min(metrics.max_offset_from_bottom);
    if offset == metrics.offset_from_bottom {
        if exit {
            frontend.copy_mode = None;
        }
        return Ok(());
    }
    issue(
        frontend,
        api::Method::PaneScroll(api::PaneScrollParams {
            pane_id: mode.owner.pane.clone(),
            offset_from_bottom: offset,
        }),
        Operation::Scroll { offset, exit },
    )
}

fn reveal(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    let Some(pane) = mode.owner.pane(frontend) else {
        return Ok(());
    };
    let Some(metrics) = pane.scroll else {
        return Ok(());
    };
    let top = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let bottom = top.saturating_add(u64::from(pane.inner_rect.height.saturating_sub(1)));
    let row = u64::from(mode.cursor.row);
    let new_top = if row < top {
        row
    } else if row > bottom {
        row.saturating_sub(u64::from(pane.inner_rect.height.saturating_sub(1)))
    } else {
        top
    };
    scroll(
        frontend,
        metrics.max_offset_from_bottom.saturating_sub(new_top),
        false,
    )
}

fn exit(frontend: &mut ClientFrontend, copy: bool) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    if copy {
        if let Some((anchor, linewise)) = mode.anchor {
            let (anchor, cursor) = if linewise {
                (
                    api::PaneTextPoint {
                        row: anchor.row.min(mode.cursor.row),
                        col: 0,
                    },
                    api::PaneTextPoint {
                        row: anchor.row.max(mode.cursor.row),
                        col: mode.geometry.0.saturating_sub(1),
                    },
                )
            } else {
                (anchor, mode.cursor)
            };
            return issue(
                frontend,
                api::Method::PaneSelectionRead(api::PaneSelectionReadParams {
                    pane_id: mode.owner.pane.clone(),
                    anchor,
                    cursor,
                    content_revision: Some(mode.revision),
                }),
                Operation::Copy,
            );
        }
    }
    scroll(frontend, mode.entry_offset, true)
}

fn motion(frontend: &mut ClientFrontend, motion: api::PaneCopyMotion) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    issue(
        frontend,
        api::Method::PaneCopyMotion(api::PaneCopyMotionParams {
            pane_id: mode.owner.pane.clone(),
            cursor: mode.cursor,
            motion,
            content_revision: Some(mode.revision),
        }),
        Operation::Motion,
    )
}

fn search(frontend: &mut ClientFrontend, reverse: bool, repeat: bool) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    if mode.query.is_empty() {
        return Ok(());
    }
    let Some(mut direction) = mode.direction else {
        return Ok(());
    };
    if reverse {
        direction = match direction {
            api::PaneCopySearchDirection::Forward => api::PaneCopySearchDirection::Backward,
            api::PaneCopySearchDirection::Backward => api::PaneCopySearchDirection::Forward,
        };
    }
    issue(
        frontend,
        api::Method::PaneCopySearch(api::PaneCopySearchParams {
            pane_id: mode.owner.pane.clone(),
            cursor: mode.cursor,
            content_revision: mode.revision,
            query: mode.query.clone(),
            direction,
            previous: if repeat {
                mode.current
                    .and_then(|index| mode.matches.get(index).copied())
            } else {
                None
            },
        }),
        Operation::Search,
    )
}

fn move_cursor(frontend: &mut ClientFrontend, rows: i64, cols: i32) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    let Some(pane) = mode.owner.pane(frontend) else {
        return Ok(());
    };
    let maximum = pane
        .scroll
        .map_or(0, |metrics| metrics.max_offset_from_bottom)
        .saturating_add(u64::from(pane.inner_rect.height.saturating_sub(1)))
        .min(u64::from(u32::MAX));
    let width = pane.inner_rect.width;
    if let Some(mode) = frontend.copy_mode.as_mut() {
        mode.cursor.row = (i64::from(mode.cursor.row) + rows).clamp(0, maximum as i64) as u32;
        mode.cursor.col =
            (i32::from(mode.cursor.col) + cols).clamp(0, i32::from(width.saturating_sub(1))) as u16;
    }
    reveal(frontend)
}

fn page(frontend: &mut ClientFrontend, direction: i64, half: bool) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    let Some(pane) = mode.owner.pane(frontend) else {
        return Ok(());
    };
    let Some(metrics) = pane.scroll else {
        return Ok(());
    };
    let lines = crate::copy_mode::copy_mode_page_lines(pane.inner_rect.height, half) as u64;
    let maximum = metrics
        .max_offset_from_bottom
        .saturating_add(u64::from(pane.inner_rect.height.saturating_sub(1)))
        .min(u64::from(u32::MAX));
    let row =
        (i64::from(mode.cursor.row) + direction * (lines as i64)).clamp(0, maximum as i64) as u32;
    let offset = if direction < 0 {
        metrics
            .offset_from_bottom
            .saturating_add(lines)
            .min(metrics.max_offset_from_bottom)
    } else {
        metrics.offset_from_bottom.saturating_sub(lines)
    };
    if let Some(mode) = frontend.copy_mode.as_mut() {
        mode.cursor.row = row;
    }
    scroll(frontend, offset, false)
}

pub(super) fn key(frontend: &mut ClientFrontend, key: TerminalKey) -> io::Result<bool> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(false);
    };
    if mode.owner.pane(frontend).is_none() {
        return Ok(false);
    }
    if key.kind == KeyEventKind::Release {
        return Ok(true);
    }
    if crate::config::terminal_key_matches_combo(key, frontend.keybinds.prefix) {
        return Ok(false);
    }
    if mode.pending.is_some()
        || mode.waiting_scroll.is_some()
        || !frontend.runtime.input_lease_current()
    {
        if let Some(mode) = frontend.copy_mode.as_mut() {
            mode.keys.push_back(key);
        }
        return Ok(true);
    }
    apply_key(frontend, key)?;
    Ok(true)
}

fn apply_key(frontend: &mut ClientFrontend, key: TerminalKey) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_mut() else {
        return Ok(());
    };
    if let Some((direction, query)) = mode.prompt.as_mut() {
        match key.code {
            KeyCode::Esc => mode.prompt = None,
            KeyCode::Enter => {
                mode.direction = Some(*direction);
                mode.query = std::mem::take(query);
                mode.prompt = None;
                search(frontend, false, false)?;
            }
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => query.clear(),
            _ => {
                if let Some(ch) = crate::copy_mode::copy_mode_command_char(key) {
                    query.push(ch);
                }
            }
        }
        return Ok(());
    }
    let ch = crate::copy_mode::copy_mode_command_char(key);
    match (key.code, ch) {
        (KeyCode::Esc, _) if mode.anchor.is_some() || !mode.query.is_empty() => {
            mode.anchor = None;
            mode.query.clear();
            mode.matches.clear();
            mode.direction = None;
            mode.current = None;
            mode.current_global = None;
            mode.total = 0;
        }
        (KeyCode::Esc, _) | (_, Some('q')) => exit(frontend, false)?,
        (KeyCode::Enter, _) | (_, Some('y')) => exit(frontend, true)?,
        (_, Some('v' | ' ')) => mode.anchor = Some((mode.cursor, false)),
        (_, Some('V')) => mode.anchor = Some((mode.cursor, true)),
        (KeyCode::Left, _) | (_, Some('h')) => move_cursor(frontend, 0, -1)?,
        (KeyCode::Right, _) | (_, Some('l')) => move_cursor(frontend, 0, 1)?,
        (KeyCode::Down, _) | (_, Some('j')) => move_cursor(frontend, 1, 0)?,
        (KeyCode::Up, _) | (_, Some('k')) => move_cursor(frontend, -1, 0)?,
        (_, Some('g')) => {
            mode.cursor.row = 0;
            reveal(frontend)?;
        }
        (_, Some('G')) => move_cursor(frontend, i64::from(u32::MAX), 0)?,
        (KeyCode::Home, _) | (_, Some('0')) => mode.cursor.col = 0,
        (KeyCode::End, _) | (_, Some('$')) => motion(frontend, api::PaneCopyMotion::LineEnd)?,
        (_, Some('^')) => motion(frontend, api::PaneCopyMotion::FirstNonBlank)?,
        (_, Some('w')) => motion(frontend, api::PaneCopyMotion::NextWordStart)?,
        (_, Some('b')) => motion(frontend, api::PaneCopyMotion::PreviousWordStart)?,
        (_, Some('e')) => motion(frontend, api::PaneCopyMotion::NextWordEnd)?,
        (_, Some('{')) => motion(frontend, api::PaneCopyMotion::PreviousParagraph)?,
        (_, Some('}')) => motion(frontend, api::PaneCopyMotion::NextParagraph)?,
        (_, Some('/')) => {
            mode.prompt = Some((api::PaneCopySearchDirection::Forward, String::new()))
        }
        (_, Some('?')) => {
            mode.prompt = Some((api::PaneCopySearchDirection::Backward, String::new()))
        }
        (_, Some('n')) => search(frontend, false, true)?,
        (_, Some('N')) => search(frontend, true, true)?,
        _ => {
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            let (direction, half) = match key.code {
                KeyCode::PageUp => (-1, false),
                KeyCode::PageDown => (1, false),
                KeyCode::Char('b') if control => (-1, false),
                KeyCode::Char('f') if control => (1, false),
                KeyCode::Char('u') if control => (-1, true),
                KeyCode::Char('d') if control => (1, true),
                _ => return Ok(()),
            };
            page(frontend, direction, half)?;
        }
    }
    Ok(())
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    let Some((id, _)) = mode.pending.as_ref() else {
        return Ok(());
    };
    if result.request_id != *id
        || result.endpoint_id != mode.owner.endpoint
        || result.generation != mode.owner.generation
        || result.boot_id != mode.owner.boot
    {
        return Ok(());
    }
    let current = mode
        .owner
        .pane(frontend)
        .is_some_and(|pane| pane.content_revision == mode.revision);
    let Some((_, operation)) = frontend
        .copy_mode
        .as_mut()
        .and_then(|mode| mode.pending.take())
    else {
        return Ok(());
    };
    if !current {
        return Ok(());
    }
    let Ok(value) = &result.result else {
        return Ok(());
    };
    let Ok(response) = serde_json::from_value::<api::ResponseResult>(value.clone()) else {
        return Ok(());
    };
    let Some(mode) = frontend.copy_mode.as_mut() else {
        return Ok(());
    };
    match (operation, response) {
        (
            Operation::Motion,
            api::ResponseResult::PaneCopyMotion {
                pane_id,
                cursor,
                content_revision,
            },
        ) if pane_id == mode.owner.pane && content_revision == mode.revision => {
            mode.cursor = cursor;
            reveal(frontend)?;
        }
        (
            Operation::Search,
            api::ResponseResult::PaneCopySearch {
                pane_id,
                content_revision,
                matches,
                total,
                current,
                current_global,
            },
        ) if pane_id == mode.owner.pane && content_revision == mode.revision => {
            mode.matches = matches;
            mode.total = total.min(usize::MAX as u64) as usize;
            mode.current = current.and_then(|index| usize::try_from(index).ok());
            mode.current_global = current_global.and_then(|index| usize::try_from(index).ok());
            if let Some(found) = mode.current.and_then(|index| mode.matches.get(index)) {
                mode.cursor = found.start;
            }
            reveal(frontend)?;
        }
        (Operation::Copy, api::ResponseResult::PaneSelection { pane_id, text })
            if pane_id == mode.owner.pane =>
        {
            if !text.is_empty() {
                crate::selection::write_osc52_bytes(text.as_bytes());
            }
            let offset = mode.entry_offset;
            scroll(frontend, offset, true)?;
        }
        (Operation::Scroll { offset, exit }, api::ResponseResult::PaneInfo { pane })
            if pane.pane_id == mode.owner.pane
                && pane
                    .scroll
                    .is_some_and(|metrics| metrics.offset_from_bottom == offset) =>
        {
            mode.waiting_scroll = Some((offset, exit))
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn observe(frontend: &mut ClientFrontend) -> io::Result<()> {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return Ok(());
    };
    if !mode.owner.exists(frontend) {
        frontend.copy_mode = None;
        return Ok(());
    }
    let Some(pane) = mode.owner.pane(frontend).cloned() else {
        if let Some(mode) = frontend.copy_mode.as_mut() {
            mode.keys.clear();
            mode.anchor = None;
        }
        return Ok(());
    };
    if !frontend.runtime.input_lease_current() {
        return Ok(());
    }
    let Some(mode) = frontend.copy_mode.as_mut() else {
        return Ok(());
    };
    if let Some((offset, exit)) = mode.waiting_scroll {
        if pane
            .scroll
            .is_none_or(|scroll| scroll.offset_from_bottom != offset)
        {
            return Ok(());
        }
        mode.waiting_scroll = None;
        if exit {
            frontend.copy_mode = None;
            return Ok(());
        }
    }
    if mode.pending.is_some() {
        return Ok(());
    }
    if mode.exit_requested {
        return exit(frontend, false);
    }
    if mode.revision != pane.content_revision
        || mode.geometry != (pane.inner_rect.width, pane.inner_rect.height)
    {
        mode.revision = pane.content_revision;
        mode.geometry = (pane.inner_rect.width, pane.inner_rect.height);
        mode.matches.clear();
        mode.current = None;
        mode.current_global = None;
        mode.total = 0;
        mode.cursor.col = mode.cursor.col.min(pane.inner_rect.width.saturating_sub(1));
    }
    while let Some(key) = frontend
        .copy_mode
        .as_mut()
        .and_then(|mode| mode.keys.pop_front())
    {
        apply_key(frontend, key)?;
        if frontend
            .copy_mode
            .as_ref()
            .is_none_or(|mode| mode.pending.is_some() || mode.waiting_scroll.is_some())
        {
            break;
        }
    }
    Ok(())
}

pub(super) fn search_active(frontend: &ClientFrontend) -> bool {
    frontend
        .copy_mode
        .as_ref()
        .is_some_and(|mode| mode.prompt.is_some())
}

pub(super) fn active(frontend: &ClientFrontend) -> bool {
    frontend
        .copy_mode
        .as_ref()
        .is_some_and(|mode| mode.owner.pane(frontend).is_some())
}

#[cfg(test)]
pub(super) fn entry_offset_for_test(frontend: &ClientFrontend) -> Option<u64> {
    frontend.copy_mode.as_ref().map(|mode| mode.entry_offset)
}

#[cfg(test)]
pub(super) fn search_idle_for_test(frontend: &ClientFrontend) -> bool {
    frontend.copy_mode.as_ref().is_some_and(|mode| {
        mode.pending.is_none() && mode.waiting_scroll.is_none() && mode.total > 0
    })
}

#[cfg(test)]
pub(super) fn search_position_for_test(frontend: &ClientFrontend) -> Option<(u32, u16, usize)> {
    frontend
        .copy_mode
        .as_ref()
        .map(|mode| (mode.cursor.row, mode.cursor.col, mode.total))
}

pub(super) fn render(
    frontend: &ClientFrontend,
    frame: &mut ratatui::Frame,
    origin: ratatui::layout::Rect,
) {
    let Some(mode) = frontend.copy_mode.as_ref() else {
        return;
    };
    let Some(pane) = mode.owner.pane(frontend) else {
        return;
    };
    let Some(scroll) = pane.scroll else {
        return;
    };
    let inner = pane.inner_rect;
    let top = scroll
        .max_offset_from_bottom
        .saturating_sub(scroll.offset_from_bottom)
        .min(u64::from(u32::MAX)) as u32;
    let palette = &frontend.chrome.settings.palette;
    let selection = mode.anchor.map(|(anchor, linewise)| {
        if linewise {
            crate::selection::TextSelection::line_range(
                (),
                anchor.row,
                mode.cursor.row,
                inner.width.saturating_sub(1),
            )
        } else {
            crate::selection::TextSelection::absolute_range(
                (),
                (anchor.row, anchor.col),
                (mode.cursor.row, mode.cursor.col),
            )
        }
    });
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: scroll.offset_from_bottom.min(usize::MAX as u64) as usize,
        max_offset_from_bottom: scroll.max_offset_from_bottom.min(usize::MAX as u64) as usize,
        viewport_rows: scroll.viewport_rows.min(usize::MAX as u64) as usize,
    };
    let cursor_style = ratatui::style::Style::default()
        .fg(crate::ui::panel_contrast_fg(palette))
        .bg(palette.accent)
        .add_modifier(ratatui::style::Modifier::BOLD);
    for y in 0..inner.height {
        for x in 0..inner.width {
            let row = top.saturating_add(u32::from(y));
            let point = (row, x);
            let mut style = None;
            for (index, found) in mode.matches.iter().enumerate() {
                if mode.revision == pane.content_revision
                    && point >= (found.start.row, found.start.col)
                    && point <= (found.end.row, found.end.col)
                {
                    style = Some(if mode.current == Some(index) {
                        cursor_style
                    } else {
                        ratatui::style::Style::default()
                            .fg(palette.text)
                            .bg(palette.surface1)
                    });
                }
            }
            if selection
                .as_ref()
                .is_some_and(|selection| selection.contains(y, x, Some(metrics)))
            {
                style = Some(crate::ui::automatic_selection_style(
                    palette,
                    frontend.host_theme,
                ));
            }
            if mode.cursor.row == row && mode.cursor.col == x {
                style = Some(cursor_style);
            }
            if let Some(style) = style {
                frame.buffer_mut()[(origin.x + inner.x + x, origin.y + inner.y + y)]
                    .set_style(style);
            }
        }
    }
    let direction = |direction| match direction {
        api::PaneCopySearchDirection::Forward => {
            crate::app::state::CopyModeSearchDirection::Forward
        }
        api::PaneCopySearchDirection::Backward => {
            crate::app::state::CopyModeSearchDirection::Backward
        }
    };
    crate::ui::render_copy_overlay(
        frame,
        frame.area(),
        palette,
        mode.anchor.is_some(),
        mode.prompt
            .as_ref()
            .map(|(kind, query)| (direction(*kind), query.as_str())),
        &mode.query,
        (mode.current_global, mode.total),
    );
}
