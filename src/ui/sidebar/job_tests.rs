use super::*;
use crate::config::SidebarTokenStyle;
use crate::workspace::Workspace;
use ratatui::{backend::TestBackend, Terminal};

fn job(caller: String, status: &str, exit_code: Option<i32>) -> crate::job::JobRecord {
    let mut job = crate::job::JobRecord {
        id: status.into(),
        label: status.into(),
        command: "true".into(),
        cwd: "/unrelated".into(),
        caller_pane: caller,
        caller_agent: "same-agent".into(),
        completion: "none".into(),
        status: status.into(),
        runner_pid: None,
        exit_code,
        started_unix_ms: None,
        finished_unix_ms: None,
        log_path: String::new(),
    };
    match status {
        "running" | "cancelling" => job.runner_pid = Some(std::process::id()),
        "queued" => {}
        _ => job.finished_unix_ms = Some(now_unix_ms()),
    }
    job
}

fn spans(tokens: &[ResolvedToken], width: usize, p: &Palette) -> Vec<Span<'static>> {
    let styled = Style::default()
        .fg(p.mauve)
        .add_modifier(Modifier::BOLD | Modifier::DIM);
    resolved_token_spans(
        tokens,
        ("●", styled),
        styled,
        styled,
        styled,
        styled,
        p,
        width,
    )
}

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

#[test]
fn endpoint_and_local_jobs_share_exact_labels_colors_age_and_caller_width() {
    let p = AppState::test_new().palette;
    for (status, exit) in [
        ("running", None),
        ("queued", None),
        ("exited", Some(0)),
        ("exited", Some(7)),
        ("cancelled", None),
        ("future", None),
    ] {
        let mut local = job("opaque:caller-日本語".into(), status, exit);
        local.label = "日本語 👩🏽‍💻 e\u{301}".into();
        local.started_unix_ms = Some(1_000);
        local.finished_unix_ms = Some(10_000);
        let endpoint = crate::protocol::endpoint_jobs::EndpointJob::from_record(
            &local,
            Some("opaque:owner".into()),
            None,
        );
        let local_line = job_panel_line((&local).into(), 70_000, &p);
        assert_eq!(local_line, job_panel_line((&endpoint).into(), 70_000, &p));
        assert_eq!(
            local_line.spans[0].style.fg,
            Some(job_status_color(status, exit, &p))
        );
        assert!(local_line.spans.last().unwrap().content.contains("👩🏽‍💻"));
        local.label.clear();
        let endpoint = crate::protocol::endpoint_jobs::EndpointJob::from_record(&local, None, None);
        assert_eq!(
            job_panel_line((&local).into(), 70_000, &p),
            job_panel_line((&endpoint).into(), 70_000, &p)
        );
    }
}

#[test]
fn workspace_job_indicators_keep_only_meaningful_states() {
    let mut app = AppState::test_new();
    app.workspaces = vec![Workspace::test_new("one")];
    let caller = format!("p{}", app.workspaces[0].tabs[0].root_pane.raw());
    let now = now_unix_ms();
    let retention = tokens::JOB_INDICATOR_FINISHED_RETENTION_MS;
    let dead_pid = 4_294_000_000u32;

    let mut fresh_exit = job(caller.clone(), "exited", Some(0));
    fresh_exit.finished_unix_ms = Some(now.saturating_sub(retention - 1));
    let mut boundary_exit = job(caller.clone(), "exited", Some(0));
    boundary_exit.finished_unix_ms = Some(now.saturating_sub(retention));
    let mut stale_exit = job(caller.clone(), "exited", Some(1));
    stale_exit.finished_unix_ms = Some(now.saturating_sub(retention + 1_000));
    let mut undated_exit = job(caller.clone(), "exited", Some(0));
    undated_exit.finished_unix_ms = None;
    let mut dead_runner = job(caller.clone(), "running", None);
    dead_runner.runner_pid = Some(dead_pid);
    let mut dead_cancelling = job(caller.clone(), "cancelling", None);
    dead_cancelling.runner_pid = Some(dead_pid);
    let mut missing_pid = job(caller.clone(), "running", None);
    missing_pid.runner_pid = None;

    app.jobs = vec![
        job(caller.clone(), "queued", None),
        job(caller, "running", None),
        fresh_exit,
        boundary_exit,
        stale_exit,
        undated_exit,
        dead_runner,
        dead_cancelling,
        missing_pid,
    ];
    app.dead_runner_pids = [dead_pid].into_iter().collect();

    assert_eq!(
        workspace_jobs(&app, &app.workspaces[0], now),
        vec![
            ("queued".to_string(), None),
            ("running".to_string(), None),
            ("exited".to_string(), Some(0)),
        ],
        "queued, verified-live running, and recently finished jobs are the only dots"
    );
    assert_eq!(app.jobs.len(), 9, "the jobs list itself is unchanged");
}

