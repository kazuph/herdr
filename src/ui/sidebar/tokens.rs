use super::AgentPanelEntry;
use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SidebarTokenStyle, SpaceSidebarToken,
    SpacesSidebarConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToken {
    pub kind: ResolvedTokenKind,
    pub style: SidebarTokenStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedTokenKind {
    JobIndicators(Vec<(String, Option<i32>)>),
    StateIcon,
    WorkspaceNumber(usize),
    StateText(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus { ahead: usize, behind: usize },
    GitDiff { additions: usize, deletions: usize },
    Custom(String),
}

impl ResolvedToken {
    fn new(kind: ResolvedTokenKind, style: SidebarTokenStyle) -> Self {
        Self { kind, style }
    }

    #[cfg(test)]
    pub(super) fn unstyled(kind: ResolvedTokenKind) -> Self {
        Self::new(kind, SidebarTokenStyle::default())
    }
}

pub(super) fn agent_rows(
    config: &AgentsSidebarConfig,
    entry: &AgentPanelEntry,
    state_text: &str,
) -> Vec<Vec<ResolvedToken>> {
    agent_rows_from_context(
        config,
        AgentTokenContext {
            agent: entry.agent,
            workspace: &entry.primary_label,
            tab: entry.primary_tab_label.as_deref(),
            pane: entry.pane_label.as_deref(),
            agent_label: entry.agent_label.as_deref(),
            terminal_title: entry.terminal_title.as_deref(),
            terminal_title_stripped: entry.terminal_title_stripped.as_deref(),
            tokens: &entry.tokens,
            state_text,
        },
    )
}

/// Token facts have no runtime resource identity. Both client projections and
/// local server views use the same configured rows without inventing PaneIds.
pub(crate) struct AgentTokenContext<'a> {
    pub agent: Option<crate::detect::Agent>,
    pub workspace: &'a str,
    pub tab: Option<&'a str>,
    pub pane: Option<&'a str>,
    pub agent_label: Option<&'a str>,
    pub terminal_title: Option<&'a str>,
    pub terminal_title_stripped: Option<&'a str>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub state_text: &'a str,
}

pub(crate) fn agent_rows_from_context(
    config: &AgentsSidebarConfig,
    context: AgentTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(context.agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        AgentSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        AgentSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(context.state_text.to_string()))
                        }
                        AgentSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        AgentSidebarToken::Tab => context
                            .tab
                            .map(|value| ResolvedTokenKind::Tab(value.to_string())),
                        AgentSidebarToken::Pane => context
                            .pane
                            .map(|value| ResolvedTokenKind::Pane(value.to_string())),
                        AgentSidebarToken::Agent => context
                            .agent_label
                            .map(|value| ResolvedTokenKind::Agent(value.to_string())),
                        AgentSidebarToken::TerminalTitle => context
                            .terminal_title
                            .map(|value| ResolvedTokenKind::TerminalTitle(value.to_string())),
                        AgentSidebarToken::TerminalTitleStripped => context
                            .terminal_title_stripped
                            .map(|value| ResolvedTokenKind::TerminalTitle(value.to_string())),
                        AgentSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        AgentSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(crate) struct SpaceTokenContext<'a> {
    pub workspace_number: usize,
    pub workspace: &'a str,
    pub branch: Option<&'a str>,
    pub state_text: &'a str,
    pub ahead_behind: Option<(usize, usize)>,
    pub diff_stats: Option<(usize, usize)>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub suppress_git_details: bool,
}

