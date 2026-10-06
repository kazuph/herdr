//! Normal TUI owns chrome and input; every runtime resource stays at its endpoint.
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::catalog::{self, LiveCatalog};
use super::chrome::{ChromeSettings, ClientChrome};
use super::handshake::EndpointConnectOptions;
use super::runtime::{EndpointRuntime, RuntimeUpdate};
use super::shell::ClientShellState;
use super::supervisor::EndpointSupervisors;
use super::{ClientEndpointId, EndpointNegotiation, EndpointRegistry};
use crate::protocol::endpoint_wire as wire;

mod ascii;
#[cfg(unix)]
mod clipboard_images;
mod context;
mod copy;
mod custom;
#[cfg(unix)]
mod direct;
mod editor;
mod graphics;
mod help;
mod history;
mod host;
mod input;
mod jobs;
mod menu;
mod mobile;
mod modal;
mod navigator;
mod notes;
mod notification;
mod popup;
mod popup_selection;
mod preferences;
mod resize;
mod right_click;
mod selection;
mod settings;
mod startup;
mod worktrees;

#[cfg(all(test, unix))]
mod process_tests;

pub(crate) use startup::run;

pub(crate) struct ClientFrontend {
    pub(crate) runtime: EndpointRuntime,
    pub(crate) chrome: ClientChrome,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    mobile: Option<mobile::Mobile>,
    navigator: Option<navigator::Navigator>,
    options: EndpointConnectOptions,
    host: host::HostPresentation,
    pub(crate) notice: Option<String>,
    blit: crate::protocol::render_ansi::BlitEncoder,
    graphics: crate::kitty_graphics::endpoint_client::ClientState,
    #[cfg(unix)]
    direct: direct::Direct,
    force_redraw: bool,
    keybinds: crate::config::LiveKeybindConfig,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    prefix: bool,
    detach_requested: bool,
    draw_host_cursor: bool,
    job_indicator_pending: bool,
    last_jobs_redraw: Instant,
    modal: Option<modal::Modal>,
    worktrees: Option<worktrees::Worktrees>,
    help: Option<help::Help>,
    menu: Option<menu::Menu>,
    context: Option<context::Context>,
    notes: Option<notes::Notes>,
    settings: Option<settings::Settings>,
    host_settings: settings::HostSettings,
    reload_request: Option<settings::ReloadRequest>,
    job_request: Option<jobs::JobRequest>,
    editor_request: Option<editor::EditorRequest>,
    custom_request: Option<custom::CommandRequest>,
    notifications: notification::Notifications,
    prompt_new_tab_name: bool,
    confirm_close: bool,
    focus_history: history::FocusHistory,
    copy_mode: Option<copy::CopyMode>,
    copy_action: Option<copy::DeferredAction>,
    resize_mode: Option<copy::Owner>,
    split_drag: Option<resize::SplitDrag>,
    right_click: Option<right_click::Gesture>,
    host_theme: crate::terminal_theme::TerminalTheme,
    pointer_selection: selection::PointerSelection,
    popup_selection: popup_selection::PopupSelection,
    copy_on_select: bool,
    prefix_input_source: crate::platform::RealPrefixInputSource,
    ascii_realm: bool,
    chrome_preferences: preferences::ClientChromePreferences,
    chrome_preferences_path: Option<std::path::PathBuf>,
}

impl ClientFrontend {
    pub(crate) fn from_stream(
        stream: crate::ipc::LocalStream,
        welcome: crate::protocol::endpoint::EndpointServerWelcome,
        config: &crate::config::Config,
        settings: ChromeSettings,
        cols: u16,
        rows: u16,
        options: EndpointConnectOptions,
    ) -> io::Result<Self> {
        let now = Instant::now();
        let mut supervisors = EndpointSupervisors::new(&[], now);
        supervisors.add_local(
            crate::server::socket_paths::client_socket_path(),
            Some(1),
            now,
        );
        let mut shell = ClientShellState::new();
        shell.begin_connection(&ClientEndpointId::Local, 1);
        let mut runtime =
            EndpointRuntime::new(shell, EndpointRegistry::empty(), supervisors, options);
        let negotiation = EndpointNegotiation::new(welcome.methods, welcome.capabilities);
        let transport = super::transport::start(
            stream,
            (),
            ClientEndpointId::Local,
            1,
            &negotiation,
            runtime.reader_sender(),
        )?;
        runtime
            .endpoints
            .insert(ClientEndpointId::Local, transport, 1, negotiation, false);
        // The first actual snapshot supplies the current viewer selection for activation.
        runtime.activate(ClientEndpointId::Local, None, now);
        Ok(Self::from_runtime(
            runtime,
            config,
            settings,
            (cols, rows),
            options,
        ))
    }

