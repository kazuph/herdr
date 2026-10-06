//! Client-owned chrome draws projections without constructing local runtime resources.
use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::shell::ClientShellState;
use super::{ClientEndpointId, ClientEndpointStatus, ResourceKey};
use crate::app::state::Palette;
use crate::config::{
    AgentsSidebarConfig, SidebarCollapsedModeConfig, SpacesSidebarConfig,
    WorkspacePanelDensityConfig,
};

mod layout;
mod sidebar;
mod surface;

#[cfg(test)]
mod tests;

pub(crate) struct ChromeSettings {
    pub(crate) sidebar_width: u16,
    pub(crate) default_sidebar_width: u16,
    pub(crate) sidebar_width_source: crate::app::state::SidebarWidthSource,
    pub(crate) sidebar_min_width: u16,
    pub(crate) sidebar_max_width: u16,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) sidebar_collapsed_mode: SidebarCollapsedModeConfig,
    pub(crate) sidebar_section_split: f32,
    pub(crate) mobile_width_threshold: u16,
    pub(crate) show_tab_bar: bool,
    pub(crate) hide_single_tab: bool,
    pub(crate) density: WorkspacePanelDensityConfig,
    pub(crate) spaces: SpacesSidebarConfig,
    pub(crate) agents: AgentsSidebarConfig,
    pub(crate) palette: Palette,
    pub(crate) mouse_scroll_lines: usize,
    pub(crate) mouse_capture: bool,
}

