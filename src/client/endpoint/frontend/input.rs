use super::super::chrome::{ChromeTarget, ChromeView};
use super::super::{FocusTarget, ResourceKey};
use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};

pub(super) fn handle(
    frontend: &mut ClientFrontend,
    event: super::super::super::ClientLoopEvent,
) -> io::Result<bool> {
    use super::super::super::ClientLoopEvent;
    match event {
        #[cfg(unix)]
        ClientLoopEvent::DirectGraphicsResponse(response) => {
            direct::response(frontend, response)?;
            Ok(true)
        }
        #[cfg(unix)]
        ClientLoopEvent::StdinInput(bytes) => {
            if clipboard_images::input(frontend, &bytes) {
                return Ok(true);
            }
            for event in crate::raw_input::parse_raw_input_bytes_sync(&bytes) {
                dispatch(frontend, event, None)?;
            }
            Ok(true)
        }
        #[cfg(unix)]
        ClientLoopEvent::PixelMouse(bytes, geometry) => {
            if let Some((x, y)) = crate::input::mouse::parse_report(&bytes) {
                if let Some((column, row)) = geometry.cell(x, y) {
                    if let Some(bytes) = crate::input::mouse::report_at_cell(&bytes, column, row) {
                        for event in crate::raw_input::parse_raw_input_bytes_sync(&bytes) {
                            dispatch(
                                frontend,
                                event,
                                Some(crate::input::mouse::HostPixels { x, y, geometry }),
                            )?;
                        }
                    }
                }
            }
            Ok(true)
        }
        ClientLoopEvent::Resize(cols, rows, cell_width_px, cell_height_px) => {
            frontend.cols = cols;
            frontend.rows = rows;
            frontend.options.cell_width_px = cell_width_px;
            frontend.options.cell_height_px = cell_height_px;
            frontend.options.pixel_geometry_exact =
                super::super::super::ioctl_cell_size() == Some((cell_width_px, cell_height_px));
            let view = frontend
                .chrome
                .compute_view(&frontend.runtime.shell, cols, rows);
            frontend.options.surface_size = wire::ClientSurfaceSize {
                cols: view.layout.pane_surface.width,
                rows: view.layout.pane_surface.height,
            };
            let update = frontend.runtime.resize(frontend.options, Instant::now());
            frontend.update(update)?;
            frontend.force_redraw = true;
            Ok(true)
        }
        #[cfg(windows)]
        ClientLoopEvent::StdinEvents(events) => {
            for event in events {
                dispatch_windows(frontend, event)?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn dispatch(
    frontend: &mut ClientFrontend,
    event: RawInputEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<()> {
    let result = dispatch_inner(frontend, event, pixels);
    ascii::sync(frontend);
    result
}

fn dispatch_inner(
    frontend: &mut ClientFrontend,
    event: RawInputEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<()> {
    if context::input(frontend, &event)?
        || menu::input(frontend, &event)?
        || notes::input(frontend, &event)
        || settings::input(frontend, &event)?
        || help::input(frontend, &event)
        || resize::input(frontend, &event)?
        || worktrees::input(frontend, &event)?
        || modal::input(frontend, &event)?
        || mobile::input(frontend, &event)?
        || navigator::input(frontend, &event)?
    {
        return Ok(());
    }
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    match event {
        RawInputEvent::Key(key) => {
            frontend.split_drag = None;
            selection::clear(frontend);
            popup_selection::clear(frontend);
            if popup_target(frontend).is_none() && !frontend.prefix && copy::key(frontend, key)? {
                return Ok(());
            }
            if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                if let Some(popup) = popup_target(frontend) {
                    if let Some(event) = key_event(key) {
                        frontend.runtime.popup_input(&popup, vec![event]);
                    }
                    return Ok(());
                }
            }
            if shell_key(frontend, key)? {
                return Ok(());
            }
            if let Some(popup) = popup_target(frontend) {
                if let Some(event) = key_event(key) {
                    frontend.runtime.popup_input(&popup, vec![event]);
                }
                return Ok(());
            }
            if let Some(pane) = focused_pane(frontend) {
                if let Some(event) = key_event(key) {
                    frontend.runtime.input(&pane, vec![event]);
                }
            }
        }
        RawInputEvent::LineFeed => {
            if let Some(popup) = popup_target(frontend) {
                frontend.runtime.popup_input(
                    &popup,
                    vec![wire::ClientPaneInputEvent::TextCommit("\n".into())],
                );
                return Ok(());
            }
            if copy::active(frontend) {
                return Ok(());
            }
            if let Some(pane) = focused_pane(frontend) {
                frontend.runtime.input(
                    &pane,
                    vec![wire::ClientPaneInputEvent::TextCommit("\n".into())],
                );
            }
        }
        RawInputEvent::Paste(text) => {
            selection::clear(frontend);
            if let Some(popup) = popup_target(frontend) {
                frontend
                    .runtime
                    .popup_input(&popup, vec![wire::ClientPaneInputEvent::Paste(text)]);
                return Ok(());
            }
            if copy::active(frontend) {
                return Ok(());
            }
            if let Some(pane) = focused_pane(frontend) {
                frontend
                    .runtime
                    .input(&pane, vec![wire::ClientPaneInputEvent::Paste(text)]);
            }
        }
        RawInputEvent::Mouse(mouse) => {
            mouse_input(frontend, &view, mouse, pixels)?;
        }
        RawInputEvent::HostDefaultColor { kind, color } => {
            frontend.host_theme = frontend.host_theme.with_color(kind, color);
        }
        RawInputEvent::OuterFocusGained | RawInputEvent::OuterFocusLost => {
            let focused = matches!(event, RawInputEvent::OuterFocusGained);
            let update = frontend.runtime.host_focus(focused);
            frontend.update(update)?;
            frontend.force_redraw |= focused;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn popup_target(frontend: &ClientFrontend) -> Option<ResourceKey> {
    let popup = frontend
        .runtime
        .shell
        .pane_surface
        .as_ref()?
        .popup
        .as_ref()?;
    Some(ResourceKey {
        endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
        id: popup.terminal_id.clone(),
    })
}

pub(super) fn focused_pane(frontend: &ClientFrontend) -> Option<ResourceKey> {
    let id = &frontend.runtime.shell.active_endpoint_id;
    let surface = frontend.runtime.shell.pane_surface.as_ref()?;
    let pane = surface.panes.iter().find(|pane| pane.focused)?;
    Some(ResourceKey {
        endpoint: id.clone(),
        id: pane.pane_id.clone(),
    })
}

fn focus_agent(frontend: &mut ClientFrontend, index: usize) -> io::Result<()> {
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let targets = frontend
        .chrome
        .agent_targets(&frontend.runtime.shell, view.detail_body.width);
    if let Some(key) = targets.get(index) {
        frontend
            .chrome
            .ensure_agent_visible(&frontend.runtime.shell, view.detail_body, key);
        let update = frontend.runtime.activate(
            key.endpoint.clone(),
            Some(FocusTarget::Pane(key.id.clone())),
            Instant::now(),
        );
        frontend.update(update)?;
    }
    Ok(())
}

fn mouse_input(
    frontend: &mut ClientFrontend,
    view: &ChromeView,
    mouse: crossterm::event::MouseEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<()> {
    if notification::mouse(frontend, mouse)? {
        return Ok(());
    }
    if popup::mouse(frontend, view.layout.pane_surface, mouse, pixels)? {
        return Ok(());
    }
    if right_click::mouse(frontend, view.layout.pane_surface, mouse, pixels)? {
        return Ok(());
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && frontend.split_drag.is_none()
        && !copy::active(frontend)
    {
        let origin = view.layout.pane_surface;
        let target = frontend
            .runtime
            .shell
            .pane_surface
            .as_ref()
            .and_then(|surface| {
                surface
                    .panes
                    .iter()
                    .find(|pane| {
                        pane.rect.width > 4
                            && pane.rect.height > 2
                            && pane.inner_rect.y > pane.rect.y
                            && mouse.row == origin.y + pane.rect.y
                            && mouse.column >= origin.x + pane.rect.x
                            && mouse.column < origin.x + pane.rect.x + pane.rect.width
                    })
                    .map(|pane| ResourceKey {
                        endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
                        id: pane.pane_id.clone(),
                    })
            });
        if let Some(target) = target {
            return context::zoom_pane(frontend, target);
        }
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let actions = view.pane_actions;
        let position = (mouse.column, mouse.row).into();
        let item = if actions.cycle_layout.contains(position) {
            Some("Cycle pane layout")
        } else if actions.rotate.contains(position) {
            Some("Rotate panes")
        } else if actions.equalize.contains(position) {
            Some("Equalize pane sizes")
        } else {
            None
        };
        if let Some(item) = item {
            if let Some(target) = focused_pane(frontend) {
                return context::pane_action(frontend, target, item);
            }
            return Ok(());
        }
    }
    if resize::mouse(frontend, view.layout.pane_surface, mouse)? {
        return Ok(());
    }
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && view
        .layout
        .sidebar
        .contains((mouse.column, mouse.row).into())
    {
        let down = mouse.kind == MouseEventKind::ScrollDown;
        let (scroll, maximum) = if view
            .workspace_body
            .contains((mouse.column, mouse.row).into())
        {
            (
                &mut frontend.chrome.workspace_scroll,
                view.workspace_max_scroll,
            )
        } else if view.detail_body.contains((mouse.column, mouse.row).into()) {
            let scroll = if !frontend.chrome.settings.sidebar_collapsed
                && frontend.chrome.detail_view == crate::app::state::SidebarDetailView::Jobs
            {
                &mut frontend.chrome.jobs_scroll
            } else {
                &mut frontend.chrome.agent_scroll
            };
            (scroll, view.agent_max_scroll)
        } else {
            return Ok(());
        };
        // Existing fork sidebar wheel advances one list row, unlike terminal scrolling.
        *scroll = if down {
            scroll.saturating_add(1).min(maximum)
        } else {
            scroll.saturating_sub(1)
        };
        frontend.force_redraw = true;
        return Ok(());
    }
    if let Some(target) = frontend.chrome.hit(view, mouse.column, mouse.row) {
        if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            match target {
                ChromeTarget::Tab(key) => {
                    context::open_tab(frontend, key.clone(), mouse.column, mouse.row);
                    return Ok(());
                }
                ChromeTarget::Workspace(key) | ChromeTarget::WorkspaceGroup(key) => {
                    context::open_workspace(frontend, key.clone(), mouse.column, mouse.row);
                    return Ok(());
                }
                _ => {}
            }
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if matches!(
                target,
                ChromeTarget::Workspace(_) | ChromeTarget::Tab(_) | ChromeTarget::Agent(_)
            ) {
                frontend.mobile = None;
                mobile::sync_selection(frontend);
            }
            let update = match target {
                ChromeTarget::WorkspaceGroup(key) => {
                    context::toggle_group(frontend, key);
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::GlobalMenu => {
                    if view.layout.mobile_header.is_empty() {
                        menu::open(frontend);
                    } else {
                        mobile::open(frontend);
                    }
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::NewWorkspace => {
                    modal::action(frontend, crate::app::NavigateAction::NewWorkspace)?;
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::NewWorkspaceInSection(endpoint, section) => {
                    context::new_workspace_in_section(frontend, endpoint.clone(), *section)?;
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::WorkspaceSection(endpoint, section) => {
                    if !frontend
                        .chrome
                        .collapsed_sections
                        .remove(&(endpoint.clone(), *section))
                    {
                        frontend
                            .chrome
                            .collapsed_sections
                            .insert((endpoint.clone(), *section));
                    }
                    frontend.chrome.workspace_scroll = 0;
                    frontend.chrome.agent_scroll = 0;
                    frontend.persist_chrome_preferences();
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::Job(key) => {
                    jobs::activate(frontend, key)?;
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::DetailTab(tab) => {
                    frontend.chrome.detail_view = *tab;
                    frontend.chrome.agent_scroll = 0;
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::Machine(id) => {
                    if !frontend.chrome.collapsed_machines.remove(id) {
                        frontend.chrome.collapsed_machines.insert(id.clone());
                    }
                    RuntimeUpdate {
                        repaint: true,
                        ..RuntimeUpdate::default()
                    }
                }
                ChromeTarget::Workspace(key) => frontend.runtime.activate(
                    key.endpoint.clone(),
                    Some(FocusTarget::Workspace(key.id.clone())),
                    Instant::now(),
                ),
                ChromeTarget::Tab(key) => frontend.runtime.activate(
                    key.endpoint.clone(),
                    Some(FocusTarget::Tab(key.id.clone())),
                    Instant::now(),
                ),
                ChromeTarget::Agent(key) => frontend.runtime.activate(
                    key.endpoint.clone(),
                    Some(FocusTarget::Pane(key.id.clone())),
                    Instant::now(),
                ),
            };
            frontend.update(update)?;
        }
        return Ok(());
    }
    if copy::active(frontend) {
        return Ok(());
    }
    if selection::mouse(frontend, view.layout.pane_surface, mouse)? {
        return Ok(());
    }
    let Some(surface) = frontend.runtime.shell.pane_surface.as_ref() else {
        return Ok(());
    };
    let Some(column) = mouse.column.checked_sub(view.layout.pane_surface.x) else {
        return Ok(());
    };
    let Some(row) = mouse.row.checked_sub(view.layout.pane_surface.y) else {
        return Ok(());
    };
    let Some(pane) = surface.panes.iter().find(|pane| {
        let rect = pane.rect;
        column >= rect.x
            && column < rect.x.saturating_add(rect.width)
            && row >= rect.y
            && row < rect.y.saturating_add(rect.height)
    }) else {
        return Ok(());
    };
    let key = ResourceKey {
        endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
        id: pane.pane_id.clone(),
    };
    if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
        context::open_pane(frontend, key, mouse.column, mouse.row);
        return Ok(());
    }
    if !pane.focused && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let update = frontend.runtime.activate(
            key.endpoint.clone(),
            Some(FocusTarget::Pane(key.id.clone())),
            Instant::now(),
        );
        frontend.update(update)?;
        return Ok(());
    }
    let pane = pane.clone();
    send_pane_mouse(
        frontend,
        view.layout.pane_surface,
        &key,
        &pane,
        mouse,
        pixels,
    )
}

pub(super) fn send_pane_mouse(
    frontend: &mut ClientFrontend,
    origin: ratatui::layout::Rect,
    key: &ResourceKey,
    pane: &wire::PaneSurfacePane,
    mouse: crossterm::event::MouseEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
) -> io::Result<()> {
    let Some(event) = surface_mouse_event(
        MouseSurface {
            origin,
            inner: pane.inner_rect,
            sgr_pixel_mouse: pane.sgr_pixel_mouse,
            pixel_width: pane.pixel_width,
            pixel_height: pane.pixel_height,
        },
        mouse,
        pixels,
        frontend.chrome.settings.mouse_scroll_lines,
    ) else {
        return Ok(());
    };
    frontend.runtime.input(key, vec![event]);
    Ok(())
}

pub(super) struct MouseSurface {
    pub(super) origin: ratatui::layout::Rect,
    pub(super) inner: wire::SurfaceRect,
    pub(super) sgr_pixel_mouse: bool,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
}

pub(super) fn surface_mouse_event(
    surface: MouseSurface,
    mouse: crossterm::event::MouseEvent,
    pixels: Option<crate::input::mouse::HostPixels>,
    lines: usize,
) -> Option<wire::ClientPaneInputEvent> {
    let MouseSurface {
        origin,
        inner,
        sgr_pixel_mouse,
        pixel_width,
        pixel_height,
    } = surface;
    let column = mouse.column.checked_sub(origin.x)?;
    let row = mouse.row.checked_sub(origin.y)?;
    if column < inner.x
        || column >= inner.x.saturating_add(inner.width)
        || row < inner.y
        || row >= inner.y.saturating_add(inner.height)
    {
        return None;
    }
    let column = column - inner.x;
    let row = row - inner.y;
    let mut position = wire::ClientMousePosition::Cell { column, row };
    if sgr_pixel_mouse {
        let global_inner = ratatui::layout::Rect::new(
            origin.x + inner.x,
            origin.y + inner.y,
            inner.width,
            inner.height,
        );
        if let Some(crate::input::mouse::Position::Pixels { x, y }) =
            pixels.and_then(|pixels| pixels.pane_position(global_inner, pixel_width, pixel_height))
        {
            position = wire::ClientMousePosition::Pixels { x, y, column, row };
        }
    }
    let geometry = matches!(position, wire::ClientMousePosition::Pixels { .. }).then_some(
        wire::ClientMouseGeometry {
            cols: inner.width,
            rows: inner.height,
            width_px: pixel_width,
            height_px: pixel_height,
        },
    );
    Some(wire::ClientPaneInputEvent::Mouse {
        kind: mouse_kind(mouse.kind),
        position,
        geometry,
        modifiers: mouse.modifiers.bits(),
        lines: lines.min(u16::MAX as usize) as u16,
    })
}

fn mouse_kind(kind: MouseEventKind) -> wire::ClientMouseKind {
    use wire::ClientMouseKind as K;
    let button = |b| match b {
        MouseButton::Left => wire::ClientMouseButton::Left,
        MouseButton::Right => wire::ClientMouseButton::Right,
        MouseButton::Middle => wire::ClientMouseButton::Middle,
    };
    match kind {
        MouseEventKind::Down(b) => K::Down(button(b)),
        MouseEventKind::Up(b) => K::Up(button(b)),
        MouseEventKind::Drag(b) => K::Drag(button(b)),
        MouseEventKind::Moved => K::Moved,
        MouseEventKind::ScrollUp => K::ScrollUp,
        MouseEventKind::ScrollDown => K::ScrollDown,
        MouseEventKind::ScrollLeft => K::ScrollLeft,
        MouseEventKind::ScrollRight => K::ScrollRight,
    }
}

fn key_event(key: crate::input::TerminalKey) -> Option<wire::ClientPaneInputEvent> {
    use wire::ClientKeyCode as C;
    let code = match key.code {
        KeyCode::Backspace => C::Backspace,
        KeyCode::Enter => C::Enter,
        KeyCode::Left => C::Left,
        KeyCode::Right => C::Right,
        KeyCode::Up => C::Up,
        KeyCode::Down => C::Down,
        KeyCode::Home => C::Home,
        KeyCode::End => C::End,
        KeyCode::PageUp => C::PageUp,
        KeyCode::PageDown => C::PageDown,
        KeyCode::Tab => C::Tab,
        KeyCode::BackTab => C::BackTab,
        KeyCode::Delete => C::Delete,
        KeyCode::Insert => C::Insert,
        KeyCode::Esc => C::Esc,
        KeyCode::Char(c) => C::Char(c),
        KeyCode::F(n) => C::F(n),
        KeyCode::Null => C::Null,
        _ => return None,
    };
    Some(wire::ClientPaneInputEvent::Key {
        code,
        modifiers: key.modifiers.bits(),
        kind: match key.kind {
            KeyEventKind::Press => wire::ClientKeyKind::Press,
            KeyEventKind::Repeat => wire::ClientKeyKind::Repeat,
            KeyEventKind::Release => wire::ClientKeyKind::Release,
        },
        repeat_count: 1,
        shifted_codepoint: key.shifted_codepoint,
        generated_text: None,
        tracks_release: true,
        physical_key_id: None,
        windows_record: None,
    })
}

#[cfg(windows)]
fn dispatch_windows(
    frontend: &mut ClientFrontend,
    event: crate::protocol::ClientInputEvent,
) -> io::Result<()> {
    use crate::protocol::ClientInputEvent as E;
    let event = match event {
        E::Key {
            code,
            modifiers,
            kind,
        } => RawInputEvent::Key(
            crate::input::TerminalKey::new(
                code.to_crossterm(),
                crossterm::event::KeyModifiers::from_bits_truncate(modifiers),
            )
            .with_kind(kind.to_crossterm()),
        ),
        E::Mouse {
            kind,
            column,
            row,
            modifiers,
        } => RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: kind.to_crossterm(),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::from_bits_truncate(modifiers),
        }),
        E::Paste { text } => RawInputEvent::Paste(text),
        E::FocusGained => RawInputEvent::OuterFocusGained,
        E::FocusLost => RawInputEvent::OuterFocusLost,
    };
    dispatch(frontend, event, None)
}

pub(super) fn shell_key(
    frontend: &mut ClientFrontend,
    key: crate::input::TerminalKey,
) -> io::Result<bool> {
    if key.kind == KeyEventKind::Release {
        return Ok(frontend.prefix);
    }
    if crate::config::terminal_key_matches_combo(key, frontend.keybinds.prefix) {
        if frontend.prefix {
            frontend.prefix = false;
            return Ok(false);
        }
        frontend.prefix = true;
        return Ok(true);
    }
    let prefixed = frontend.prefix;
    frontend.prefix = false;
    use crate::app::{navigation_action_for_bindings, BindingDispatch};
    let action = navigation_action_for_bindings(
        &frontend.keybinds.keybinds,
        key,
        if prefixed {
            BindingDispatch::Prefix
        } else {
            BindingDispatch::Direct
        },
    );
    if action.is_none_or(custom::indexed) && custom::key(frontend, key, prefixed)? {
        return Ok(true);
    }
    run_navigation_action(frontend, key, prefixed, action)
}

pub(super) fn run_navigation_action(
    frontend: &mut ClientFrontend,
    key: crate::input::TerminalKey,
    prefixed: bool,
    action: Option<crate::app::NavigateAction>,
) -> io::Result<bool> {
    use crate::app::NavigateAction;
    if let Some(action) = action {
        if action == NavigateAction::OpenNotificationTarget {
            notification::open(frontend)?;
            return Ok(true);
        }
        if action == NavigateAction::EditScrollback {
            if let Some(pane) = focused_pane(frontend) {
                editor::open(frontend, pane)?;
            }
            return Ok(true);
        }
        if action == NavigateAction::OpenNavigator {
            navigator::open(frontend);
            return Ok(true);
        }
        if mobile::action(frontend, action) {
            return Ok(true);
        }
        if copy::before_action(frontend, key, prefixed, action)? {
            return Ok(true);
        }
        if action == NavigateAction::Settings {
            settings::open(frontend)?;
            return Ok(true);
        }
        if action == NavigateAction::ReloadConfig {
            settings::reload(frontend)?;
            return Ok(true);
        }
        if action == NavigateAction::Help {
            frontend.help = Some(help::Help::default());
            return Ok(true);
        }
        if action == NavigateAction::EnterResizeMode {
            resize::enter(frontend);
            return Ok(true);
        }
        if action == NavigateAction::CopyMode {
            copy::enter(frontend);
            return Ok(true);
        }
        if history::action(frontend, action)? {
            return Ok(true);
        }
        if worktrees::action(frontend, action)? || modal::action(frontend, action)? {
            return Ok(true);
        }
    }
    if action == Some(NavigateAction::Detach) {
        frontend.detach_requested = true;
        return Ok(true);
    }
    if action == Some(NavigateAction::ToggleSidebar) {
        frontend.chrome.settings.sidebar_collapsed = !frontend.chrome.settings.sidebar_collapsed;
        frontend.persist_chrome_preferences();
        return Ok(true);
    }
    if matches!(
        action,
        Some(NavigateAction::NextAgent | NavigateAction::PreviousAgent)
    ) {
        if frontend.runtime.input_lease_current() {
            let view =
                frontend
                    .chrome
                    .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
            let entries = frontend
                .chrome
                .agent_targets(&frontend.runtime.shell, view.detail_body.width);
            if !entries.is_empty() {
                let current = focused_pane(frontend)
                    .and_then(|key| entries.iter().position(|entry| entry == &key));
                let next = match (current, action == Some(NavigateAction::NextAgent)) {
                    (Some(index), true) => (index + 1) % entries.len(),
                    (Some(index), false) => (index + entries.len() - 1) % entries.len(),
                    (None, true) => 0,
                    (None, false) => entries.len() - 1,
                };
                focus_agent(frontend, next)?;
            }
        }
        return Ok(true);
    }
    if let Some(NavigateAction::FocusAgent(index)) = action {
        focus_agent(frontend, index)?;
        return Ok(true);
    }
    let direction = match action {
        Some(NavigateAction::FocusPaneLeft) => Some((crate::layout::NavDirection::Left, false)),
        Some(NavigateAction::FocusPaneRight) => Some((crate::layout::NavDirection::Right, false)),
        Some(NavigateAction::FocusPaneUp) => Some((crate::layout::NavDirection::Up, false)),
        Some(NavigateAction::FocusPaneDown) => Some((crate::layout::NavDirection::Down, false)),
        Some(NavigateAction::SwapPaneLeft) => Some((crate::layout::NavDirection::Left, true)),
        Some(NavigateAction::SwapPaneRight) => Some((crate::layout::NavDirection::Right, true)),
        Some(NavigateAction::SwapPaneUp) => Some((crate::layout::NavDirection::Up, true)),
        Some(NavigateAction::SwapPaneDown) => Some((crate::layout::NavDirection::Down, true)),
        _ => None,
    };
    if let Some((direction, swap)) = direction {
        if frontend.runtime.input_lease_current() {
            let target = frontend
                .runtime
                .shell
                .pane_surface
                .as_ref()
                .and_then(|surface| {
                    let focused = surface.panes.iter().find(|pane| pane.focused)?;
                    let rect = |r: wire::SurfaceRect| {
                        ratatui::layout::Rect::new(r.x, r.y, r.width, r.height)
                    };
                    let target = crate::layout::find_rect_in_direction(
                        &focused.pane_id,
                        rect(focused.rect),
                        direction,
                        surface
                            .panes
                            .iter()
                            .map(|pane| (&pane.pane_id, rect(pane.rect))),
                    )?;
                    Some((focused.pane_id.clone(), target.clone()))
                });
            if let Some((source, target)) = target {
                if swap {
                    match frontend
                        .runtime
                        .issue_method(crate::api::schema::Method::PaneSwap(
                            crate::api::schema::PaneSwapParams {
                                source_pane_id: Some(source),
                                target_pane_id: Some(target),
                                ..Default::default()
                            },
                        )) {
                        Ok(update) => {
                            frontend.update(update)?;
                        }
                        Err(error) => frontend.notice = Some(error),
                    }
                } else {
                    let id = frontend.runtime.shell.active_endpoint_id.clone();
                    let update = frontend.runtime.activate(
                        id,
                        Some(FocusTarget::Pane(target)),
                        Instant::now(),
                    );
                    frontend.update(update)?;
                }
            }
        }
        return Ok(true);
    }
    if let Some(NavigateAction::Zoom) = action {
        if let Some(pane) = focused_pane(frontend) {
            match frontend
                .runtime
                .issue_method(crate::api::schema::Method::PaneZoom(
                    crate::api::schema::PaneZoomParams {
                        pane_id: Some(pane.id),
                        mode: crate::api::schema::PaneZoomMode::Toggle,
                    },
                )) {
                Ok(update) => {
                    frontend.update(update)?;
                }
                Err(error) => frontend.notice = Some(error),
            }
        }
        return Ok(true);
    }
    if matches!(
        action,
        Some(NavigateAction::SplitVertical | NavigateAction::SplitHorizontal)
    ) {
        if let Some(pane) = focused_pane(frontend) {
            match frontend
                .runtime
                .issue_method(crate::api::schema::Method::PaneSplit(
                    crate::api::schema::PaneSplitParams {
                        workspace_id: None,
                        target_pane_id: Some(pane.id),
                        direction: if action == Some(NavigateAction::SplitVertical) {
                            crate::api::schema::SplitDirection::Right
                        } else {
                            crate::api::schema::SplitDirection::Down
                        },
                        ratio: None,
                        cwd: None,
                        focus: true,
                        env: Default::default(),
                    },
                )) {
                Ok(update) => {
                    frontend.update(update)?;
                }
                Err(error) => frontend.notice = Some(error),
            }
        }
        return Ok(true);
    }
    if let Some(action @ (NavigateAction::SwitchWorkspace(_) | NavigateAction::SwitchTab(_))) =
        action
    {
        let selected = match action {
            NavigateAction::SwitchWorkspace(index) => frontend
                .chrome
                .visual_workspace_targets(&frontend.runtime.shell, frontend.cols)
                .get(index)
                .map(|key| (key.endpoint.clone(), FocusTarget::Workspace(key.id.clone()))),
            NavigateAction::SwitchTab(index) => {
                let id = &frontend.runtime.shell.active_endpoint_id;
                frontend
                    .runtime
                    .shell
                    .endpoint(id)
                    .and_then(|endpoint| endpoint.cache.snapshot())
                    .and_then(|snapshot| {
                        snapshot
                            .tabs
                            .iter()
                            .filter(|tab| {
                                Some(&tab.workspace_id) == snapshot.focused_workspace_id.as_ref()
                            })
                            .nth(index)
                    })
                    .map(|tab| (id.clone(), FocusTarget::Tab(tab.tab_id.clone())))
            }
            _ => unreachable!("indexed navigation action"),
        };
        if let Some((id, target)) = selected {
            let update = frontend.runtime.activate(id, Some(target), Instant::now());
            frontend.update(update)?;
        }
        return Ok(true);
    }
    if matches!(
        action,
        Some(NavigateAction::CyclePaneNext | NavigateAction::CyclePanePrevious)
    ) {
        let id = &frontend.runtime.shell.active_endpoint_id;
        let target = frontend
            .runtime
            .shell
            .endpoint(id)
            .and_then(|endpoint| endpoint.cache.snapshot())
            .and_then(|snapshot| {
                let panes = snapshot
                    .panes
                    .iter()
                    .filter(|pane| Some(&pane.tab_id) == snapshot.focused_tab_id.as_ref())
                    .collect::<Vec<_>>();
                let index = panes.iter().position(|pane| pane.focused)?;
                let next = if action == Some(NavigateAction::CyclePaneNext) {
                    (index + 1) % panes.len()
                } else {
                    (index + panes.len() - 1) % panes.len()
                };
                Some(panes[next].pane_id.clone())
            });
        if let Some(target) = target {
            let update = frontend.runtime.activate(
                id.clone(),
                Some(FocusTarget::Pane(target)),
                Instant::now(),
            );
            frontend.update(update)?;
        }
        return Ok(true);
    }
    let workspace_step = if action == Some(NavigateAction::PreviousWorkspace) {
        Some(false)
    } else if action == Some(NavigateAction::NextWorkspace) {
        Some(true)
    } else {
        None
    };
    if let Some(forward) = workspace_step {
        let active = &frontend.runtime.shell.active_endpoint_id;
        let entries = frontend
            .chrome
            .visual_workspace_targets(&frontend.runtime.shell, frontend.cols);
        let current = frontend
            .runtime
            .shell
            .endpoint(active)
            .and_then(|endpoint| endpoint.cache.snapshot())
            .and_then(|snapshot| snapshot.focused_workspace_id.as_ref());
        if !entries.is_empty() {
            let index = entries
                .iter()
                .position(|key| &key.endpoint == active && Some(&key.id) == current)
                .unwrap_or(0);
            let target = if forward {
                &entries[(index + 1) % entries.len()]
            } else {
                &entries[(index + entries.len() - 1) % entries.len()]
            };
            let update = frontend.runtime.activate(
                target.endpoint.clone(),
                Some(FocusTarget::Workspace(target.id.clone())),
                Instant::now(),
            );
            frontend.update(update)?;
        }
        return Ok(true);
    }
    let tab_step = if action == Some(NavigateAction::PreviousTab) {
        Some(false)
    } else if action == Some(NavigateAction::NextTab) {
        Some(true)
    } else {
        None
    };
    if let Some(forward) = tab_step {
        let endpoint = &frontend.runtime.shell.active_endpoint_id;
        if let Some(snapshot) = frontend
            .runtime
            .shell
            .endpoint(endpoint)
            .and_then(|endpoint| endpoint.cache.snapshot())
        {
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| Some(&tab.workspace_id) == snapshot.focused_workspace_id.as_ref())
                .collect::<Vec<_>>();
            if let Some(index) = tabs.iter().position(|tab| tab.focused) {
                let index = if forward {
                    (index + 1) % tabs.len()
                } else {
                    (index + tabs.len() - 1) % tabs.len()
                };
                let update = frontend.runtime.activate(
                    endpoint.clone(),
                    Some(FocusTarget::Tab(tabs[index].tab_id.clone())),
                    Instant::now(),
                );
                frontend.update(update)?;
            }
        }
        return Ok(true);
    }
    Ok(prefixed)
}