    pub(crate) fn from_runtime(
        runtime: EndpointRuntime,
        config: &crate::config::Config,
        settings: ChromeSettings,
        size: (u16, u16),
        options: EndpointConnectOptions,
    ) -> Self {
        Self {
            runtime,
            chrome: ClientChrome::new(settings),
            cols: size.0,
            rows: size.1,
            mobile: None,
            navigator: None,
            options,
            host: host::HostPresentation::new(config.ui.mouse_capture),
            notice: None,
            blit: crate::protocol::render_ansi::BlitEncoder::new(),
            graphics: crate::kitty_graphics::endpoint_client::ClientState::default(),
            #[cfg(unix)]
            direct: direct::Direct::default(),
            force_redraw: true,
            keybinds: crate::config::LiveKeybindConfig {
                prefix: config.prefix_key(),
                keybinds: config.keybinds(),
            },
            remote_image_paste_key: config
                .remote_image_paste_key()
                .unwrap_or_else(|diagnostic| {
                    tracing::warn!(%diagnostic, "local remote image paste key config diagnostic");
                    None
                }),
            prefix: false,
            detach_requested: false,
            draw_host_cursor: super::super::should_draw_host_cursor(config.ui.host_cursor),
            job_indicator_pending: false,
            last_jobs_redraw: Instant::now(),
            modal: None,
            worktrees: None,
            help: None,
            menu: None,
            context: None,
            notes: None,
            settings: None,
            host_settings: settings::HostSettings::new(config),
            reload_request: None,
            job_request: None,
            editor_request: None,
            custom_request: None,
            notifications: notification::Notifications::default(),
            prompt_new_tab_name: config.ui.prompt_new_tab_name,
            confirm_close: config.ui.confirm_close,
            focus_history: history::FocusHistory::default(),
            copy_mode: None,
            copy_action: None,
            resize_mode: None,
            split_drag: None,
            right_click: None,
            host_theme: crate::terminal_theme::TerminalTheme::default(),
            pointer_selection: selection::PointerSelection::default(),
            popup_selection: popup_selection::PopupSelection::default(),
            copy_on_select: config.ui.copy_on_select,
            prefix_input_source: crate::platform::RealPrefixInputSource::default(),
            ascii_realm: false,
            chrome_preferences: preferences::ClientChromePreferences::default(),
            chrome_preferences_path: None,
        }
    }

    fn persist_chrome_preferences(&mut self) {
        let Some(path) = self.chrome_preferences_path.as_deref() else {
            return;
        };
        // The fork snapshot persists the current width, including the Normal preset.
        self.chrome_preferences.sidebar_width = Some(self.chrome.settings.sidebar_width);
        self.chrome_preferences.sidebar_section_split =
            Some(self.chrome.settings.sidebar_section_split);
        self.chrome_preferences.sidebar_collapsed = Some(self.chrome.settings.sidebar_collapsed);
        let mut local_groups: Vec<_> = self
            .chrome
            .collapsed_groups
            .iter()
            .filter_map(|(endpoint, key)| {
                matches!(endpoint, ClientEndpointId::Local).then_some(key.clone())
            })
            .collect();
        local_groups.sort();
        self.chrome_preferences.collapsed_groups = Some(local_groups);
        let mut remote = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (endpoint, key) in &self.chrome.collapsed_groups {
            if let ClientEndpointId::Ssh(profile_id) = endpoint {
                remote
                    .entry(profile_id.clone())
                    .or_default()
                    .push(key.clone());
            }
        }
        self.chrome_preferences.remote_collapsed_groups = remote
            .into_iter()
            .map(|(profile_id, mut collapsed_groups)| {
                collapsed_groups.sort();
                preferences::ClientRemoteCollapsedGroups {
                    profile_id,
                    collapsed_groups,
                }
            })
            .collect();
        self.chrome_preferences.collapsed_sections = Some(
            self.chrome
                .collapsed_sections
                .iter()
                .map(|(endpoint, section)| preferences::ClientCollapsedSection {
                    profile_id: match endpoint {
                        ClientEndpointId::Local => None,
                        ClientEndpointId::Ssh(profile) => Some(profile.clone()),
                    },
                    section: *section,
                })
                .collect(),
        );
        if let Err(error) = preferences::store(path, self.chrome_preferences.clone()) {
            self.notice = Some(error);
        }
    }