impl ChromeSettings {
    pub(crate) fn from_config(
        config: &crate::config::Config,
        palette: Palette,
        saved: Option<&crate::persist::SessionSnapshot>,
    ) -> Self {
        let ui = &config.ui;
        let defaults = crate::config::Config::default().ui;
        let (min, max) =
            crate::config::validated_sidebar_bounds(ui.sidebar_min_width, ui.sidebar_max_width)
                .unwrap_or((defaults.sidebar_min_width, defaults.sidebar_max_width));
        Self {
            default_sidebar_width: ui.sidebar_width.clamp(min, max),
            sidebar_width_source: if saved.and_then(|saved| saved.sidebar_width).is_some() {
                crate::app::state::SidebarWidthSource::Persisted
            } else {
                crate::app::state::SidebarWidthSource::ConfigDefault
            },
            sidebar_width: saved
                .and_then(|saved| saved.sidebar_width)
                .unwrap_or(ui.sidebar_width)
                .clamp(min, max),
            sidebar_min_width: min,
            sidebar_max_width: max,
            sidebar_collapsed: ui.sidebar_start_collapsed,
            sidebar_collapsed_mode: ui.sidebar_collapsed_mode,
            // Existing App::new session preference, read without restore or PTY creation.
            sidebar_section_split: saved
                .and_then(|saved| saved.sidebar_section_split)
                .unwrap_or(0.5),
            mobile_width_threshold: ui.mobile_width_threshold,
            show_tab_bar: ui.show_tab_bar,
            hide_single_tab: ui.hide_tab_bar_when_single_tab,
            density: ui.workspace_panel_density,
            spaces: ui.sidebar.spaces.clone(),
            agents: ui.sidebar.agents.clone(),
            palette,
            mouse_scroll_lines: ui.mouse_scroll_lines(),
            mouse_capture: ui.mouse_capture,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeLayout {
    pub(crate) area: Rect,
    pub(crate) sidebar: Rect,
    pub(crate) tab_bar: Rect,
    pub(crate) pane_surface: Rect,
    pub(crate) pane_actions: Rect,
    pub(crate) mobile_header: Rect,
}

#[derive(Clone, Debug)]
pub(crate) enum ChromeTarget {
    GlobalMenu,
    NewWorkspace,
    WorkspaceSection(ClientEndpointId, crate::workspace::WorkspaceSection),
    NewWorkspaceInSection(ClientEndpointId, crate::workspace::WorkspaceSection),
    Machine(ClientEndpointId),
    Workspace(ResourceKey),
    WorkspaceGroup(ResourceKey),
    Agent(ResourceKey),
    Tab(ResourceKey),
    DetailTab(crate::app::state::SidebarDetailView),
    Job(ResourceKey),
}

pub(crate) struct ChromeHit {
    pub(crate) rect: Rect,
    pub(crate) target: ChromeTarget,
}

pub(crate) struct ChromeView {
    mobile_header_facts: Option<crate::ui::MobileHeaderFacts>,
    pub(crate) menu_launcher: Rect,
    pub(crate) layout: ChromeLayout,
    pub(crate) hits: Vec<ChromeHit>,
    pub(crate) workspace_max_scroll: usize,
    pub(crate) agent_max_scroll: usize,
    pub(crate) workspace_body: Rect,
    pub(crate) detail_body: Rect,
    pub(crate) lines: Vec<(Rect, Line<'static>)>,
    pub(crate) backgrounds: Vec<(Rect, Style)>,
    pub(crate) surface: Option<crate::protocol::endpoint_wire::PaneSurfaceFrame>,
    pub(crate) section_headers: Vec<(crate::app::state::WorkspaceSectionHeaderArea, bool)>,
    pub(crate) pane_actions: crate::ui::PaneActionBarRects,
}

pub(crate) struct ClientChrome {
    pub(crate) settings: ChromeSettings,
    pub(crate) collapsed_machines: HashSet<ClientEndpointId>,
    pub(crate) collapsed_groups: HashSet<(ClientEndpointId, String)>,
    pub(crate) last_workspace_selection: Option<ResourceKey>,
    pub(crate) navigate_selection: Option<ResourceKey>,
    pub(crate) collapsed_sections:
        std::collections::BTreeSet<(ClientEndpointId, crate::workspace::WorkspaceSection)>,
    pub(crate) workspace_scroll: usize,
    pub(crate) agent_scroll: usize,
    pub(crate) tab_scroll: usize,
    pub(crate) spinner_tick: u32,
    pub(crate) detail_view: crate::app::state::SidebarDetailView,
    pub(crate) jobs_scroll: usize,
}

impl ClientChrome {
    pub(crate) fn visual_workspace_targets(
        &self,
        shell: &ClientShellState,
        cols: u16,
    ) -> Vec<ResourceKey> {
        sidebar::visual_workspace_targets(
            self,
            shell,
            self.settings.sidebar_width,
            cols > 0 && cols <= self.settings.mobile_width_threshold,
        )
    }

    pub(crate) fn agent_targets(&self, shell: &ClientShellState, width: u16) -> Vec<ResourceKey> {
        sidebar::agent_targets(self, shell, width)
    }

    pub(crate) fn ensure_agent_visible(
        &mut self,
        shell: &ClientShellState,
        area: Rect,
        key: &ResourceKey,
    ) {
        sidebar::ensure_agent_visible(self, shell, area, key);
    }

    pub(crate) fn new(settings: ChromeSettings) -> Self {
        Self {
            settings,
            collapsed_machines: HashSet::new(),
            collapsed_groups: HashSet::new(),
            last_workspace_selection: None,
            navigate_selection: None,
            collapsed_sections: Default::default(),
            workspace_scroll: 0,
            agent_scroll: 0,
            tab_scroll: 0,
            spinner_tick: 0,
            detail_view: crate::app::state::SidebarDetailView::default(),
            jobs_scroll: 0,
        }
    }

    pub(crate) fn compute_view(
        &mut self,
        shell: &ClientShellState,
        cols: u16,
        rows: u16,
    ) -> ChromeView {
        let snapshot = shell
            .endpoint(&shell.active_endpoint_id)
            .and_then(|endpoint| endpoint.cache.snapshot());
        let tabs = snapshot
            .map(|snapshot| {
                snapshot
                    .tabs
                    .iter()
                    .filter(|tab| Some(&tab.workspace_id) == snapshot.focused_workspace_id.as_ref())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let layout = layout::compute(
            &self.settings,
            cols,
            rows,
            tabs.len(),
            snapshot.is_some_and(|snapshot| snapshot.focused_workspace_id.is_some()),
        );
        let copy_label = crate::ui::pane_action_copy_label(false, false);
        let mut view = ChromeView {
            mobile_header_facts: None,
            menu_launcher: Rect::default(),
            layout,
            hits: Vec::new(),
            workspace_max_scroll: 0,
            agent_max_scroll: 0,
            workspace_body: Rect::default(),
            detail_body: Rect::default(),
            lines: Vec::new(),
            backgrounds: Vec::new(),
            section_headers: Vec::new(),
            surface: shell.pane_surface.clone(),
            pane_actions: crate::ui::pane_action_bar_rects(
                layout.pane_actions,
                copy_label.len() as u16,
            ),
        };
        sidebar::compute(self, shell, &mut view);
        let mut x = layout.tab_bar.x;
        for tab in tabs.into_iter().skip(self.tab_scroll) {
            if x >= layout.tab_bar.right() || layout.tab_bar.is_empty() {
                break;
            }
            let label = if tab.zoomed {
                format!("{} Z", tab.label)
            } else {
                tab.label.clone()
            };
            // Existing fork ui/tabs.rs minimum and label padding.
            let width = u16::try_from(unicode_width::UnicodeWidthStr::width(label.as_str()))
                .unwrap_or(u16::MAX)
                .saturating_add(4)
                .max(8)
                .min(layout.tab_bar.right().saturating_sub(x));
            let rect = Rect::new(x, layout.tab_bar.y, width, 1);
            let style = Style::default()
                .fg(if tab.focused {
                    self.settings.palette.text
                } else {
                    self.settings.palette.overlay0
                })
                .add_modifier(if tab.focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            view.lines
                .push((rect, Line::from(Span::styled(format!(" {label} "), style))));
            view.hits.push(ChromeHit {
                rect,
                target: ChromeTarget::Tab(ResourceKey {
                    endpoint: shell.active_endpoint_id.clone(),
                    id: tab.tab_id.clone(),
                }),
            });
            x = x.saturating_add(width).saturating_add(1);
        }
        if !layout.mobile_header.is_empty() {
            let workspace = snapshot.and_then(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| {
                        Some(&workspace.workspace_id) == snapshot.focused_workspace_id.as_ref()
                    })
                    .map(|workspace| {
                        let (state, seen) = super::sidebar::state(workspace.agent_status);
                        let (dot, dot_style) = crate::ui::state_summary_icon(
                            state,
                            seen,
                            self.spinner_tick,
                            &self.settings.palette,
                        );
                        let tabs = snapshot
                            .tabs
                            .iter()
                            .filter(|tab| tab.workspace_id == workspace.workspace_id)
                            .collect::<Vec<_>>();
                        let tab = tabs
                            .iter()
                            .enumerate()
                            .find(|(_, tab)| tab.tab_id == workspace.active_tab_id)
                            .map(|(index, tab)| {
                                if tabs.len() <= 1 {
                                    format!("tab {}", tab.label)
                                } else {
                                    format!("tab {} · {}/{}", tab.label, index + 1, tabs.len())
                                }
                            })
                            .unwrap_or_default();
                        crate::ui::MobileHeaderWorkspace {
                            name: workspace.label.clone(),
                            tab,
                            dot: dot.to_string(),
                            dot_style,
                        }
                    })
            });
            let mut counts = crate::ui::GlobalAgentCounts::default();
            for agent in shell
                .endpoints
                .iter()
                .filter_map(|endpoint| endpoint.cache.snapshot())
                .flat_map(|snapshot| &snapshot.agents)
            {
                let (state, seen) = super::sidebar::state(agent.agent_status);
                match (state, seen) {
                    (crate::detect::AgentState::Blocked, _) => counts.blocked += 1,
                    (crate::detect::AgentState::Idle, false) => counts.done += 1,
                    (crate::detect::AgentState::Working, _) => counts.working += 1,
                    (crate::detect::AgentState::Idle, true) => counts.idle += 1,
                    (crate::detect::AgentState::Unknown, _) => {}
                }
            }
            view.mobile_header_facts = Some(crate::ui::MobileHeaderFacts { workspace, counts });
            view.menu_launcher = crate::ui::mobile_header_hit_areas(layout.mobile_header).menu;
            view.hits.push(ChromeHit {
                rect: view.menu_launcher,
                target: ChromeTarget::GlobalMenu,
            });
        }
        view
    }

    pub(crate) fn render(&self, view: &ChromeView) -> crate::protocol::FrameData {
        self.render_with_overlay(view, |_| {})
    }

    pub(crate) fn render_with_overlay(
        &self,
        view: &ChromeView,
        overlay: impl FnOnce(&mut ratatui::Frame),
    ) -> crate::protocol::FrameData {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
            view.layout.area.width,
            view.layout.area.height,
        ))
        .expect("in-memory terminal creation is infallible");
        terminal
            .draw(|frame| {
                frame.buffer_mut().set_style(
                    view.layout.area,
                    Style::default()
                        .fg(self.settings.palette.text)
                        .bg(self.settings.palette.panel_bg),
                );
                for (rect, style) in &view.backgrounds {
                    frame.buffer_mut().set_style(*rect, *style);
                }
                for (rect, line) in &view.lines {
                    frame.render_widget(Paragraph::new(line.clone()), *rect);
                }
                if let Some(facts) = &view.mobile_header_facts {
                    crate::ui::render_mobile_header_from(
                        frame,
                        view.layout.mobile_header,
                        view.menu_launcher,
                        &self.settings.palette,
                        facts,
                    );
                }
                for (header, expanded) in &view.section_headers {
                    crate::ui::sidebar::render_workspace_section_header(
                        frame,
                        header,
                        *expanded,
                        &self.settings.palette,
                        view.workspace_body.bottom(),
                    );
                }
                crate::ui::render_pane_action_bar_with_palette(
                    frame,
                    view.layout.pane_actions,
                    crate::ui::pane_action_copy_label(false, false),
                    &self.settings.palette,
                );
            })
            .expect("in-memory drawing is infallible");
        let mut frame = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            terminal.backend().buffer(),
            None,
            &[],
        );
        if let Some(surface) = &view.surface {
            surface::compose(&mut frame, view.layout.pane_surface, &surface.frame);
            if let Some(popup) = surface.popup.as_deref() {
                surface::compose_popup(
                    &mut frame,
                    view.layout.pane_surface,
                    popup,
                    &self.settings.palette,
                );
            }
        }
        let buffer = frame
            .to_ratatui_buffer()
            .expect("composed frame has exact geometry");
        terminal
            .draw(|target| {
                *target.buffer_mut() = buffer;
                overlay(target);
            })
            .expect("in-memory drawing is infallible");
        let mut rendered = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            terminal.backend().buffer(),
            frame.cursor.clone(),
            &[],
        );
        rendered.hyperlinks = frame.hyperlinks;
        rendered.graphics = frame.graphics;
        for (cell, original) in rendered.cells.iter_mut().zip(frame.cells) {
            if *cell
                == (crate::protocol::CellData {
                    hyperlink: None,
                    ..original.clone()
                })
            {
                cell.hyperlink = original.hyperlink;
            }
        }
        rendered
    }

    pub(crate) fn hit<'a>(
        &self,
        view: &'a ChromeView,
        column: u16,
        row: u16,
    ) -> Option<&'a ChromeTarget> {
        view.hits
            .iter()
            .find(|hit| hit.rect.contains((column, row).into()))
            .map(|hit| &hit.target)
    }
}

pub(super) fn status_text(status: ClientEndpointStatus) -> &'static str {
    match status {
        ClientEndpointStatus::Connecting => "connecting",
        ClientEndpointStatus::Online => "online",
        ClientEndpointStatus::Reconnecting => "reconnecting",
        ClientEndpointStatus::Attention => "attention",
        ClientEndpointStatus::Disabled => "disabled",
    }
}