pub(crate) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    let mut rows = config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        SpaceSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        SpaceSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(context.state_text.to_string()))
                        }
                        SpaceSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        SpaceSidebarToken::Branch if !context.suppress_git_details => {
                            Some(ResolvedTokenKind::Branch(
                                context.branch.unwrap_or("nogit").to_string(),
                            ))
                        }
                        SpaceSidebarToken::Branch => None,
                        SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                            .ahead_behind
                            .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                            .map(|(ahead, behind)| ResolvedTokenKind::GitStatus { ahead, behind }),
                        SpaceSidebarToken::GitStatus => None,
                        SpaceSidebarToken::GitDiff if !context.suppress_git_details => context
                            .diff_stats
                            .filter(|(additions, deletions)| *additions > 0 || *deletions > 0)
                            .map(|(additions, deletions)| ResolvedTokenKind::GitDiff {
                                additions,
                                deletions,
                            }),
                        SpaceSidebarToken::GitDiff => None,
                        SpaceSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        SpaceSidebarToken::Styled { .. } => None,
                    }?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect::<Vec<_>>();

    if let Some(row) = rows.iter_mut().find(|row| {
        row.iter()
            .any(|token| matches!(token.kind, ResolvedTokenKind::Workspace(_)))
    }) {
        let workspace_index = row
            .iter()
            .position(|token| matches!(token.kind, ResolvedTokenKind::Workspace(_)))
            .expect("workspace row was selected");
        let number_index = row[..workspace_index]
            .iter()
            .rposition(|token| matches!(token.kind, ResolvedTokenKind::StateIcon))
            .map_or(workspace_index, |index| index + 1);
        row.insert(
            number_index,
            ResolvedToken::new(
                ResolvedTokenKind::WorkspaceNumber(context.workspace_number),
                SidebarTokenStyle::default(),
            ),
        );
    }

    rows
}

/// How long a finished job keeps its space-card indicator, in unix ms.
pub(crate) const JOB_INDICATOR_FINISHED_RETENTION_MS: u128 = 60_000;

/// Space-card dots only mark jobs that still mean something: a queued job, a
/// `running`/`cancelling` job whose runner process was verified alive off the
/// render path, or a finished job inside its retention window. `runner_alive`
/// is `None` when nothing verified the runner (no pid, or a server that does
/// not report liveness).
pub(crate) fn job_indicator_visible(
    status: &str,
    runner_alive: Option<bool>,
    finished_unix_ms: Option<u128>,
    now_unix_ms: u128,
) -> bool {
    match status {
        "running" | "cancelling" => runner_alive == Some(true),
        "queued" => true,
        _ => finished_unix_ms.is_some_and(|finished| {
            now_unix_ms.saturating_sub(finished) < JOB_INDICATOR_FINISHED_RETENTION_MS
        }),
    }
}

pub(crate) fn with_job_indicators(
    mut rows: Vec<Vec<ResolvedToken>>,
    jobs: Vec<(String, Option<i32>)>,
    display_height: usize,
) -> Vec<Vec<ResolvedToken>> {
    if jobs.is_empty() || display_height == 0 {
        return rows;
    }
    let visible = rows.len().min(display_height);
    let position = rows[..visible]
        .iter()
        .enumerate()
        .find_map(|(row, tokens)| {
            tokens
                .iter()
                .position(|token| {
                    matches!(
                        token.kind,
                        ResolvedTokenKind::Branch(_)
                            | ResolvedTokenKind::GitStatus { .. }
                            | ResolvedTokenKind::GitDiff { .. }
                    )
                })
                .map(|column| (row, column))
        })
        .or_else(|| {
            rows[..visible]
                .iter()
                .enumerate()
                .find_map(|(row, tokens)| {
                    tokens
                        .iter()
                        .position(|token| matches!(token.kind, ResolvedTokenKind::Workspace(_)))
                        .map(|column| (row, column))
                })
        })
        .unwrap_or((0, 0));
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows[position.0].insert(
        position.1,
        ResolvedToken::new(
            ResolvedTokenKind::JobIndicators(jobs),
            SidebarTokenStyle::default(),
        ),
    );
    rows
}