    pub(crate) fn dispatch_input(
        &mut self,
        event: crate::raw_input::RawInputEvent,
    ) -> io::Result<()> {
        input::dispatch(self, event, None)
    }

    fn update(&mut self, update: RuntimeUpdate) -> io::Result<bool> {
        let repaint = update.repaint || update.clear_host_effects || !update.completed.is_empty();
        if self
            .modal
            .as_ref()
            .is_some_and(|modal| !modal.current(self))
        {
            self.modal = None;
        }
        if let Some(error) = update.error {
            tracing::warn!(endpoint = ?self.runtime.shell.active_endpoint_id, %error, "endpoint presentation failed");
            self.focus_history.cancel();
            self.notice = Some(error);
        }
        if update.clear_host_effects {
            #[cfg(unix)]
            direct::cancel(self);
            self.graphics.set_scope("");
            self.host.clear()?;
            self.force_redraw = true;
        }
        for effect in update.host_effects {
            if notification::viewer_focus(self, &effect)? {
                continue;
            }
            if notification::forwarded_effect(self, &effect) {
                continue;
            }
            #[cfg(unix)]
            if direct::effect(self, &effect)? {
                continue;
            }
            self.host.apply(&self.runtime, effect)?;
        }
        // Endpoint notices are qualified and remain silent. Shell notification controls own
        // their display and target navigation; sound is never emitted by this receive loop.
        for effect in update.notifications {
            if self
                .runtime
                .endpoints
                .accepts(&effect.endpoint_id, effect.generation)
            {
                if let wire::ServerMessage::SemanticNotification(notification) = effect.message {
                    self.notice = Some(format!(
                        "{}: {}",
                        self.runtime
                            .shell
                            .endpoint(&effect.endpoint_id)
                            .map_or("Herdr", |endpoint| endpoint.label.as_str()),
                        notification.title
                    ));
                }
            }
        }
        for completed in update.completed {
            jobs::completed(self, &completed)?;
            editor::completed(self, &completed)?;
            let custom_handled = custom::completed(self, &completed)?;
            menu::completed(self, &completed);
            settings::completed(self, &completed);
            modal::completed(self, &completed);
            context::completed(self, &completed)?;
            copy::completed(self, &completed)?;
            selection::completed(self, &completed);
            popup_selection::completed(self, &completed);
            let worktree_handled = worktrees::completed(self, &completed)?;
            if let Err(error) = completed.result {
                if !worktree_handled && !custom_handled {
                    self.notice = Some(error.message);
                }
            }
        }
        jobs::observe(self);
        editor::observe(self);
        custom::observe(self);
        notification::observe(self);
        menu::observe(self);
        context::observe(self)?;
        notes::observe(self);
        settings::observe(self);
        history::observe(self);
        copy::observe(self)?;
        copy::resume_action(self)?;
        selection::observe(self)?;
        popup_selection::observe(self)?;
        resize::observe(self);
        resize::observe_drag(self)?;
        right_click::observe(self)?;
        worktrees::observe(self);
        ascii::sync(self);
        Ok(repaint)
    }

