//! Settings interaction borrows the fork controller; terminal persistence stays server-owned.
use super::*;
use crate::app::state::{SelectionListState, SettingsSection, SettingsState, ThemeRuntimeConfig};
use crate::app::{SettingsAction, SettingsInput};
use crate::raw_input::RawInputEvent;
use crossterm::event::KeyEventKind;

pub(super) struct HostSettings {
    pub theme: ThemeRuntimeConfig,
    pub name: String,
    pub sound: bool,
    pub sound_config: crate::config::SoundConfig,
    pub toast: crate::config::ToastDelivery,
    pub labels: bool,
    pub prefix_ascii: bool,
    pub right_click_passthrough: Option<crossterm::event::KeyModifiers>,
}

impl HostSettings {
    pub(super) fn new(config: &crate::config::Config) -> Self {
        let theme = crate::app::theme_runtime_config(config, true);
        let (_, name) = crate::app::resolve_effective_theme(&theme, None);
        Self {
            theme,
            name,
            sound: config.ui.sound.enabled,
            sound_config: config.ui.sound.clone(),
            toast: config.ui.toast.delivery,
            labels: config.ui.show_agent_labels_on_pane_borders,
            prefix_ascii: config.experimental.switch_ascii_input_source_in_prefix,
            right_click_passthrough: config.ui.right_click_passthrough_modifiers(),
        }
    }
}

pub(super) struct Settings {
    pub(super) state: SettingsState,
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    pub(super) history: Option<bool>,
    request: Option<String>,
}

impl Settings {
    fn current(&self, frontend: &ClientFrontend) -> bool {
        frontend.runtime.shell.active_endpoint_id == self.endpoint
            && frontend
                .runtime
                .shell
                .endpoint(&self.endpoint)
                .is_some_and(|endpoint| {
                    endpoint
                        .cache
                        .live_snapshot(self.generation)
                        .is_some_and(|snapshot| snapshot.boot_id == self.boot)
                })
    }
}

fn borrowed<'a>(settings: &'a mut Settings, frontend: &'a mut ClientFrontend) -> SettingsInput<'a> {
    SettingsInput {
        settings: &mut settings.state,
        palette: &mut frontend.chrome.settings.palette,
        theme_name: &mut frontend.host_settings.name,
        theme_runtime: &frontend.host_settings.theme,
        sound: frontend.host_settings.sound,
        toast: frontend.host_settings.toast,
        pane_labels: frontend.host_settings.labels,
        // Experiments is not opened until this endpoint has supplied its actual fact.
        pane_history: settings.history.unwrap_or(false),
        prefix_ascii: frontend.host_settings.prefix_ascii,
        area: ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows),
        closed: false,
    }
}

pub(super) fn open(frontend: &mut ClientFrontend) -> io::Result<()> {
    if !frontend.runtime.input_lease_current() {
        return Ok(());
    }
    let endpoint = frontend.runtime.shell.active_endpoint_id.clone();
    let Some(connection) = frontend.runtime.endpoints.connection(&endpoint) else {
        return Ok(());
    };
    let generation = connection.generation;
    let history_supported = connection
        .negotiation
        .supports_method("server.pane_history.get");
    let Some((boot, _)) = frontend
        .runtime
        .shell
        .endpoint_snapshot_identity(&endpoint, generation)
    else {
        return Ok(());
    };
    let mut settings = Settings {
        state: SettingsState {
            section: SettingsSection::Theme,
            list: SelectionListState::new(0),
            original_palette: None,
            original_theme: None,
        },
        endpoint,
        generation,
        boot: boot.to_owned(),
        history: None,
        request: None,
    };
    borrowed(&mut settings, frontend).open(SettingsSection::Theme);
    if history_supported {
        match frontend.runtime.issue_method_with_id(
            crate::api::schema::Method::ServerPaneHistoryGet(
                crate::api::schema::EmptyParams::default(),
            ),
        ) {
            Ok((id, update)) => {
                settings.request = Some(id);
                frontend.update(update)?;
            }
            Err(error) => frontend.notice = Some(error),
        }
    }
    frontend.settings = Some(settings);
    Ok(())
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(settings) = &frontend.settings else {
        return;
    };
    if !settings.current(frontend) {
        return;
    }
    if settings.state.section == SettingsSection::Experiments && settings.history.is_none() {
        return;
    }
    crate::ui::render_settings_from(
        &crate::ui::SettingsRenderFacts {
            palette: &frontend.chrome.settings.palette,
            settings: &settings.state,
            theme_name: &frontend.host_settings.name,
            sound: frontend.host_settings.sound,
            toast: frontend.host_settings.toast,
            pane_labels: frontend.host_settings.labels,
            pane_history: settings.history.unwrap_or(false),
            prefix_ascii: frontend.host_settings.prefix_ascii,
        },
        frame,
        ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows),
    );
}

