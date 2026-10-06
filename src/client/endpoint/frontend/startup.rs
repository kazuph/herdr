use super::*;

pub(crate) fn run() -> io::Result<()> {
    use super::super::super as client;
    client::init_logging();
    tracing::debug!(requested_encoding = ?client::requested_render_encoding(), "client owns text composition and terminal ANSI output");
    crate::logging::startup("client");
    crate::server::autodetect::validate_running_server_compatibility()?;
    let loaded = crate::config::Config::load();
    let config = &loaded.config;
    let graphics = config.experimental.kitty_graphics;
    let (cols, rows, cell_width_px, cell_height_px, exact) =
        client::initial_terminal_geometry(graphics);
    let theme = crate::app::theme_runtime_config(config, true);
    let (palette, _) = crate::app::resolve_effective_theme(&theme, None);
    let saved = crate::persist::load();
    let mut settings = ChromeSettings::from_config(config, palette, saved.as_ref());
    let preferences_path =
        preferences::path_for_local_endpoint(&crate::server::socket_paths::client_socket_path());
    let chrome_preferences = preferences::load(&preferences_path);
    if let Some(preferences) = &chrome_preferences {
        settings.sidebar_width = preferences
            .sidebar_width
            .unwrap_or(config.ui.sidebar_width)
            .clamp(settings.sidebar_min_width, settings.sidebar_max_width);
        settings.sidebar_width_source = if preferences.sidebar_width.is_some() {
            crate::app::state::SidebarWidthSource::Persisted
        } else {
            crate::app::state::SidebarWidthSource::ConfigDefault
        };
        if let Some(collapsed) = preferences.sidebar_collapsed {
            settings.sidebar_collapsed = collapsed;
        }
        if let Some(split) = preferences
            .sidebar_section_split
            .filter(|split| split.is_finite())
        {
            settings.sidebar_section_split = split.clamp(0.1, 0.9);
        }
    }
    let mut chrome = ClientChrome::new(settings);
    let initial = chrome.compute_view(&ClientShellState::new(), cols, rows);
    let options = EndpointConnectOptions {
        surface_size: wire::ClientSurfaceSize {
            cols: initial.layout.pane_surface.width,
            rows: initial.layout.pane_surface.height,
        },
        cell_width_px,
        cell_height_px,
        pixel_geometry_exact: exact,
        endpoint_keybindings: false,
        mouse_capture: config.ui.mouse_capture,
        surface_active: false,
    };
    // Main/session startup still owns starting Local. The TUI connects to the existing
    // socket; SSH supervisor workers also connect only to an already running server.
    let mut stream =
        crate::ipc::connect_local_stream(&crate::server::socket_paths::client_socket_path())?;
    let welcome = super::super::handshake::connect(&mut stream, options, true)?;
    let terminal_guard = client::setup_terminal(config.ui.mouse_capture)?;
    let reset_keys = terminal_guard.reset_modify_other_keys;
    let reset_colors = terminal_guard.reset_host_color_scheme_reports;
    #[cfg(windows)]
    let restore_input = terminal_guard.restore_windows_input_mode;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        client::restore_terminal_state(
            reset_keys,
            reset_colors,
            #[cfg(windows)]
            restore_input,
        );
        original_hook(info);
    }));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    let quit = Arc::new(AtomicBool::new(false));
    let signal_quit = quit.clone();
    let _ = ctrlc::set_handler(move || signal_quit.store(true, Ordering::Release));
    let (tx, rx) = mpsc::channel(256);
    let mouse_capture = Arc::new(AtomicBool::new(config.ui.mouse_capture));
    let pixel_mouse = Arc::new(AtomicBool::new(false));
    let input_quit = quit.clone();
    let input_tx = tx.clone();
    let input_mouse = mouse_capture.clone();
    let input_pixels = pixel_mouse.clone();
    #[cfg(unix)]
    let matcher = Arc::new(std::sync::Mutex::new(
        client::direct_graphics::ResponseMatcher::default(),
    ));
    #[cfg(unix)]
    let frontend_matcher = matcher.clone();
    #[cfg(unix)]
    let matcher_active = matcher
        .lock()
        .map(|matcher| matcher.active_handle())
        .unwrap_or_default();
    let query_theme = client::should_query_host_terminal_theme();
    std::thread::spawn(move || {
        client::input::stdin_reader_loop(
            input_tx,
            &input_quit,
            query_theme,
            input_mouse,
            input_pixels,
            #[cfg(unix)]
            matcher,
            #[cfg(unix)]
            matcher_active,
        )
    });
    if query_theme {
        client::query_host_terminal_theme();
    }
    let resize_quit = quit.clone();
    std::thread::spawn(move || {
        client::resize_poll_loop(
            tx,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            graphics,
            &resize_quit,
        )
    });
    let result = rt.block_on(async {
        let mut frontend = ClientFrontend::from_stream(
            stream,
            welcome,
            config,
            chrome.settings,
            cols,
            rows,
            options,
        )?;
        let legacy_groups = saved
            .as_ref()
            .map(|saved| saved.collapsed_space_keys.clone())
            .unwrap_or_default();
        frontend.chrome_preferences = chrome_preferences.unwrap_or_default();
        frontend.chrome.collapsed_groups = frontend
            .chrome_preferences
            .local_collapsed_groups(&legacy_groups)
            .into_iter()
            .map(|key| (ClientEndpointId::Local, key))
            .chain(
                frontend
                    .chrome_preferences
                    .remote_collapsed_groups
                    .iter()
                    .flat_map(|group| {
                        group.collapsed_groups.iter().map(|key| {
                            (ClientEndpointId::Ssh(group.profile_id.clone()), key.clone())
                        })
                    }),
            )
            .collect();
        frontend.chrome.collapsed_sections =
            if let Some(sections) = &frontend.chrome_preferences.collapsed_sections {
                sections
                    .iter()
                    .map(|entry| {
                        (
                            entry
                                .profile_id
                                .as_ref()
                                .map_or(ClientEndpointId::Local, |profile| {
                                    ClientEndpointId::Ssh(profile.clone())
                                }),
                            entry.section,
                        )
                    })
                    .collect()
            } else {
                saved
                    .as_ref()
                    .into_iter()
                    .flat_map(|saved| saved.collapsed_workspace_sections.iter())
                    .map(|section| (ClientEndpointId::Local, *section))
                    .collect()
            };
        frontend.chrome_preferences_path = Some(preferences_path);
        #[cfg(unix)]
        {
            frontend.direct.matcher = frontend_matcher;
        }
        frontend.host.mouse_capture = mouse_capture;
        frontend.host.pixel_mouse = pixel_mouse;
        frontend.event_loop(rx, quit.clone()).await
    });
    quit.store(true, Ordering::Release);
    drop(terminal_guard);
    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    result
}
