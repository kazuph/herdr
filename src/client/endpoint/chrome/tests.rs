use super::*;

#[test]
fn endpoint_chrome_agent_navigation_uses_rendered_order_and_keeps_target_visible() {
    let config = crate::config::Config::default();
    let mut chrome = ClientChrome::new(ChromeSettings::from_config(
        &config,
        Palette::catppuccin(),
        None,
    ));
    let mut snapshot: crate::protocol::endpoint_wire::ClientShellSnapshot =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
    let pane = snapshot.panes[0].clone();
    let agent = snapshot.agents[0].clone();
    snapshot.panes.clear();
    snapshot.agents.clear();
    snapshot.agent_order.clear();
    for index in 0..3 {
        let id = format!("opaque agent {index}");
        snapshot
            .panes
            .push(crate::protocol::endpoint_wire::ClientShellPane {
                pane_id: id.clone(),
                ..pane.clone()
            });
        snapshot
            .agents
            .push(crate::protocol::endpoint_wire::ClientShellAgent {
                pane_id: id.clone(),
                ..agent.clone()
            });
        snapshot.agent_order.push(id);
    }
    let mut shell = ClientShellState::new();
    shell.begin_connection(&ClientEndpointId::Local, 1);
    assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot));
    let view = chrome.compute_view(&shell, 160, 20);
    let targets = chrome.agent_targets(&shell, view.detail_body.width);
    assert_eq!(
        targets
            .iter()
            .map(|target| target.id.as_str())
            .collect::<Vec<_>>(),
        vec!["opaque agent 0", "opaque agent 1", "opaque agent 2"]
    );
    chrome.ensure_agent_visible(&shell, view.detail_body, &targets[2]);
    let view = chrome.compute_view(&shell, 160, 20);
    let hit = view
        .hits
        .iter()
        .find(|hit| matches!(&hit.target, ChromeTarget::Agent(key) if key == &targets[2]))
        .unwrap();
    assert!(hit.rect.y >= view.detail_body.y);
    assert!(hit.rect.bottom() <= view.detail_body.bottom());
    chrome.ensure_agent_visible(&shell, view.detail_body, &targets[0]);
    assert_eq!(chrome.agent_scroll, 0);
}

#[test]
fn endpoint_chrome_fork_geometry_including_action_bar_matches_existing_ui_for_all_tiny_sizes() {
    let config = crate::config::Config::default();
    for collapsed in [false, true] {
        for hidden in [false, true] {
            for single_hidden in [false, true] {
                let mut settings =
                    ChromeSettings::from_config(&config, Palette::catppuccin(), None);
                settings.sidebar_collapsed = collapsed;
                settings.sidebar_collapsed_mode = if hidden {
                    SidebarCollapsedModeConfig::Hidden
                } else {
                    SidebarCollapsedModeConfig::Compact
                };
                settings.hide_single_tab = single_hidden;
                let mut app = crate::app::AppState::test_new();
                app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
                app.active = Some(0);
                app.selected = 0;
                app.mode = crate::app::Mode::Terminal;
                app.sidebar_width = settings.sidebar_width;
                app.sidebar_min_width = settings.sidebar_min_width;
                app.sidebar_max_width = settings.sidebar_max_width;
                app.sidebar_collapsed = collapsed;
                app.sidebar_collapsed_mode = settings.sidebar_collapsed_mode;
                app.mobile_width_threshold = settings.mobile_width_threshold;
                app.hide_tab_bar_when_single_tab = single_hidden;
                for cols in 0..=80 {
                    for rows in 0..=20 {
                        let area = Rect::new(0, 0, cols, rows);
                        crate::ui::compute_view(&mut app, area);
                        let layout = layout::compute(&settings, cols, rows, 1, true);
                        assert_eq!(layout.pane_surface, app.view.terminal_area, "{cols}x{rows} collapsed{collapsed} hidden{hidden} single{single_hidden}");
                        assert_eq!(layout.tab_bar, app.view.tab_bar_rect);
                        assert_eq!(layout.pane_actions, app.view.pane_action_bar_rect);
                        assert!(layout.pane_surface.right() <= area.right());
                        assert!(layout.pane_surface.bottom() <= area.bottom());
                    }
                }
            }
        }
    }
}

#[test]
fn endpoint_chrome_composition_keeps_exact_cells_hyperlinks_cursor_and_unicode_without_changing_wire(
) {
    let area = Rect::new(2, 1, 6, 2);
    let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 6, 2));
    buffer.set_string(0, 0, "漢字👩🏽‍💻", Style::default());
    let source = crate::protocol::endpoint_wire::FrameData::from_ratatui_buffer_with_hyperlinks(
        &buffer,
        Some(crate::protocol::endpoint_wire::CursorState {
            x: 2,
            y: 1,
            visible: true,
            shape: 6,
        }),
        &[((0, 0), "漢".into(), "https://example.org/opaque".into())],
    );
    let mut target = crate::protocol::FrameData::from_ratatui_buffer(
        &ratatui::buffer::Buffer::empty(Rect::new(0, 0, 10, 4)),
        None,
    );
    assert!(surface::compose(&mut target, area, &source));
    assert_eq!(target.cells.len(), 40);
    for y in 0..2 {
        for x in 0..6 {
            let actual = &target.cells[(y + 1) * 10 + x + 2];
            let original = &source.cells[y * 6 + x];
            assert_eq!(
                (
                    &actual.symbol,
                    actual.fg,
                    actual.bg,
                    actual.modifier,
                    actual.skip
                ),
                (
                    &original.symbol,
                    original.fg,
                    original.bg,
                    original.modifier,
                    original.skip
                )
            );
        }
    }
    assert_eq!(
        target.cursor,
        Some(crate::protocol::CursorState {
            x: 4,
            y: 2,
            visible: true,
            shape: 6
        })
    );
    assert_eq!(target.hyperlinks, source.hyperlinks);
    assert_eq!(target.cells[12].hyperlink, Some(0));
    let before = target.clone();
    assert!(!surface::compose(
        &mut target,
        Rect::new(2, 1, 5, 2),
        &source
    ));
    assert_eq!(target, before);
}