fn edit(content: &str, action: &SettingsAction) -> String {
    use crate::config::{upsert_section_bool as flag, upsert_section_value as value};
    match action {
        SettingsAction::SaveTheme(name) => flag(
            &value(content, "theme", "name", &format!("\"{name}\"")),
            "theme",
            "auto_switch",
            false,
        ),
        SettingsAction::SaveSound(enabled) => flag(content, "ui.sound", "enabled", *enabled),
        SettingsAction::SaveToastDelivery(delivery) => {
            let name = match delivery {
                crate::config::ToastDelivery::Off => "off",
                crate::config::ToastDelivery::Herdr => "herdr",
                crate::config::ToastDelivery::Terminal => "terminal",
                crate::config::ToastDelivery::System => "system",
            };
            crate::config::remove_section_key(
                &value(content, "ui.toast", "delivery", &format!("\"{name}\"")),
                "ui.toast",
                "enabled",
            )
        }
        SettingsAction::SaveAgentBorderLabels(enabled) => {
            flag(content, "ui", "show_agent_labels_on_pane_borders", *enabled)
        }
        SettingsAction::SaveSwitchAsciiInputSourceInPrefix(enabled) => flag(
            content,
            "experimental",
            "switch_ascii_input_source_in_prefix",
            *enabled,
        ),
        SettingsAction::SavePaneHistory(_) => content.to_owned(),
    }
}

fn save(
    frontend: &mut ClientFrontend,
    settings: &mut Settings,
    action: SettingsAction,
) -> io::Result<()> {
    if let SettingsAction::SavePaneHistory(enabled) = action {
        if !settings.current(frontend) {
            return Ok(());
        }
        match frontend.runtime.issue_method_with_id(
            crate::api::schema::Method::ServerPaneHistorySet(
                crate::api::schema::ServerPaneHistorySetParams { enabled },
            ),
        ) {
            Ok((id, update)) => {
                settings.request = Some(id);
                frontend.update(update)?;
            }
            Err(error) => frontend.notice = Some(error),
        }
        return Ok(());
    }
    let context = match &action {
        SettingsAction::SaveTheme(_) => "theme",
        SettingsAction::SaveSound(_) => "sound setting",
        SettingsAction::SaveToastDelivery(_) => "toast setting",
        SettingsAction::SaveAgentBorderLabels(_) => "agent border labels",
        SettingsAction::SaveSwitchAsciiInputSourceInPrefix(_) => "prefix ascii input source",
        SettingsAction::SavePaneHistory(_) => "pane screen history",
    };
    let path = crate::config::config_path();
    let result = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, edit(&content, &action))
    })();
    if let Err(error) = result {
        crate::logging::config_write_failed(&path, context, &error.to_string());
        frontend.notice = Some(format!("failed to save {context}: {error}"));
        return Ok(());
    }
    reload_host(frontend);
    Ok(())
}