#[test]
fn job_priority_clipping_preserves_complete_display_graphemes_and_styles() {
    let p = AppState::test_new().palette;
    let style = Style::default()
        .fg(p.mauve)
        .add_modifier(Modifier::BOLD | Modifier::DIM);
    for grapheme in ["👨‍👩‍👧‍👦", "👩🏽‍💻", "🇯🇵", "e\u{301}", "日"]
    {
        let width = display_width(grapheme);
        let input = Span::styled(grapheme, style);
        assert_eq!(clip_token_spans(vec![input.clone()], width), vec![input]);
        let longer = Span::styled(format!("{grapheme}tail"), style);
        let clipped = clip_token_spans(vec![longer], width);
        assert_eq!(text(&clipped), grapheme);
        assert_eq!(clipped[0].style, style);
        assert!(clip_token_spans(vec![Span::raw(grapheme)], width - 1).is_empty());
        let tokens = vec![
            ResolvedToken::unstyled(ResolvedTokenKind::JobIndicators(vec![(
                "running".into(),
                None,
            )])),
            ResolvedToken::unstyled(ResolvedTokenKind::Custom(grapheme.into())),
        ];
        let output = spans(&tokens, display_width("● ") + width, &p);
        assert_eq!(text(&output), format!("● {grapheme}"));
        assert_eq!(output[0].style.fg, Some(p.yellow));
        assert_eq!(display_width(&text(&output)), display_width("● ") + width);
    }
}

#[test]
fn job_indicators_preserve_status_order_colors_and_physical_width_before_git() {
    let p = AppState::test_new().palette;
    let cases = [
        ("running", None, p.yellow),
        ("running", Some(0), p.yellow),
        ("queued", Some(1), p.overlay0),
        ("exited", Some(0), p.green),
        ("exited", Some(7), p.red),
        ("exited", None, p.red),
        ("cancelling", None, p.red),
        ("cancelled", None, p.red),
    ];
    let tokens = vec![
        ResolvedToken::unstyled(ResolvedTokenKind::JobIndicators(
            cases.iter().map(|(s, c, _)| (s.to_string(), *c)).collect(),
        )),
        ResolvedToken {
            kind: ResolvedTokenKind::Branch("日本語-long-main".into()),
            style: SidebarTokenStyle {
                fg: Some(serde_json::from_str("\"#abcdef\"").unwrap()),
                bold: Some(true),
                dim: Some(true),
            },
        },
        ResolvedToken::unstyled(ResolvedTokenKind::GitStatus {
            ahead: usize::MAX,
            behind: usize::MAX,
        }),
        ResolvedToken::unstyled(ResolvedTokenKind::GitDiff {
            additions: usize::MAX,
            deletions: usize::MAX,
        }),
    ];
    let glyph_width = display_width("●");
    for width in 0..=cases.len() * glyph_width + 90 {
        let output = spans(&tokens, width, &p);
        assert!(
            output
                .iter()
                .map(|span| display_width(&span.content))
                .sum::<usize>()
                <= width
        );
        let indicators: Vec<_> = output.iter().filter(|span| span.content == "●").collect();
        assert_eq!(
            indicators.len(),
            cases.len().min(width / glyph_width),
            "width={width}"
        );
        for (index, indicator) in indicators.iter().enumerate() {
            assert_eq!(indicator.style.fg, Some(cases[index].2));
            assert_eq!(indicator.style.add_modifier, Modifier::empty());
        }
        assert!(text(&output).starts_with(&"●".repeat(indicators.len())));
    }
    let simple = vec![
        tokens[0].clone(),
        ResolvedToken::unstyled(ResolvedTokenKind::Branch("main".into())),
    ];
    assert_eq!(
        text(&spans(&simple, cases.len() + 5, &p)),
        format!("{} main", "●".repeat(cases.len()))
    );
    let mut changed_palette = p;
    std::mem::swap(&mut changed_palette.yellow, &mut changed_palette.green);
    let output = spans(&simple, cases.len(), &changed_palette);
    assert_eq!(output[0].style.fg, Some(changed_palette.yellow));
    assert_eq!(output[3].style.fg, Some(changed_palette.green));
}