    fn draw(&mut self) -> io::Result<()> {
        mobile::sync_selection(self);
        navigator::observe(self);
        let mut view = self
            .chrome
            .compute_view(&self.runtime.shell, self.cols, self.rows);
        let size = wire::ClientSurfaceSize {
            cols: view.layout.pane_surface.width,
            rows: view.layout.pane_surface.height,
        };
        if size != self.options.surface_size {
            self.options.surface_size = size;
            let update = self.runtime.resize(self.options, Instant::now());
            self.update(update)?;
            view.surface = None;
        }
        let mobile_presentation = mobile::presentation(self);
        let navigator_presentation = navigator::presentation(self);
        let frame = presentation_frame(
            self.chrome.render_with_overlay(&view, |frame| {
                copy::render(self, frame, view.layout.pane_surface);
                selection::render(self, frame, view.layout.pane_surface);
                popup_selection::render(self, frame, view.layout.pane_surface);
                if self.resize_mode.is_some() {
                    crate::ui::render_resize_overlay_facts(
                        frame,
                        view.layout.pane_surface,
                        &self.chrome.settings.palette,
                    );
                }
                mobile::render(self, frame, view.layout.pane_surface);
                navigator::render(self, frame);
                notification::render(self, frame);
                menu::render(self, frame, view.menu_launcher);
                context::render(self, frame);
                notes::render(self, frame);
                help::render(self, frame);
                settings::render(self, frame);
                worktrees::render(self, frame);
                if let Some(modal) = &self.modal {
                    modal.render(
                        frame,
                        view.layout.area,
                        view.layout.pane_surface,
                        &self.chrome.settings.palette,
                    );
                }
                // The existing bottom-row diagnostic must cover the pane action bar.
                if let Some(notice) = &self.notice {
                    if self.rows > 0 {
                        frame.render_widget(
                            ratatui::widgets::Paragraph::new(notice.as_str()).style(
                                ratatui::style::Style::default().fg(self
                                    .chrome
                                    .settings
                                    .palette
                                    .red),
                            ),
                            ratatui::layout::Rect::new(0, self.rows - 1, self.cols, 1),
                        );
                    }
                }
            }),
            (self.runtime.input_lease_current() || self.runtime.awaiting_surface_pair())
                && self.mobile.is_none()
                && self.navigator.is_none()
                && self.menu.is_none()
                && self.context.is_none()
                && self.notes.is_none()
                && self.help.is_none()
                && self.settings.is_none()
                && self.worktrees.is_none()
                && !copy::active(self)
                && !self
                    .modal
                    .as_ref()
                    .is_some_and(modal::Modal::hides_terminal_cursor),
            self.draw_host_cursor,
        );
        let encoded = if self.draw_host_cursor {
            self.blit
                .encode_with_suppressed_visible_cursor(&frame, self.force_redraw)
        } else {
            self.blit.encode(&frame, self.force_redraw)
        };
        use std::io::Write as _;
        let mut stdout = io::stdout().lock();
        let (next_graphics, graphics_bytes) = graphics::encode(self, &view);
        crate::client::write_encoded_frame_with_graphics(
            &mut stdout,
            &encoded.bytes,
            &graphics_bytes,
        )?;
        stdout.flush()?;
        self.graphics = next_graphics;
        self.blit.commit(frame, encoded);
        mobile::commit_presentation(self, mobile_presentation);
        navigator::commit(self, navigator_presentation);
        self.force_redraw = false;
        Ok(())
    }

    /// True while any endpoint's projection has a finished job still inside the
    /// sidebar indicator retention window, so the 100ms tick keeps repainting
    /// until the dot expires.
    fn job_indicator_expiry_pending(&self) -> bool {
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.runtime.shell.endpoints.iter().any(|endpoint| {
            endpoint.cache.snapshot().is_some_and(|snapshot| {
                endpoint
                    .jobs
                    .has_finished_indicator_pending_expiry(snapshot, now_unix_ms)
            })
        })
    }