fn reload_host(frontend: &mut ClientFrontend) -> crate::config::ConfigReloadReport {
    let report = match crate::config::load_live_config() {
        Ok(loaded) => {
            let mut diagnostics = loaded.diagnostics.clone();
            if !loaded
                .invalid_sections
                .iter()
                .any(|section| section == "keys")
            {
                frontend.remote_image_paste_key = loaded.config.remote_image_paste_key().unwrap_or_else(|diagnostic| {
                    tracing::warn!(%diagnostic, "local remote image paste key config diagnostic");
                    None
                });
                match loaded.config.live_keybinds_with_diagnostics() {
                    Ok((bindings, warnings)) => {
                        frontend.keybinds = bindings;
                        diagnostics.extend(warnings);
                    }
                    Err(warnings) => diagnostics.extend(
                        warnings
                            .into_iter()
                            .map(|warning| format!("{warning}; kept current keybinds")),
                    ),
                }
            }
            let valid_ui = !loaded
                .invalid_sections
                .iter()
                .any(|section| section == "ui")
                && loaded.config.invalid_sidebar_bounds_diagnostic().is_none();
            if let Some(diagnostic) = loaded.config.invalid_sidebar_bounds_diagnostic() {
                diagnostics.push(format!("{diagnostic}; keeping previous [ui] settings"));
            }
            if valid_ui {
                let config = &loaded.config;
                let settings = &mut frontend.chrome.settings;
                if settings.sidebar_width_source
                    == crate::app::state::SidebarWidthSource::ConfigDefault
                {
                    settings.sidebar_width = config.ui.sidebar_width;
                }
                settings.default_sidebar_width = config.ui.sidebar_width;
                settings.mouse_capture = config.ui.mouse_capture;
                settings.sidebar_min_width = config.ui.sidebar_min_width;
                settings.sidebar_max_width = config.ui.sidebar_max_width;
                settings.sidebar_width = settings
                    .sidebar_width
                    .clamp(settings.sidebar_min_width, settings.sidebar_max_width);
                settings.sidebar_collapsed_mode = config.ui.sidebar_collapsed_mode;
                settings.mobile_width_threshold = config.ui.mobile_width_threshold;
                settings.show_tab_bar = config.ui.show_tab_bar;
                settings.hide_single_tab = config.ui.hide_tab_bar_when_single_tab;
                settings.density = config.ui.workspace_panel_density;
                settings.spaces = config.ui.sidebar.spaces.clone();
                settings.agents = config.ui.sidebar.agents.clone();
                settings.mouse_scroll_lines = config.ui.mouse_scroll_lines();
                frontend.chrome.agent_scroll = 0;
                frontend.copy_on_select = config.ui.copy_on_select;
                if !frontend.copy_on_select {
                    selection::clear(frontend);
                }
                frontend.confirm_close = config.ui.confirm_close;
                frontend.prompt_new_tab_name = config.ui.prompt_new_tab_name;
                frontend.draw_host_cursor =
                    super::super::super::should_draw_host_cursor(config.ui.host_cursor);
                diagnostics.extend(config.ui.sound.diagnostics());
                if let Err(error) = frontend.host.set_preference(config.ui.mouse_capture) {
                    diagnostics.push(error.to_string());
                }
                frontend.options.mouse_capture = config.ui.mouse_capture;
                frontend
                    .runtime
                    .set_mouse_capture_preference(config.ui.mouse_capture);
            }
            if !loaded
                .invalid_sections
                .iter()
                .any(|section| section == "theme")
            {
                frontend.host_settings.theme =
                    crate::app::theme_runtime_config(&loaded.config, valid_ui);
                let (palette, name) = crate::app::resolve_effective_theme(
                    &frontend.host_settings.theme,
                    frontend
                        .host_theme
                        .background
                        .map(|color| color.inferred_appearance()),
                );
                frontend.chrome.settings.palette = palette;
                frontend.host_settings.name = name;
            }
            if valid_ui {
                frontend.host_settings.sound = loaded.config.ui.sound.enabled;
                frontend.host_settings.sound_config = loaded.config.ui.sound.clone();
                frontend.host_settings.toast = loaded.config.ui.toast.delivery;
                frontend.host_settings.labels = loaded.config.ui.show_agent_labels_on_pane_borders;
                frontend.host_settings.right_click_passthrough =
                    loaded.config.ui.right_click_passthrough_modifiers();
            }
            if !loaded
                .invalid_sections
                .iter()
                .any(|section| section == "experimental")
            {
                frontend.host_settings.prefix_ascii = loaded
                    .config
                    .experimental
                    .switch_ascii_input_source_in_prefix;
            }
            frontend.notice = crate::config::config_diagnostic_summary(&diagnostics);
            crate::config::ConfigReloadReport {
                status: if diagnostics.is_empty() {
                    crate::config::ConfigReloadStatus::Applied
                } else {
                    crate::config::ConfigReloadStatus::Partial
                },
                diagnostics,
            }
        }
        Err(diagnostics) => {
            frontend.notice = crate::config::config_diagnostic_summary(&diagnostics);
            crate::config::ConfigReloadReport {
                status: crate::config::ConfigReloadStatus::Failed,
                diagnostics,
            }
        }
    };
    frontend.force_redraw = true;
    report
}

pub(super) struct ReloadRequest {
    endpoint: ClientEndpointId,
    generation: u64,
    boot: String,
    request: String,
    host_report: crate::config::ConfigReloadReport,
}

