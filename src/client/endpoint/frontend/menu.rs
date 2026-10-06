//! Fork menu presentation captures the current endpoint; runtime actions never use host state.
use super::*;
use crate::api::schema as api;
use crate::app::state::{DangerousAction, MenuListState, SidebarWidthSource};
use crate::app::{
    global_menu_action_label, global_menu_actions_for, GlobalMenuAction, GlobalMenuInput,
};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

struct Owner {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    workspace: Option<String>,
    tab: Option<String>,
}

impl Owner {
    fn capture(frontend: &ClientFrontend) -> Option<Self> {
        let endpoint = frontend
            .runtime
            .shell
            .endpoint(&frontend.runtime.shell.active_endpoint_id)?;
        let generation = endpoint.generation?;
        let snapshot = endpoint.cache.live_snapshot(generation)?;
        Some(Self {
            endpoint: endpoint.endpoint_id.clone(),
            generation,
            boot: snapshot.boot_id.clone(),
            workspace: snapshot.focused_workspace_id.clone(),
            tab: snapshot.focused_tab_id.clone(),
        })
    }
    fn current(&self, frontend: &ClientFrontend) -> bool {
        frontend.runtime.shell.active_endpoint_id == self.endpoint
            && frontend
                .runtime
                .shell
                .endpoint(&self.endpoint)
                .and_then(|endpoint| endpoint.cache.live_snapshot(self.generation))
                .is_some_and(|snapshot| {
                    snapshot.boot_id == self.boot
                        && snapshot.focused_workspace_id == self.workspace
                        && snapshot.focused_tab_id == self.tab
                })
    }
}

enum Page {
    List,
    WarningQuery(String),
    // Missing facts remain unresolved; they never become a safe empty warning list.
    WarningUnavailable,
    Danger(DangerousAction, Vec<api::AgentSessionWarningInfo>),
}

pub(super) struct Menu {
    owner: Owner,
    state: MenuListState,
    actions: Vec<GlobalMenuAction>,
    page: Page,
}

impl Menu {
    pub(super) fn mode(&self) -> crate::app::Mode {
        match self.page {
            Page::Danger(_, _) => crate::app::Mode::ConfirmDanger,
            _ => crate::app::Mode::GlobalMenu,
        }
    }
}

pub(super) fn open(frontend: &mut ClientFrontend) {
    let Some(owner) = Owner::capture(frontend) else {
        return;
    };
    selection::clear(frontend);
    frontend.prefix = false;
    frontend.menu = Some(Menu {
        owner,
        state: MenuListState::new(0),
        actions: global_menu_actions_for(notes::available(frontend)),
        page: Page::List,
    });
    frontend.force_redraw = true;
}

fn rect(frontend: &ClientFrontend, launcher: Rect, menu: &Menu) -> Rect {
    let labels = menu
        .actions
        .iter()
        .copied()
        .map(global_menu_action_label)
        .collect::<Vec<_>>();
    crate::app::global_menu_rect_from(
        Rect::new(0, 0, frontend.cols, frontend.rows),
        launcher,
        &labels,
        |_| false,
    )
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, launcher: Rect) {
    let Some(menu) = &frontend.menu else {
        return;
    };
    match &menu.page {
        Page::Danger(action, warnings) => crate::ui::render_confirm_danger_from(
            frame,
            Rect::new(0, 0, frontend.cols, frontend.rows),
            &frontend.chrome.settings.palette,
            *action,
            warnings,
        ),
        _ => {
            let labels = menu
                .actions
                .iter()
                .copied()
                .map(global_menu_action_label)
                .collect::<Vec<_>>();
            crate::ui::render_global_menu_from(
                frame,
                rect(frontend, launcher, menu),
                &frontend.chrome.settings.palette,
                &labels,
                menu.state.highlighted,
                |_| false,
            );
        }
    }
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend
        .menu
        .as_ref()
        .is_some_and(|menu| !menu.owner.current(frontend))
    {
        frontend.menu = None;
    }
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) {
    let Some(menu) = frontend.menu.as_ref() else {
        return;
    };
    let Page::WarningQuery(request) = &menu.page else {
        return;
    };
    if *request != result.request_id
        || menu.owner.endpoint != result.endpoint_id
        || menu.owner.generation != result.generation
        || menu.owner.boot != result.boot_id
        || !menu.owner.current(frontend)
    {
        return;
    }
    let warnings = result
        .result
        .as_ref()
        .ok()
        .and_then(|value| serde_json::from_value::<api::ResponseResult>(value.clone()).ok())
        .and_then(|result| match result {
            api::ResponseResult::SessionSnapshot { snapshot } => snapshot.agent_session_warnings,
            _ => None,
        });
    if let Some(menu) = frontend.menu.as_mut() {
        menu.page = match warnings {
            Some(warnings) => Page::Danger(DangerousAction::Restart, warnings),
            None => {
                tracing::warn!(endpoint = ?result.endpoint_id, "restart warning facts unavailable; confirmation is blocked");
                Page::WarningUnavailable
            }
        };
    }
}

