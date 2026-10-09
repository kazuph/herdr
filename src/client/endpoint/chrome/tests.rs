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
    // A frame for another geometry is kept visible, clipped and unscaled, without granting a
    // cursor or image placements for the new area.
    let mut resized = target.clone();
    resized.cursor = None;
    resized.graphics = Vec::new();
    assert!(!surface::compose(
        &mut resized,
        Rect::new(2, 1, 5, 2),
        &source
    ));
    assert_eq!(resized.cursor, None);
    assert!(resized.graphics.is_empty());
    for y in 0..2u16 {
        for x in 0..5u16 {
            let shown = &resized.cells
                [usize::from(1 + y) * usize::from(resized.width) + usize::from(2 + x)];
            let original =
                &source.cells[usize::from(y) * usize::from(source.width) + usize::from(x)];
            assert_eq!(shown.symbol, original.symbol);
        }
    }
}

#[test]
fn endpoint_selected_workspace_retains_original_accent_band() {
    for density in [
        crate::config::WorkspacePanelDensityConfig::Slim,
        crate::config::WorkspacePanelDensityConfig::Full,
    ] {
        let config = crate::config::Config::default();
        let mut settings = ChromeSettings::from_config(&config, Palette::catppuccin(), None);
        settings.density = density;
        let mut chrome = ClientChrome::new(settings);
        let snapshot: crate::protocol::endpoint_wire::ClientShellSnapshot =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
            )))
            .unwrap();
        let mut snapshot = crate::protocol::endpoint_projection::SnapshotJson::from(snapshot);
        for workspace in &snapshot.snapshot.workspaces {
            snapshot.workspace_facts.insert(
                workspace.workspace_id.clone(),
                crate::protocol::endpoint_projection::WorkspaceFacts {
                    git_space: None,
                    section: Some(crate::workspace::WorkspaceSection::ALL[0]),
                },
            );
        }
        let remote = ClientEndpointId::Ssh("band-owner".into());
        let mut shell = ClientShellState::new();
        shell.set_endpoint_catalog(&[crate::machine::MachineProfile {
            id: "band-owner".into(),
            label: "Remote".into(),
            target: "unused".into(),
            session: "owned".into(),
            enabled: true,
        }]);
        for id in [ClientEndpointId::Local, remote.clone()] {
            assert!(shell.begin_connection(&id, 1));
            assert!(shell.receive_snapshot(&id, 1, snapshot.clone()));
        }
        let workspace = snapshot.focused_workspace_id.clone().unwrap();
        for selected_endpoint in [ClientEndpointId::Local, remote.clone()] {
            chrome.navigate_selection = Some(ResourceKey {
                endpoint: selected_endpoint.clone(),
                id: workspace.clone(),
            });
            let view = chrome.compute_view(&shell, 160, 60);
            let frame = chrome.render(&view);
            assert_eq!(view.machine_headers.len(), 1);
            assert_eq!(view.machine_headers[0].1, "Remote");
            assert_eq!(view.section_headers.len(), 1);
            assert_eq!(
                view.section_headers[0].0.section,
                crate::workspace::WorkspaceSection::ALL[0]
            );
            let remote_new = view.hits.iter().find(|hit| matches!(&hit.target, ChromeTarget::NewWorkspaceInSection(id, crate::workspace::WorkspaceSection::None) if id == &remote)).unwrap();
            assert_eq!(remote_new.rect.y, view.machine_headers[0].0.y);
            assert!(!view.hits.iter().any(
                |hit| matches!(&hit.target, ChromeTarget::WorkspaceSection(id, _) if id == &remote)
            ));
            for hit in &view.hits {
                if let ChromeTarget::Machine(id) = &hit.target {
                    let text = (hit.rect.x..hit.rect.right())
                        .map(|x| {
                            frame.cells[usize::from(hit.rect.y) * usize::from(frame.width)
                                + usize::from(x)]
                            .symbol
                            .as_str()
                        })
                        .collect::<String>();
                    let label = if id.is_local() { "Local" } else { "Remote" };
                    assert!(text.contains(label), "machine header overwritten: {text:?}");
                }
            }
            let mut selected_cards = 0;
            for hit in &view.hits {
                let ChromeTarget::Workspace(key) = &hit.target else {
                    continue;
                };
                let selected = key.endpoint == selected_endpoint && key.id == workspace;
                selected_cards += usize::from(selected);
                for y in hit.rect.y..hit.rect.bottom() {
                    let cell = &frame.cells
                        [usize::from(y) * usize::from(frame.width) + usize::from(hit.rect.x)];
                    assert_eq!(cell.symbol == "▌", selected, "{key:?} at {y}");
                    if selected {
                        let mut expected = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 1, 1));
                        expected[(0, 0)].set_fg(chrome.settings.palette.accent);
                        let expected =
                            crate::protocol::FrameData::from_ratatui_buffer(&expected, None);
                        assert_eq!(cell.fg, expected.cells[0].fg);
                    }
                }
            }
            assert_eq!(selected_cards, 1);
        }
        chrome
            .collapsed_sections
            .insert((remote.clone(), crate::workspace::WorkspaceSection::ALL[0]));
        assert!(
            sidebar::visual_workspace_targets(&chrome, &shell, 26, false)
                .iter()
                .any(|key| key.endpoint == remote && key.id == workspace)
        );
        chrome.collapsed_machines.insert(remote.clone());
        let collapsed = chrome.compute_view(&shell, 160, 60);
        assert_eq!(collapsed.machine_headers.len(), 1);
        assert!(!collapsed.machine_headers[0].2);
        assert!(!collapsed.hits.iter().any(
            |hit| matches!(&hit.target, ChromeTarget::Workspace(key) if key.endpoint == remote)
        ));
        assert!(collapsed.hits.iter().any(
            |hit| matches!(&hit.target, ChromeTarget::NewWorkspaceInSection(id, _) if id == &remote)
        ));
    }
}