pub(super) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    if matches!(
        previous.kind,
        ResolvedTokenKind::StateIcon | ResolvedTokenKind::WorkspaceNumber(_)
    ) || matches!(current.kind, ResolvedTokenKind::WorkspaceNumber(_))
        || matches!(
            current.kind,
            ResolvedTokenKind::GitStatus { .. } | ResolvedTokenKind::GitDiff { .. }
        )
    {
        " "
    } else {
        " · "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentSidebarToken, SpaceSidebarToken};
    use crate::detect::AgentState;

    fn entry() -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            primary_label: "repo".into(),
            primary_tab_label: None,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: Some("pi".into()),
            agent_kind_label: Some("pi".into()),
            agent: Some(crate::detect::Agent::Pi),
            state: AgentState::Working,
            seen: true,
            last_agent_state_change_seq: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn missing_custom_tokens_elide_rows_and_separators() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Custom("missing".into()),
                ],
                vec![AgentSidebarToken::Custom("missing".into())],
                vec![AgentSidebarToken::Agent],
            ],
            ..Default::default()
        };

        let rows = agent_rows(&config, &entry, "working");

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::StateIcon)]
        );
        assert_eq!(
            rows[1],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "pi".into()
            ))]
        );
    }

    #[test]
    fn state_text_and_arbitrary_values_are_independent_tokens() {
        let mut entry = entry();
        entry
            .tokens
            .insert("summary".into(), "reviewing auth".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Custom("summary".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "deep in the mines"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateText("deep in the mines".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("reviewing auth".into())),
            ]]
        );
    }

    #[test]
    fn terminal_title_builtins_are_distinct_from_custom_tokens() {
        let mut entry = entry();
        entry.terminal_title = Some("⠋ raw title".into());
        entry.terminal_title_stripped = Some("raw title".into());
        entry
            .tokens
            .insert("terminal_title".into(), "custom title".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, &entry, "working"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("⠋ raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("custom title".into())),
            ]]
        );
    }

    #[test]
    fn known_agent_override_replaces_default_rows() {
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Workspace]],
            ..Default::default()
        };
        config
            .rows_by_agent
            .insert("pi".into(), vec![vec![AgentSidebarToken::Agent]]);
        let mut pi = entry();
        pi.agent_label = Some("renamed pi".into());

        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "renamed pi".into()
            ))]]
        );

        pi.agent = None;
        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );
    }

    #[test]
    fn grouped_children_suppress_all_builtin_git_details() {
        let config = SpacesSidebarConfig::default();

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace_number: 1,
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "idle",
                    ahead_behind: Some((2, 1)),
                    diff_stats: Some((4, 3)),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                },
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::WorkspaceNumber(1)),
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("feature".into())),
            ]]
        );
    }

    #[test]
    fn workspace_number_uses_plain_spaces_between_state_and_name() {
        let state = ResolvedToken::unstyled(ResolvedTokenKind::StateIcon);
        let number = ResolvedToken::unstyled(ResolvedTokenKind::WorkspaceNumber(1));
        let workspace = ResolvedToken::unstyled(ResolvedTokenKind::Workspace("repo".to_string()));

        assert_eq!(separator(&state, &number), " ");
        assert_eq!(separator(&number, &workspace), " ");
    }

    #[test]
    fn default_space_rows_include_worktree_diff_stats() {
        let rows = space_rows(
            &SpacesSidebarConfig::default(),
            SpaceTokenContext {
                workspace_number: 1,
                workspace: "repo",
                branch: Some("main"),
                state_text: "idle",
                ahead_behind: None,
                diff_stats: Some((7, 2)),
                tokens: &std::collections::HashMap::new(),
                suppress_git_details: false,
            },
        );

        assert_eq!(
            rows[1],
            vec![
                ResolvedToken::unstyled(ResolvedTokenKind::Branch("main".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::GitDiff {
                    additions: 7,
                    deletions: 2,
                }),
            ]
        );
    }

    #[test]
    fn default_space_rows_render_nogit_without_a_branch() {
        let rows = space_rows(
            &SpacesSidebarConfig::default(),
            SpaceTokenContext {
                workspace_number: 1,
                workspace: "shell",
                branch: None,
                state_text: "idle",
                ahead_behind: None,
                diff_stats: None,
                tokens: &std::collections::HashMap::new(),
                suppress_git_details: false,
            },
        );

        assert_eq!(
            rows[1],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::Branch(
                "nogit".into()
            ))]
        );
    }

    #[test]
    fn workspace_custom_token_can_replace_git_specific_details() {
        let tokens = std::collections::HashMap::from([("jj_status".into(), "2 changes".into())]);
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Custom("jj_status".into())]],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace_number: 1,
                    workspace: "repo",
                    branch: None,
                    state_text: "idle",
                    ahead_behind: None,
                    diff_stats: None,
                    tokens: &tokens,
                    suppress_git_details: false,
                },
            ),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Custom(
                "2 changes".into()
            ))]]
        );
    }
}