fn issue(frontend: &mut ClientFrontend, method: api::Method) -> io::Result<()> {
    match frontend.runtime.issue_method(method) {
        Ok(update) => {
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn apply_action(
    frontend: &mut ClientFrontend,
    action: GlobalMenuAction,
) -> io::Result<()> {
    open(frontend);
    let Some(menu) = frontend.menu.take() else {
        return Ok(());
    };
    if !menu.actions.contains(&action) {
        return Ok(());
    }
    apply(frontend, menu, action)
}

fn apply(
    frontend: &mut ClientFrontend,
    mut menu: Menu,
    action: GlobalMenuAction,
) -> io::Result<()> {
    use GlobalMenuAction::*;
    match action {
        Separator => frontend.menu = Some(menu),
        NewWorkspace | NewTab => {
            modal::action(
                frontend,
                if action == NewWorkspace {
                    crate::app::NavigateAction::NewWorkspace
                } else {
                    crate::app::NavigateAction::NewTab
                },
            )?;
        }
        Settings => settings::open(frontend)?,
        Keybinds => frontend.help = Some(help::Help::default()),
        ReloadConfig => settings::reload(frontend)?,
        SidebarNarrow | SidebarNormal | SidebarWide => {
            let settings = &mut frontend.chrome.settings;
            settings.sidebar_width = match action {
                SidebarNarrow => settings.sidebar_min_width,
                SidebarWide => settings.sidebar_max_width,
                _ => settings
                    .default_sidebar_width
                    .clamp(settings.sidebar_min_width, settings.sidebar_max_width),
            };
            settings.sidebar_width_source = if action == SidebarNormal {
                SidebarWidthSource::ConfigDefault
            } else {
                SidebarWidthSource::Manual
            };
            frontend.persist_chrome_preferences();
        }
        WhatsNew => notes::open(frontend),
        Detach => frontend.detach_requested = true,
        StopServer | RestoreAgents => {
            menu.page = Page::Danger(
                if action == StopServer {
                    DangerousAction::StopServer
                } else {
                    DangerousAction::RestoreAgents
                },
                Vec::new(),
            );
            frontend.menu = Some(menu);
        }
        Restart => {
            match frontend
                .runtime
                .issue_method_with_id(api::Method::SessionSnapshot(api::EmptyParams::default()))
            {
                Ok((request, update)) => {
                    menu.page = Page::WarningQuery(request);
                    frontend.menu = Some(menu);
                    frontend.update(update)?;
                }
                Err(error) => {
                    frontend.notice = Some(error);
                    frontend.menu = Some(menu);
                }
            }
        }
    }
    frontend.force_redraw = true;
    Ok(())
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        return Ok(false);
    }
    let Some(mut menu) = frontend.menu.take() else {
        return Ok(false);
    };
    if !menu.owner.current(frontend) {
        return Ok(true);
    }
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    let mut selected = None;
    let mut close = false;
    let mut confirm = false;
    match &menu.page {
        Page::List => match event {
            RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                let mut input = GlobalMenuInput {
                    state: &mut menu.state,
                    actions: &menu.actions,
                    closed: false,
                };
                selected = input.key(KeyEvent::new_with_kind(key.code, key.modifiers, key.kind));
                close = input.closed;
            }
            RawInputEvent::Mouse(mouse) => {
                let popup = rect(frontend, view.menu_launcher, &menu);
                let inner = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .inner(popup);
                if inner.contains((mouse.column, mouse.row).into()) {
                    let index = usize::from(mouse.row - inner.y);
                    if let Some(action) = menu
                        .actions
                        .get(index)
                        .copied()
                        .filter(|action| *action != GlobalMenuAction::Separator)
                    {
                        menu.state.highlighted = index;
                        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                            selected = Some(action);
                        }
                    }
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    close = true;
                }
            }
            _ => {}
        },
        Page::Danger(_, warnings) => match event {
            RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => confirm = true,
                KeyCode::Esc => close = true,
                _ => {}
            },
            RawInputEvent::Mouse(mouse)
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
            {
                if let Some(popup) =
                    crate::ui::confirm_danger_popup_rect(view.layout.area, warnings.len())
                {
                    let inner = ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .inner(popup);
                    let (accept, cancel) = crate::ui::confirm_danger_button_rects(inner);
                    confirm = accept.contains((mouse.column, mouse.row).into());
                    close = cancel.contains((mouse.column, mouse.row).into())
                        || !popup.contains((mouse.column, mouse.row).into());
                }
            }
            _ => {}
        },
        Page::WarningQuery(_) | Page::WarningUnavailable => {
            close = matches!(event, RawInputEvent::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc);
        }
    }
    if close {
        frontend.force_redraw = true;
        return Ok(true);
    }
    if let Some(action) = selected {
        if frontend.runtime.input_lease_current() {
            apply(frontend, menu, action)?;
        } else {
            frontend.menu = Some(menu);
        }
    } else if confirm && frontend.runtime.input_lease_current() {
        if let Page::Danger(action, _) = menu.page {
            issue(
                frontend,
                match action {
                    DangerousAction::StopServer => {
                        api::Method::ServerStop(api::EmptyParams::default())
                    }
                    DangerousAction::Restart => {
                        api::Method::ServerRestart(api::EmptyParams::default())
                    }
                    DangerousAction::RestoreAgents => {
                        api::Method::AgentRestore(api::AgentRestoreParams { dry_run: false })
                    }
                },
            )?;
        }
    } else {
        frontend.menu = Some(menu);
    }
    Ok(true)
}

pub(super) fn graphics_rect(frontend: &ClientFrontend, launcher: Rect) -> Option<Rect> {
    let menu = frontend.menu.as_ref()?;
    match &menu.page {
        Page::Danger(_, warnings) => crate::ui::confirm_danger_popup_rect(
            Rect::new(0, 0, frontend.cols, frontend.rows),
            warnings.len(),
        ),
        _ => Some(rect(frontend, launcher, menu)),
    }
}
