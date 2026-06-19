use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::AppState;

fn keybind_label(bindings: &crate::config::ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_string())
}

fn indexed_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        "unset".to_string()
    } else if bindings.len() == 9 {
        let first = &bindings[0].label;
        if first.ends_with('1') {
            format!("{}1..9", first.trim_end_matches('1'))
        } else {
            bindings
                .iter()
                .map(|binding| binding.label.clone())
                .collect::<Vec<_>>()
                .join(" / ")
        }
    } else {
        bindings
            .iter()
            .map(|binding| binding.label.clone())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

pub(super) fn keybind_help_groups(
    app: &AppState,
) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let kb = &app.keybinds;
    let mut groups = Vec::new();

    groups.push((
        "global",
        vec![
            (
                crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
                "prefix mode",
            ),
            (keybind_label(&kb.help), "keybinds"),
            (keybind_label(&kb.settings), "settings"),
            (keybind_label(&kb.detach), "detach"),
            (keybind_label(&kb.reload_config), "reload config"),
            (
                keybind_label(&kb.open_notification_target),
                "open notification target",
            ),
        ],
    ));

    groups.push((
        "navigation",
        vec![
            ("esc".to_string(), "back"),
            ("↑ / ↓".to_string(), "workspace list"),
            ("h j k l / arrows".to_string(), "move focus"),
            ("tab / shift+tab".to_string(), "cycle pane"),
            ("enter".to_string(), "open workspace"),
            ("1..9".to_string(), "switch workspace"),
        ],
    ));

    let workspace_tab = vec![
        (keybind_label(&kb.workspace_picker), "workspace navigation"),
        (keybind_label(&kb.new_workspace), "new workspace"),
        (keybind_label(&kb.rename_workspace), "rename workspace"),
        (keybind_label(&kb.close_workspace), "close workspace"),
        (keybind_label(&kb.previous_workspace), "previous workspace"),
        (keybind_label(&kb.next_workspace), "next workspace"),
        (indexed_label(&kb.switch_workspace), "switch workspace 1-9"),
        (keybind_label(&kb.previous_agent), "previous agent"),
        (keybind_label(&kb.next_agent), "next agent"),
        (indexed_label(&kb.focus_agent), "focus agent 1-9"),
        (keybind_label(&kb.new_tab), "new tab"),
        (keybind_label(&kb.rename_tab), "rename tab"),
        (keybind_label(&kb.previous_tab), "previous tab"),
        (keybind_label(&kb.next_tab), "next tab"),
        (indexed_label(&kb.switch_tab), "switch tab 1-9"),
        (keybind_label(&kb.close_tab), "close tab"),
    ];
    groups.push(("workspaces / tabs", workspace_tab));

    let panes = vec![
        (keybind_label(&kb.split_vertical), "split vertical"),
        (keybind_label(&kb.split_horizontal), "split horizontal"),
        (keybind_label(&kb.close_pane), "close pane"),
        (keybind_label(&kb.rename_pane), "rename pane"),
        (keybind_label(&kb.copy_mode), "copy mode"),
        (keybind_label(&kb.edit_scrollback), "edit scrollback"),
        (keybind_label(&kb.zoom), "zoom pane"),
        (keybind_label(&kb.resize_mode), "resize mode"),
        (keybind_label(&kb.toggle_sidebar), "toggle sidebar"),
        (keybind_label(&kb.focus_pane_left), "focus pane left"),
        (keybind_label(&kb.focus_pane_down), "focus pane down"),
        (keybind_label(&kb.focus_pane_up), "focus pane up"),
        (keybind_label(&kb.focus_pane_right), "focus pane right"),
        (keybind_label(&kb.cycle_pane_next), "cycle pane next"),
        (
            keybind_label(&kb.cycle_pane_previous),
            "cycle pane previous",
        ),
    ];
    groups.push(("panes", panes));

    if !kb.custom_commands.is_empty() {
        groups.push((
            "custom",
            kb.custom_commands
                .iter()
                .map(|binding| (binding.label.clone(), "custom command"))
                .collect(),
        ));
    }

    groups
}

pub(crate) fn keybind_help_lines(app: &AppState) -> Vec<(usize, Line<'static>)> {
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);

    let groups = keybind_help_groups(app);
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);

    let mut lines = Vec::new();

    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(vec![Span::styled(format!(" {group}"), heading_style)]),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {:<width$} ", key, width = key_width);
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(padded_key, key_style),
                    Span::styled(label.to_string(), label_style),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }

    lines
}

pub(super) fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    super::dim_background(frame, frame.area());

    let Some(inner) = render_modal_shell(frame, frame.area(), 76, 22, &app.palette) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(frame, header_rows[0], "keybinds", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(
        Paragraph::new(" available commands and configured shortcuts")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[1],
    );

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app
            .keybind_help_max_scroll()
            .saturating_sub(app.keybind_help.scroll) as usize,
        max_offset_from_bottom: app.keybind_help_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        keybind_help_lines(app)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((app.keybind_help.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("wheel ↑↓", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("jump", Style::default().fg(app.palette.overlay0)),
            Span::styled(" pgup / pgdn ", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close", Style::default().fg(app.palette.overlay0)),
            Span::styled(" esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}