#[test]
fn job_row_composition_handles_hidden_git_group_custom_empty_and_duplicate_tokens() {
    let indicator = vec![("running".into(), None)];
    let workspace = ResolvedToken::unstyled(ResolvedTokenKind::Workspace("repo".into()));
    let branch = ResolvedToken::unstyled(ResolvedTokenKind::Branch("main".into()));
    let rows = vec![
        vec![workspace.clone()],
        vec![ResolvedToken::unstyled(ResolvedTokenKind::StateText(
            "idle".into(),
        ))],
        vec![branch.clone(), branch.clone()],
    ];
    for height in [1, 2, 3] {
        let composed = tokens::with_job_indicators(rows.clone(), indicator.clone(), height);
        assert_eq!(composed.len(), rows.len());
        assert_eq!(
            composed
                .iter()
                .flatten()
                .filter(|token| matches!(token.kind, ResolvedTokenKind::JobIndicators(_)))
                .count(),
            1
        );
        let target = if height == 3 { 2 } else { 0 };
        assert!(matches!(
            composed[target][0].kind,
            ResolvedTokenKind::JobIndicators(_)
        ));
    }
    assert_eq!(tokens::with_job_indicators(rows.clone(), vec![], 3), rows);
    let empty = tokens::with_job_indicators(vec![], indicator.clone(), 1);
    assert_eq!(empty.len(), 1);
    let custom = tokens::with_job_indicators(
        vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Custom(
            "value".into(),
        ))]],
        indicator.clone(),
        1,
    );
    assert!(matches!(
        custom[0][0].kind,
        ResolvedTokenKind::JobIndicators(_)
    ));
    let grouped = tokens::with_job_indicators(
        vec![vec![
            ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
            ResolvedToken::unstyled(ResolvedTokenKind::WorkspaceNumber(3)),
            workspace,
        ]],
        indicator,
        1,
    );
    let p = AppState::test_new().palette;
    assert_eq!(text(&spans(&grouped[0], 20, &p)), "● 3 ● repo");
    for width in 0..20 {
        assert!(display_width(&text(&spans(&grouped[0], width, &p))) <= width);
    }
}

#[test]
fn job_membership_uses_live_aliases_all_tabs_and_follows_move_and_close() {
    let mut app = AppState::test_new();
    app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
    let second_tab = Workspace::test_new("tab").tabs.remove(0);
    let pane = second_tab.root_pane;
    app.workspaces[0].tabs.push(second_tab);
    app.public_pane_id_aliases.insert("restored".into(), pane);
    app.pane_id_aliases.insert(900001, pane);
    app.jobs = [
        "restored".to_string(),
        "p_900001".into(),
        format!("p{}", pane.raw()),
        "p999999".into(),
        String::new(),
    ]
    .into_iter()
    .map(|caller| job(caller, "running", None))
    .collect();
    app.jobs_scroll = app.jobs.len() - 1;
    assert_eq!(
        workspace_jobs(&app, &app.workspaces[0], now_unix_ms()).len(),
        3
    );
    assert!(workspace_jobs(&app, &app.workspaces[1], now_unix_ms()).is_empty());
    let moved = app.workspaces[0].tabs.pop().unwrap();
    app.workspaces[1].tabs.push(moved);
    assert!(workspace_jobs(&app, &app.workspaces[0], now_unix_ms()).is_empty());
    assert_eq!(
        workspace_jobs(&app, &app.workspaces[1], now_unix_ms()).len(),
        3
    );
    app.workspaces.swap(0, 1);
    assert_eq!(
        workspace_jobs(&app, &app.workspaces[0], now_unix_ms()).len(),
        3
    );
    app.workspaces[0].tabs.pop();
    assert!(workspace_jobs(&app, &app.workspaces[0], now_unix_ms()).is_empty());
    assert_eq!(app.jobs.len(), 5);
}

