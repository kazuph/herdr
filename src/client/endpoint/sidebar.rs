//! Configured sidebar tokens from one endpoint's opaque resource projection.
//! No local workspace, PaneId, caller resolution, or JobStore is constructed.

use super::{ClientEndpointId, ResourceKey};
use crate::api::schema::AgentStatus;
use crate::config::{AgentsSidebarConfig, SpacesSidebarConfig};
use crate::detect::AgentState;
use crate::protocol::endpoint_jobs::EndpointJobsProjection;
use crate::protocol::endpoint_wire::{ClientShellAgent, ClientShellSnapshot, ClientShellWorkspace};
use crate::ui::sidebar::tokens::{self, AgentTokenContext, ResolvedToken, SpaceTokenContext};

pub(crate) struct SidebarProjection<'a> {
    pub(crate) endpoint: &'a ClientEndpointId,
    pub(crate) snapshot: &'a ClientShellSnapshot,
    pub(crate) jobs: Option<&'a EndpointJobsProjection>,
}

pub(crate) fn state(status: AgentStatus) -> (AgentState, bool) {
    match status {
        AgentStatus::Idle => (AgentState::Idle, true),
        AgentStatus::Working => (AgentState::Working, true),
        AgentStatus::Blocked => (AgentState::Blocked, true),
        AgentStatus::Done => (AgentState::Idle, false),
        AgentStatus::Unknown => (AgentState::Unknown, true),
    }
}