    async fn event_loop(
        &mut self,
        mut input_events: mpsc::Receiver<super::super::ClientLoopEvent>,
        quit: Arc<AtomicBool>,
    ) -> io::Result<()> {
        let (catalog_tx, mut catalog_rx) = mpsc::channel(256);
        let watch = catalog::watch(crate::machine::default_path(), catalog_tx, quit.clone())?;
        let _watch_guard = CatalogGuard {
            quit: quit.clone(),
            watch,
        };
        let mut live_catalog = LiveCatalog::default();
        let mut maintenance = tokio::time::interval(Duration::from_millis(100));
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut animation = tokio::time::interval(crate::app::HEADLESS_ANIMATION_INTERVAL);
        animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.draw()?;
        while !quit.load(Ordering::Acquire) && !self.detach_requested {
            let repaint = tokio::select! {
                event = self.runtime.reader_events.recv() => {
                    let Some(event) = event else { break };
                    let update = self.runtime.receive(event, Instant::now());
                    self.update(update)?
                }
                event = self.runtime.supervisor_events.recv() => {
                    let Some(event) = event else { break };
                    let update = self.runtime.supervisor_event(event, Instant::now());
                    self.update(update)?
                }
                // Keep input in the existing bounded channel across a split snapshot /
                // surface update instead of consuming it while the lease rejects it.
                event = input_events.recv(), if !self.runtime.awaiting_surface_pair() => {
                    let Some(event) = event else { break };
                    input::handle(self, event)?
                }
                _ = async {
                    match self.pointer_selection.deadline.into_iter().chain(self.popup_selection.deadline).min() {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let now = Instant::now();
                    if self.pointer_selection.deadline.is_some_and(|deadline| deadline <= now) { selection::tick(self,now)?; }
                    if self.popup_selection.deadline.is_some_and(|deadline| deadline <= now) { popup_selection::tick(self,now)?; }
                    true
                }
                update = catalog_rx.recv() => {
                    if let Some(update) = update {
                        if live_catalog.apply(update) {
                            let update = self.runtime.apply_catalog(live_catalog.profiles().to_vec(), Instant::now());
                            self.update(update)?;
                        }
                        if let Some(error) = live_catalog.error() {
                            self.notice = Some(error.to_owned());
                        }
                    }
                    true
                }
                // Existing fork client timer interval. This schedules health and retry work;
                // endpoint connections execute on independent workers, never on this loop.
                _ = animation.tick() => {
                    self.chrome.spinner_tick = self.chrome.spinner_tick
                        .wrapping_add(crate::app::HEADLESS_ANIMATION_TICK_STEP);
                    self.runtime.shell.endpoints.iter().any(|endpoint| {
                        endpoint.cache.snapshot().is_some_and(|snapshot| {
                            snapshot.workspaces.iter().any(|workspace| {
                                workspace.agent_status == crate::api::schema::AgentStatus::Working
                            })
                        })
                    })
                }
                _ = maintenance.tick() => {
                    let now = Instant::now();
                    let update = self.runtime.tick(now);
                    let repaint = self.update(update)?;
                    let indicator_pending = self.job_indicator_expiry_pending();
                    let indicator_just_expired =
                        std::mem::replace(&mut self.job_indicator_pending, indicator_pending)
                            && !indicator_pending;
                    let jobs_due = (self.chrome.detail_view == crate::app::state::SidebarDetailView::Jobs
                        || indicator_pending)
                        && now >= self.last_jobs_redraw + crate::app::JOBS_REFRESH_INTERVAL;
                    if jobs_due { self.last_jobs_redraw = now; }
                    repaint || jobs_due || indicator_just_expired
                }
            };
            if repaint {
                self.draw()?;
            }
        }
        #[cfg(unix)]
        direct::cancel(self);
        Ok(())
    }
}

fn presentation_frame(
    mut frame: crate::protocol::FrameData,
    input_available: bool,
    draw_host_cursor: bool,
) -> crate::protocol::FrameData {
    // A surface can be painted before its ordered presentation effects finish. Do not
    // advertise an input cursor until the exact surface/snapshot lease is current.
    if !input_available {
        if let Some(cursor) = frame.cursor.as_mut() {
            cursor.visible = false;
        }
    }
    if draw_host_cursor {
        crate::protocol::render_ansi::frame_with_drawn_cursor(frame)
    } else {
        frame
    }
}

struct CatalogGuard {
    quit: Arc<AtomicBool>,
    watch: std::thread::JoinHandle<()>,
}
impl Drop for CatalogGuard {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Release);
        self.watch.thread().unpark();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoint_frontend_cursor_requires_input_lease_and_preserves_host_policy() {
        let frame = crate::protocol::FrameData {
            width: 1,
            height: 1,
            cells: vec![crate::protocol::CellData {
                symbol: "x".into(),
                fg: 0,
                bg: 0,
                modifier: 0,
                skip: false,
                hyperlink: None,
            }],
            cursor: Some(crate::protocol::CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        for drawn in [false, true] {
            let blocked = super::presentation_frame(frame.clone(), false, drawn);
            assert!(!blocked.cursor.as_ref().unwrap().visible);
            assert_eq!(blocked.cells[0].modifier, 0);
            let encoder = crate::protocol::render_ansi::BlitEncoder::new();
            assert!(!encoder
                .encode(&blocked, true)
                .bytes
                .windows(6)
                .any(|bytes| bytes == b"\x1b[?25h"));
            let ready = super::presentation_frame(frame.clone(), true, drawn);
            assert!(ready.cursor.as_ref().unwrap().visible);
            assert_eq!(ready.cells[0].modifier, if drawn { 1 << 6 } else { 0 });
            let encoded = if drawn {
                encoder.encode_with_suppressed_visible_cursor(&ready, true)
            } else {
                encoder.encode(&ready, true)
            };
            assert_eq!(
                encoded.bytes.windows(6).any(|bytes| bytes == b"\x1b[?25h"),
                !drawn
            );
        }
    }
}