#[test]
fn space_job_cells_preserve_geometry_and_render_is_pure() {
    let mut app = AppState::test_new();
    app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
    app.workspaces[0].cached_git_branch = Some("main".into());
    let caller = format!("p{}", app.workspaces[0].tabs[0].root_pane.raw());
    app.jobs = vec![
        job(caller.clone(), "running", None),
        job(caller, "exited", Some(0)),
    ];
    app.active = Some(0);
    app.ensure_test_terminals();
    let area = Rect::new(0, 0, 35, 30);
    for density in [WorkspacePanelDensity::Full, WorkspacePanelDensity::Slim] {
        app.workspace_panel_density = density;
        let (geometry, headers) = compute_workspace_list_areas(&app, area);
        app.view.workspace_card_areas = geometry;
        app.view.workspace_section_header_areas = headers;
        let before_jobs = serde_json::to_value(&app.jobs).unwrap();
        let before_scroll = app.jobs_scroll;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(&app, &TerminalRuntimeRegistry::new(), frame, area, true)
            })
            .unwrap();
        assert_eq!(serde_json::to_value(&app.jobs).unwrap(), before_jobs);
        assert_eq!(app.jobs_scroll, before_scroll);
        let card = &app.view.workspace_card_areas[0];
        let y = card.rect.y + 1;
        let x = card.rect.x + 3;
        let buffer = terminal.backend().buffer();
        for (offset, color) in [app.palette.yellow, app.palette.green]
            .into_iter()
            .enumerate()
        {
            let cell = &buffer[(x + offset as u16, y)];
            assert_eq!(cell.symbol(), "●");
            assert_eq!(cell.fg, color);
            assert_eq!(cell.modifier, Modifier::empty());
        }
        assert_eq!(buffer[(x + 2, y)].symbol(), " ");
        assert_eq!(buffer[(x + 3, y)].symbol(), "m");
        assert_eq!(
            workspace_row_height(&app, &app.workspaces[1], false),
            if density == WorkspacePanelDensity::Full {
                3
            } else {
                2
            }
        );
    }
}

#[test]
fn grouped_child_jobs_hide_with_the_child_without_aggregating_into_parent() {
    let mut app = AppState::test_new();
    app.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("child")];
    for (index, ws) in app.workspaces.iter_mut().enumerate() {
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "job-group".into(),
            label: "repo".into(),
            repo_root: "/repo/root".into(),
            checkout_path: if index == 0 {
                "/repo/root".into()
            } else {
                "/repo/child".into()
            },
            is_linked_worktree: index != 0,
        });
    }
    app.jobs = vec![job(
        format!("p{}", app.workspaces[1].tabs[0].root_pane.raw()),
        "running",
        None,
    )];
    app.ensure_test_terminals();
    let area = Rect::new(0, 0, 35, 40);
    for collapsed in [false, true, false] {
        if collapsed {
            app.collapsed_space_keys.insert("job-group".into());
        } else {
            app.collapsed_space_keys.remove("job-group");
        }
        let (cards, headers) = compute_workspace_list_areas(&app, area);
        app.view.workspace_card_areas = cards;
        app.view.workspace_section_header_areas = headers;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(&app, &TerminalRuntimeRegistry::new(), frame, area, false)
            })
            .unwrap();
        let glyphs: Vec<_> = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .collect();
        assert_eq!(glyphs.len(), usize::from(!collapsed));
        assert!(workspace_jobs(&app, &app.workspaces[0], now_unix_ms()).is_empty());
        assert_eq!(app.jobs.len(), 1);
    }
}