#[test]
fn endpoint_sidebar_width_button_renders_in_the_detail_footer_and_cycles_presets() {
    let config = crate::config::Config::default();
    let mut chrome = ClientChrome::new(ChromeSettings::from_config(
        &config,
        Palette::catppuccin(),
        None,
    ));
    let shell = ClientShellState::new();
    let label_at = |chrome: &mut ClientChrome| {
        let view = chrome.compute_view(&shell, 160, 50);
        let frame = chrome.render(&view);
        let hit = view
            .hits
            .iter()
            .find(|hit| matches!(hit.target, ChromeTarget::SidebarWidthToggle))
            .expect("sidebar width button hit target");
        assert_eq!(hit.rect.y, view.layout.sidebar.bottom() - 1);
        (hit.rect.x..hit.rect.right())
            .map(|x| {
                frame.cells[usize::from(hit.rect.y) * usize::from(frame.width) + usize::from(x)]
                    .symbol
                    .clone()
            })
            .collect::<String>()
    };

    chrome
        .settings
        .set_sidebar_width_preset(crate::app::state::SidebarWidthPreset::Normal);
    assert_eq!(label_at(&mut chrome).trim_end(), " NORMAL");
    chrome.settings.cycle_sidebar_width_preset();
    assert_eq!(
        chrome.settings.sidebar_width,
        chrome.settings.sidebar_max_width
    );
    assert_eq!(label_at(&mut chrome).trim_end(), " WIDE");
    chrome.settings.cycle_sidebar_width_preset();
    assert_eq!(
        chrome.settings.sidebar_width,
        chrome.settings.sidebar_min_width
    );
    assert_eq!(label_at(&mut chrome).trim_end(), " NARROW");
    chrome.settings.cycle_sidebar_width_preset();
    assert_eq!(
        chrome.settings.sidebar_width_source,
        crate::app::state::SidebarWidthSource::ConfigDefault
    );
    assert_eq!(label_at(&mut chrome).trim_end(), " NORMAL");
}

#[test]
fn endpoint_tab_bar_draws_fixed_width_chips_a_new_tab_button_and_scroll_arrows() {
    let config = crate::config::Config::default();
    let palette = Palette::catppuccin();
    let mut chrome = ClientChrome::new(ChromeSettings::from_config(&config, palette.clone(), None));
    let mut snapshot: crate::protocol::endpoint_wire::ClientShellSnapshot =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
    let workspace = snapshot.focused_workspace_id.clone().unwrap();
    let template = snapshot
        .tabs
        .iter()
        .find(|tab| tab.workspace_id == workspace)
        .unwrap()
        .clone();
    let tabs = |count: usize| {
        (0..count)
            .map(|index| crate::protocol::endpoint_wire::ClientShellTab {
                tab_id: format!("{workspace}:t{index}"),
                number: index + 1,
                label: if index == 1 {
                    "build".into()
                } else {
                    (index + 1).to_string()
                },
                custom_label: index == 1,
                focused: index == 1,
                ..template.clone()
            })
            .collect::<Vec<_>>()
    };
    snapshot.tabs.retain(|tab| tab.workspace_id != workspace);
    snapshot.tabs.extend(tabs(3));
    let mut shell = ClientShellState::new();
    shell.begin_connection(&ClientEndpointId::Local, 1);
    assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot.clone()));
    let view = chrome.compute_view(&shell, 160, 20);
    let tab_hits = view
        .hits
        .iter()
        .filter(|hit| matches!(hit.target, ChromeTarget::Tab(_)))
        .map(|hit| hit.rect)
        .collect::<Vec<_>>();
    assert_eq!(
        tab_hits.iter().map(|rect| rect.width).collect::<Vec<_>>(),
        vec![
            crate::ui::tab_chip_width("1"),
            crate::ui::tab_chip_width("build"),
            crate::ui::tab_chip_width("3"),
        ],
        "chips keep the in-process minimum width and padding"
    );
    let active = view
        .lines
        .iter()
        .find(|(rect, _)| *rect == tab_hits[1])
        .unwrap();
    assert_eq!(active.1.spans[0].style.bg, Some(palette.accent));
    let inactive = view
        .lines
        .iter()
        .find(|(rect, _)| *rect == tab_hits[0])
        .unwrap();
    assert_eq!(inactive.1.spans[0].style.bg, Some(palette.surface0));
    let new_tab = view
        .hits
        .iter()
        .find(|hit| matches!(hit.target, ChromeTarget::NewTab))
        .unwrap();
    assert_eq!(new_tab.rect.x, tab_hits[2].right());
    assert!(!view
        .hits
        .iter()
        .any(|hit| matches!(hit.target, ChromeTarget::TabScroll { .. })));

    snapshot.tabs.retain(|tab| tab.workspace_id != workspace);
    snapshot.tabs.extend(tabs(30));
    let mut shell = ClientShellState::new();
    shell.begin_connection(&ClientEndpointId::Local, 1);
    assert!(shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot));
    let view = chrome.compute_view(&shell, 80, 20);
    for right in [false, true] {
        assert!(view
            .hits
            .iter()
            .any(|hit| matches!(hit.target, ChromeTarget::TabScroll { right: r } if r == right)));
    }
    assert!(view
        .hits
        .iter()
        .any(|hit| matches!(hit.target, ChromeTarget::NewTab)));
}