pub(super) fn reload(frontend: &mut ClientFrontend) -> io::Result<()> {
    if !frontend.runtime.input_lease_current() {
        return Ok(());
    }
    let endpoint = frontend.runtime.shell.active_endpoint_id.clone();
    let Some(connection) = frontend.runtime.endpoints.connection(&endpoint) else {
        return Ok(());
    };
    let generation = connection.generation;
    let Some((boot, _)) = frontend
        .runtime
        .shell
        .endpoint_snapshot_identity(&endpoint, generation)
    else {
        return Ok(());
    };
    let boot = boot.to_owned();
    let host_report = reload_host(frontend);
    match frontend
        .runtime
        .issue_method_with_id(crate::api::schema::Method::ServerReloadConfig(
            crate::api::schema::EmptyParams::default(),
        )) {
        Ok((request, update)) => {
            frontend.reload_request = Some(ReloadRequest {
                endpoint,
                generation,
                boot,
                request,
                host_report,
            });
            frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

fn reload_completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) {
    let Some(pending) = frontend.reload_request.as_ref() else {
        return;
    };
    if pending.endpoint != result.endpoint_id
        || pending.generation != result.generation
        || pending.boot != result.boot_id
        || pending.request != result.request_id
    {
        return;
    }
    let Some(pending) = frontend.reload_request.take() else {
        return;
    };
    if frontend.runtime.shell.active_endpoint_id != pending.endpoint
        || frontend
            .runtime
            .shell
            .endpoint_snapshot_identity(&pending.endpoint, pending.generation)
            .is_none_or(|(boot, _)| boot != pending.boot)
    {
        return;
    }
    if let Ok(value) = &result.result {
        if value.get("type").and_then(serde_json::Value::as_str) != Some("config_reload") {
            return;
        }
        let mut diagnostics = pending.host_report.diagnostics;
        if let Some(items) = value
            .get("diagnostics")
            .and_then(serde_json::Value::as_array)
        {
            diagnostics.extend(
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned),
            );
        }
        frontend.notice = crate::config::config_diagnostic_summary(&diagnostics)
            .or_else(|| Some("reloaded config".to_owned()));
        frontend.force_redraw = true;
    }
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    result: &super::super::commands::EndpointCommandResult,
) {
    reload_completed(frontend, result);
    let Some(settings) = frontend.settings.as_mut() else {
        return;
    };
    if result.endpoint_id != settings.endpoint
        || result.generation != settings.generation
        || result.boot_id != settings.boot
        || settings.request.as_deref() != Some(&result.request_id)
    {
        return;
    }
    settings.request = None;
    if let Ok(value) = &result.result {
        if value.get("type").and_then(serde_json::Value::as_str) == Some("pane_history") {
            settings.history = value.get("enabled").and_then(serde_json::Value::as_bool);
        }
    }
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if frontend
        .settings
        .as_ref()
        .is_some_and(|settings| !settings.current(frontend))
    {
        if let Some(mut settings) = frontend.settings.take() {
            if let Some(palette) = settings.state.original_palette.take() {
                frontend.chrome.settings.palette = palette;
            }
            if let Some(name) = settings.state.original_theme.take() {
                frontend.host_settings.name = name;
            }
            frontend.force_redraw = true;
        }
    }
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
    let Some(mut settings) = frontend.settings.take() else {
        return Ok(false);
    };
    if !settings.current(frontend) {
        return Ok(false);
    }
    let (action, closed) = {
        let mut input = borrowed(&mut settings, frontend);
        let action = match event {
            RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                input.key(key.as_key_event())
            }
            RawInputEvent::Mouse(mouse) => input.mouse(*mouse),
            _ => None,
        };
        (action, input.closed)
    };
    if let Some(action) = action {
        save(frontend, &mut settings, action)?;
    }
    if !closed {
        frontend.settings = Some(settings);
    }
    Ok(true)
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<ratatui::layout::Rect> {
    let settings = frontend.settings.as_ref()?;
    if !settings.current(frontend)
        || settings.state.section == SettingsSection::Experiments && settings.history.is_none()
    {
        return None;
    }
    crate::ui::centered_popup_rect(
        ratatui::layout::Rect::new(0, 0, frontend.cols, frontend.rows),
        crate::ui::SETTINGS_POPUP_WIDTH,
        crate::ui::settings_popup_height(),
    )
}