impl SidebarProjection<'_> {
    pub(crate) fn key(&self, id: &str) -> ResourceKey {
        ResourceKey {
            endpoint: self.endpoint.clone(),
            id: id.to_owned(),
        }
    }

    pub(crate) fn workspace_jobs(
        &self,
        workspace_id: &str,
        now_unix_ms: u128,
    ) -> Option<Vec<(String, Option<i32>)>> {
        let projection = self.jobs.filter(|jobs| {
            jobs.boot_id == self.snapshot.boot_id && jobs.revision <= self.snapshot.revision
        })?;
        Some(
            projection
                .jobs
                .iter()
                .filter(|job| job.workspace_id.as_deref() == Some(workspace_id))
                .filter(|job| {
                    tokens::job_indicator_visible(
                        &job.status,
                        job.runner_alive,
                        job.finished_unix_ms,
                        now_unix_ms,
                    )
                })
                .map(|job| (job.status.clone(), job.exit_code))
                .collect(),
        )
    }

    pub(crate) fn workspace_rows(
        &self,
        config: &SpacesSidebarConfig,
        workspace: &ClientShellWorkspace,
        diff_stats: Option<(usize, usize)>,
        indented: bool,
        display_height: usize,
        now_unix_ms: u128,
    ) -> Vec<Vec<ResolvedToken>> {
        let (state, seen) = state(workspace.agent_status);
        let label = if indented {
            crate::ui::sidebar::grouped_child_display_label(
                &workspace.label,
                workspace.branch.as_deref(),
                workspace.custom_label,
            )
        } else {
            workspace.label.clone()
        };
        let token_values = workspace.tokens.iter().cloned().collect();
        let rows = tokens::space_rows(
            config,
            SpaceTokenContext {
                workspace_number: workspace.number,
                workspace: &label,
                branch: workspace.branch.as_deref(),
                state_text: crate::ui::state_label(state, seen),
                ahead_behind: workspace.git_ahead_behind,
                diff_stats,
                tokens: &token_values,
                suppress_git_details: indented,
            },
        );
        // A server without job facts contributes no invented badges. Caller
        // strings are retained facts; only the owning server resolves membership.
        match self.workspace_jobs(&workspace.workspace_id, now_unix_ms) {
            Some(jobs) => tokens::with_job_indicators(rows, jobs, display_height),
            None => rows,
        }
    }

    pub(crate) fn agent_rows(
        &self,
        config: &AgentsSidebarConfig,
        agent: &ClientShellAgent,
    ) -> Option<Vec<Vec<ResolvedToken>>> {
        let workspace = self
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
        let tab =
            self.snapshot.tabs.iter().find(|tab| {
                tab.tab_id == agent.tab_id && tab.workspace_id == workspace.workspace_id
            })?;
        let pane = self.snapshot.panes.iter().find(|pane| {
            pane.pane_id == agent.pane_id
                && pane.tab_id == tab.tab_id
                && pane.workspace_id == workspace.workspace_id
        })?;
        let multi_tab = self
            .snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace.workspace_id)
            .count()
            > 1;
        let (state, seen) = state(agent.agent_status);
        let status_key = crate::ui::agent_panel_status_key(state, seen);
        let state_text = agent
            .state_labels
            .iter()
            .find(|(key, _)| key == status_key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| crate::ui::state_label(state, seen));
        let token_values = agent.tokens.iter().cloned().collect();
        Some(tokens::agent_rows_from_context(
            config,
            AgentTokenContext {
                agent: agent
                    .agent
                    .as_deref()
                    .and_then(crate::detect::parse_agent_label),
                workspace: &workspace.label,
                tab: (multi_tab || tab.custom_label).then_some(tab.label.as_str()),
                pane: pane.label.as_deref(),
                agent_label: agent
                    .display_agent
                    .as_deref()
                    .or(agent.name.as_deref())
                    .or(agent.agent.as_deref()),
                terminal_title: agent.terminal_title.as_deref(),
                terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
                tokens: &token_values,
                state_text,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint_jobs::EndpointJob;
    use crate::ui::sidebar::tokens::ResolvedTokenKind;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .expect("frozen gen1 projection")
    }

    #[test]
    fn endpoint_sidebar_opaque_identity_and_absent_job_facts_do_not_federate_callers() {
        let snapshot = snapshot();
        let local_id = ClientEndpointId::Local;
        let remote_id = ClientEndpointId::Ssh("machine-with-the-same-resource-ids".into());
        let local = SidebarProjection {
            endpoint: &local_id,
            snapshot: &snapshot,
            jobs: None,
        };
        let remote = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &snapshot,
            jobs: None,
        };
        assert_ne!(local.key("w1:p1"), remote.key("w1:p1"));
        assert_eq!(remote.key("w1:p1").id, "w1:p1");
        let now = 1_000_000;
        assert!(remote.workspace_jobs("w1", now).is_none());
        let record = crate::job::JobRecord {
            id: "job".into(),
            label: "job".into(),
            command: "true".into(),
            cwd: "/remote-only".into(),
            caller_pane: "w1:p1".into(),
            caller_agent: "codex".into(),
            completion: "none".into(),
            status: "running".into(),
            runner_pid: Some(4321),
            exit_code: None,
            started_unix_ms: None,
            finished_unix_ms: None,
            log_path: "/remote-log".into(),
        };
        let mut jobs = EndpointJobsProjection {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            jobs: vec![EndpointJob::from_record(&record, None, Some(true))],
        };
        let projection = SidebarProjection {
            jobs: Some(&jobs),
            ..remote
        };
        assert_eq!(projection.workspace_jobs("w1", now), Some(Vec::new()));
        jobs.jobs[0].workspace_id = Some("different opaque workspace".into());
        let projection = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &snapshot,
            jobs: Some(&jobs),
        };
        assert_eq!(projection.workspace_jobs("w1", now), Some(Vec::new()));
        jobs.jobs[0].workspace_id = Some("w1".into());
        let projection = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &snapshot,
            jobs: Some(&jobs),
        };
        assert_eq!(
            projection.workspace_jobs("w1", now),
            Some(vec![("running".into(), None)])
        );
        let mut newer_snapshot = snapshot.clone();
        newer_snapshot.revision += 1;
        let updating = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &newer_snapshot,
            jobs: Some(&jobs),
        };
        let config = crate::config::Config::default();
        assert_eq!(
            projection.workspace_rows(
                &config.ui.sidebar.spaces,
                &snapshot.workspaces[0],
                None,
                false,
                2,
                now
            ),
            updating.workspace_rows(
                &config.ui.sidebar.spaces,
                &newer_snapshot.workspaces[0],
                None,
                false,
                2,
                now
            ),
            "snapshot arrival must not remove job marks or shift the branch row"
        );
        newer_snapshot.boot_id.push_str("-new-server");
        assert!(SidebarProjection {
            endpoint: &remote_id,
            snapshot: &newer_snapshot,
            jobs: Some(&jobs),
        }
        .workspace_jobs("w1", now)
        .is_none());
        jobs.jobs[0].runner_alive = Some(false);
        let projection = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &snapshot,
            jobs: Some(&jobs),
        };
        assert_eq!(projection.workspace_jobs("w1", now), Some(Vec::new()));
        jobs.jobs[0].runner_alive = Some(true);
        jobs.revision += 1;
        let projection = SidebarProjection {
            endpoint: &remote_id,
            snapshot: &snapshot,
            jobs: Some(&jobs),
        };
        assert!(projection.workspace_jobs("w1", now).is_none());
    }

    #[test]
    fn endpoint_sidebar_uses_shared_job_colors_clipping_and_custom_agent_rows() {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].label = "👩🏽‍💻 日本語".into();
        snapshot.agents[0].terminal_title_stripped = Some("👨‍👩‍👧‍👦".into());
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.spaces]
rows = [["state_icon", "workspace"], ["branch", "git_status"]]
[ui.sidebar.agents]
rows = [["workspace", "state_text", "terminal_title_stripped", "$task"]]
"#,
        )
        .unwrap();
        let endpoint = ClientEndpointId::Ssh("opaque-machine".into());
        let projection = SidebarProjection {
            endpoint: &endpoint,
            snapshot: &snapshot,
            jobs: None,
        };
        let agent_rows = projection
            .agent_rows(&config.ui.sidebar.agents, &snapshot.agents[0])
            .unwrap();
        assert!(agent_rows[0]
            .iter()
            .any(|token| token.kind == ResolvedTokenKind::StateText("waiting".into())));
        assert!(agent_rows[0]
            .iter()
            .any(|token| token.kind == ResolvedTokenKind::TerminalTitle("👨‍👩‍👧‍👦".into())));
        let mut rows = projection.workspace_rows(
            &config.ui.sidebar.spaces,
            &snapshot.workspaces[0],
            Some((2, 3)),
            false,
            2,
            1_000_000,
        );
        rows = tokens::with_job_indicators(
            rows,
            vec![
                ("running".into(), None),
                ("queued".into(), None),
                ("exited".into(), Some(0)),
                ("exited".into(), Some(7)),
            ],
            2,
        );
        let palette = crate::app::state::Palette::catppuccin();
        for width in 0..=80 {
            for row in &rows {
                let spans = crate::ui::sidebar::clip_token_spans(
                    crate::ui::sidebar::resolved_token_spans(
                        row,
                        ("◆", Style::default()),
                        Style::default(),
                        Style::default(),
                        Style::default(),
                        Style::default(),
                        &palette,
                        width,
                    ),
                    width,
                );
                let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
                assert!(text.width() <= width, "{width}: {text}");
                let colors = spans
                    .iter()
                    .filter(|span| span.content == "●")
                    .map(|span| span.style.fg)
                    .collect::<Vec<_>>();
                let expected = [palette.yellow, palette.overlay0, palette.green, palette.red];
                assert_eq!(
                    colors,
                    expected[..colors.len()]
                        .iter()
                        .copied()
                        .map(Some)
                        .collect::<Vec<_>>()
                );
            }
        }
        snapshot.panes[0].workspace_id = "foreign workspace".into();
        let projection = SidebarProjection {
            endpoint: &endpoint,
            snapshot: &snapshot,
            jobs: None,
        };
        assert!(projection
            .agent_rows(&config.ui.sidebar.agents, &snapshot.agents[0])
            .is_none());
    }
}
