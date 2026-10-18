//! "Copied N lines" after this screen writes a selection to the clipboard. Like the
//! server-drawn screen, it covers the sidebar's last row for a short time.
use super::*;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

pub(super) struct CopyFeedback {
    message: String,
    until: Instant,
}

/// Shows the line count of `text`, unless clipboard toasts are turned off.
pub(super) fn show(frontend: &mut ClientFrontend, text: &str, now: Instant) {
    if !frontend.host_settings.clipboard.enabled {
        frontend.copy_feedback = None;
        return;
    }
    let lines = text.lines().count();
    let noun = if lines == 1 { "line" } else { "lines" };
    frontend.copy_feedback = Some(CopyFeedback {
        message: format!("Copied {lines} {noun}"),
        until: now + crate::app::COPY_FEEDBACK_DURATION,
    });
}

/// Clears the message once its time is up. Returns true when it was cleared.
pub(super) fn tick(frontend: &mut ClientFrontend, now: Instant) -> bool {
    if frontend
        .copy_feedback
        .as_ref()
        .is_some_and(|shown| now >= shown.until)
    {
        frontend.copy_feedback = None;
        return true;
    }
    false
}

pub(super) fn render(
    frontend: &ClientFrontend,
    frame: &mut ratatui::Frame,
    view: &super::super::chrome::ChromeView,
) {
    let Some(shown) = &frontend.copy_feedback else {
        return;
    };
    let sidebar = view.layout.sidebar;
    // The sidebar's last column is its separator.
    let width = sidebar.width.saturating_sub(1);
    if frontend.chrome.settings.sidebar_collapsed || width == 0 || sidebar.height == 0 {
        return;
    }
    let palette = &frontend.chrome.settings.palette;
    let label: String = shown.message.chars().take(usize::from(width)).collect();
    frame.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default()
                .fg(palette.green)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(sidebar.x, sidebar.bottom() - 1, width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontend() -> ClientFrontend {
        let config = crate::config::Config::default();
        let snapshot: wire::ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .expect("snapshot fixture");
        let mut shell = ClientShellState::new();
        shell.begin_connection(&ClientEndpointId::Local, 3);
        shell.receive_snapshot(&ClientEndpointId::Local, 3, snapshot);
        let options = EndpointConnectOptions {
            surface_size: wire::ClientSurfaceSize {
                cols: 100,
                rows: 30,
            },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: false,
        };
        let runtime = EndpointRuntime::new(
            shell,
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        ClientFrontend::from_runtime(
            runtime,
            &config,
            ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None),
            (100, 30),
            options,
        )
    }

    fn screen_rows(frontend: &mut ClientFrontend) -> Vec<String> {
        let view =
            frontend
                .chrome
                .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
        let sidebar = view.layout.sidebar;
        let frame = frontend
            .chrome
            .render_with_overlay(&view, |frame| render(frontend, frame, &view));
        let row = |y: u16| {
            (0..frame.width)
                .map(|x| {
                    frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)]
                        .symbol
                        .clone()
                })
                .collect::<String>()
        };
        let mut rows: Vec<String> = (0..frame.height).map(row).collect();
        // The sidebar's last row comes first so tests can look at it directly.
        rows.insert(0, row(sidebar.bottom() - 1));
        rows
    }

    #[test]
    fn copy_feedback_counts_lines_and_expires() {
        let mut frontend = frontend();
        let now = Instant::now();
        show(&mut frontend, "alpha\nbeta\ngamma\n", now);
        assert_eq!(
            frontend
                .copy_feedback
                .as_ref()
                .map(|shown| shown.message.as_str()),
            Some("Copied 3 lines")
        );
        assert!(!tick(&mut frontend, now));
        assert!(tick(
            &mut frontend,
            now + crate::app::COPY_FEEDBACK_DURATION
        ));
        assert!(frontend.copy_feedback.is_none());
    }

    #[test]
    fn copy_feedback_says_line_for_a_single_line() {
        let mut frontend = frontend();
        show(&mut frontend, "alpha", Instant::now());
        assert_eq!(
            frontend
                .copy_feedback
                .as_ref()
                .map(|shown| shown.message.as_str()),
            Some("Copied 1 line")
        );
    }

    #[test]
    fn copy_feedback_stays_hidden_when_clipboard_toasts_are_off() {
        let mut frontend = frontend();
        frontend.host_settings.clipboard.enabled = false;
        show(&mut frontend, "alpha\n", Instant::now());
        assert!(frontend.copy_feedback.is_none());
    }

    #[test]
    fn copy_feedback_draws_only_in_the_sidebar_corner() {
        let mut frontend = frontend();
        let before = screen_rows(&mut frontend);
        assert!(!before.iter().any(|row| row.contains("Copied")));

        show(&mut frontend, "alpha\nbeta\ngamma\n", Instant::now());
        let after = screen_rows(&mut frontend);
        assert!(
            after[0].starts_with("Copied 3 lines"),
            "sidebar corner: {:?}",
            after[0]
        );
        assert_eq!(
            after[1..]
                .iter()
                .filter(|row| row.contains("Copied"))
                .count(),
            1,
            "only the sidebar corner shows the message"
        );
    }
}
