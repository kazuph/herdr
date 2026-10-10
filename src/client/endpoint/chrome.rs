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
    pub(crate) transparent_cards: bool,
    pub(crate) density: WorkspacePanelDensityConfig,
    pub(crate) spaces: SpacesSidebarConfig,
    pub(crate) agents: AgentsSidebarConfig,
    pub(crate) palette: Palette,
    pub(crate) mouse_scroll_lines: usize,
    pub(crate) mouse_capture: bool,
}

impl ChromeSettings {
    pub(crate) fn sidebar_preset_width(
        &self,
        preset: crate::app::state::SidebarWidthPreset,
    ) -> u16 {
        use crate::app::state::SidebarWidthPreset;
        match preset {
            SidebarWidthPreset::Narrow => self.sidebar_min_width,
            SidebarWidthPreset::Normal => self
                .default_sidebar_width
                .clamp(self.sidebar_min_width, self.sidebar_max_width),
            SidebarWidthPreset::Wide => self.sidebar_max_width,
        }
    }

    /// Same classification as the sidebar width button in the fork UI.
    pub(crate) fn sidebar_width_preset(&self) -> crate::app::state::SidebarWidthPreset {
        use crate::app::state::SidebarWidthPreset;
        if self.sidebar_width <= self.sidebar_preset_width(SidebarWidthPreset::Narrow) {
            SidebarWidthPreset::Narrow
        } else if self.sidebar_width <= self.sidebar_preset_width(SidebarWidthPreset::Normal) {
            SidebarWidthPreset::Normal
        } else {
            SidebarWidthPreset::Wide
        }
    }

    pub(crate) fn set_sidebar_width_preset(
        &mut self,
        preset: crate::app::state::SidebarWidthPreset,
    ) {
        self.sidebar_width = self.sidebar_preset_width(preset);
        self.sidebar_width_source = if preset == crate::app::state::SidebarWidthPreset::Normal {
            crate::app::state::SidebarWidthSource::ConfigDefault
        } else {
            crate::app::state::SidebarWidthSource::Manual
        };
    }

    /// The sidebar width button cycles narrow -> normal -> wide -> narrow.
    pub(crate) fn cycle_sidebar_width_preset(&mut self) {
        use crate::app::state::SidebarWidthPreset;
        let next = match self.sidebar_width_preset() {
            SidebarWidthPreset::Narrow => SidebarWidthPreset::Normal,
            SidebarWidthPreset::Normal => SidebarWidthPreset::Wide,
            SidebarWidthPreset::Wide => SidebarWidthPreset::Narrow,
        };
        self.set_sidebar_width_preset(next);
    }

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
            transparent_cards: ui.transparent_sidebar_cards,
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
    /// The `+` after the tabs: a new tab in the focused space.
    NewTab,
    /// The `<` / `>` arrows shown when the tabs overflow the bar.
    TabScroll {
        right: bool,
    },
    DetailTab(crate::app::state::SidebarDetailView),
    SidebarWidthToggle,
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
    workspace_selection_band: Option<Rect>,
    machine_headers: Vec<(Rect, String, bool)>,
    pub(crate) section_headers: Vec<(crate::app::state::WorkspaceSectionHeaderArea, bool)>,
    pub(crate) pane_actions: crate::ui::PaneActionBarRects,
}

#[cfg(test)]
impl ChromeView {
    pub(crate) fn empty_for_test() -> Self {
        Self {
            mobile_header_facts: None,
            menu_launcher: Rect::default(),
            layout: ChromeLayout::default(),
            hits: Vec::new(),
            workspace_max_scroll: 0,
            agent_max_scroll: 0,
            workspace_body: Rect::default(),
            detail_body: Rect::default(),
            lines: Vec::new(),
            backgrounds: Vec::new(),
            surface: None,
            workspace_selection_band: None,
            machine_headers: Vec::new(),
            section_headers: Vec::new(),
            pane_actions: crate::ui::PaneActionBarRects::default(),
        }
    }
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
    /// Keep the focused tab centered until the user scrolls the tab bar by
    /// hand; a focus change re-enables it.
    pub(crate) tab_follow_active: bool,
    tab_follow_key: Option<String>,
    pub(crate) spinner_tick: u32,
    pub(crate) detail_view: crate::app::state::SidebarDetailView,
    pub(crate) jobs_scroll: usize,
    /// Reused render target; reallocated only when the terminal size changes.
    scratch: std::cell::RefCell<Option<ratatui::Terminal<ratatui::backend::TestBackend>>>,
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
            tab_follow_active: true,
            tab_follow_key: None,
            spinner_tick: 0,
            detail_view: crate::app::state::SidebarDetailView::default(),
            jobs_scroll: 0,
            scratch: std::cell::RefCell::new(None),
        }
    }

    /// Tab chips with the same layout rules as the in-process tab bar: fixed
    /// chip widths, the focused tab in the accent color, `<` / `>` when the
    /// tabs overflow, and a `+` that opens a new tab.
    fn compute_tab_bar(
        &mut self,
        shell: &ClientShellState,
        tabs: &[&crate::protocol::endpoint_wire::ClientShellTab],
        area: Rect,
        view: &mut ChromeView,
    ) {
        if area.is_empty() {
            return;
        }
        let p = self.settings.palette.clone();
        let labels = tabs
            .iter()
            .map(|tab| {
                if tab.zoomed {
                    format!("{} Z", tab.label)
                } else {
                    tab.label.clone()
                }
            })
            .collect::<Vec<_>>();
        let widths = labels
            .iter()
            .map(|label| crate::ui::tab_chip_width(label))
            .collect::<Vec<_>>();
        let active = tabs.iter().position(|tab| tab.focused).unwrap_or(0);
        let active_key = tabs.get(active).map(|tab| tab.tab_id.clone());
        if active_key != self.tab_follow_key {
            self.tab_follow_key = active_key;
            self.tab_follow_active = true;
        }
        let bar = crate::ui::compute_tab_bar_view_for_widths(
            &widths,
            active,
            area,
            self.tab_scroll,
            self.tab_follow_active,
            self.settings.mouse_capture,
        );
        self.tab_scroll = bar.scroll;
        let endpoint = shell.active_endpoint_id.clone();
        for (idx, tab) in tabs.iter().enumerate() {
            let Some(rect) = bar.tab_hit_areas.get(idx).copied() else {
                break;
            };
            if rect.width == 0 {
                continue;
            }
            let style = if tab.focused {
                let base = Style::default()
                    .fg(crate::ui::panel_contrast_fg(&p))
                    .bg(p.accent);
                if tab.custom_label {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                }
            } else if tab.custom_label {
                Style::default().fg(p.overlay1).bg(p.surface0)
            } else {
                Style::default()
                    .fg(p.overlay0)
                    .bg(p.surface0)
                    .add_modifier(Modifier::DIM)
            };
            let width = rect.width as usize;
            let text = format!(" {:width$}", labels[idx], width = width.saturating_sub(1));
            let text = crate::ui::truncate_end(&text, width);
            view.lines
                .push((rect, Line::from(Span::styled(text, style))));
            view.hits.push(ChromeHit {
                rect,
                target: ChromeTarget::Tab(ResourceKey {
                    endpoint: endpoint.clone(),
                    id: tab.tab_id.clone(),
                }),
            });
        }
        let last_visible = bar.tab_hit_areas.iter().rposition(|rect| rect.width > 0);
        let first_visible = bar.tab_hit_areas.iter().position(|rect| rect.width > 0);
        for (rect, right, enabled) in [
            (bar.scroll_left_hit_area, false, bar.scroll > 0),
            (
                bar.scroll_right_hit_area,
                true,
                last_visible.is_some_and(|idx| idx + 1 < tabs.len()),
            ),
        ] {
            if rect.width == 0 {
                continue;
            }
            let style = if enabled {
                Style::default().fg(p.overlay1).bg(p.surface0)
            } else {
                Style::default()
                    .fg(p.overlay0)
                    .bg(p.surface0)
                    .add_modifier(Modifier::DIM)
            };
            let arrow = if right { " > " } else { " < " };
            view.lines
                .push((rect, Line::from(Span::styled(arrow, style))));
            view.hits.push(ChromeHit {
                rect,
                target: ChromeTarget::TabScroll { right },
            });
        }
        if bar.new_tab_hit_area.width > 0 {
            view.lines.push((
                bar.new_tab_hit_area,
                Line::from(Span::styled(" + ", Style::default().fg(p.overlay1))),
            ));
            view.hits.push(ChromeHit {
                rect: bar.new_tab_hit_area,
                target: ChromeTarget::NewTab,
            });
        }
        // Mark tabs hidden past either edge, like the in-process tab bar.
        let ellipsis = Style::default().fg(p.overlay0);
        if first_visible.is_some_and(|idx| idx > 0) {
            let x = if bar.scroll_left_hit_area.width > 0 {
                bar.scroll_left_hit_area.right()
            } else {
                area.x
            };
            if x < area.right() {
                view.lines.push((
                    Rect::new(x, area.y, 1, 1),
                    Line::from(Span::styled("…", ellipsis)),
                ));
            }
        }
        if last_visible.is_some_and(|idx| idx + 1 < tabs.len()) {
            let x = if bar.scroll_right_hit_area.width > 0 {
                bar.scroll_right_hit_area.x.saturating_sub(1)
            } else {
                area.right().saturating_sub(1)
            };
            if x >= area.x && x < area.right() {
                view.lines.push((
                    Rect::new(x, area.y, 1, 1),
                    Line::from(Span::styled("…", ellipsis)),
                ));
            }
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
            workspace_selection_band: None,
            machine_headers: Vec::new(),
            surface: shell.pane_surface.clone(),
            pane_actions: crate::ui::pane_action_bar_rects(
                layout.pane_actions,
                copy_label.len() as u16,
            ),
        };
        sidebar::compute(self, shell, &mut view);
        if view.surface.is_none() {
            self.offline_placeholder(shell, &mut view);
        }
        self.compute_tab_bar(shell, &tabs, layout.tab_bar, &mut view);
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

    /// A machine that is not presenting gets an explanation in its own pane area instead of a
    /// blank screen. The sidebar and every other machine stay usable.
    fn offline_placeholder(&self, shell: &ClientShellState, view: &mut ChromeView) {
        let area = view.layout.pane_surface;
        let Some(endpoint) = shell.endpoint(&shell.active_endpoint_id) else {
            return;
        };
        if area.width < 8 || area.height < 3 || endpoint.status == ClientEndpointStatus::Online {
            return;
        }
        let p = &self.settings.palette;
        let mut lines = vec![Line::from(Span::styled(
            format!("{}: {}", endpoint.label, status_text(endpoint.status)),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))];
        if let Some(diagnostic) = &endpoint.diagnostic {
            lines.push(Line::from(Span::styled(
                diagnostic.clone(),
                Style::default().fg(p.overlay0),
            )));
        }
        if !endpoint.endpoint_id.is_local() {
            lines.push(Line::from(Span::styled(
                "Local spaces in the sidebar keep working.",
                Style::default().fg(p.overlay0),
            )));
        }
        let top = area.y + area.height.saturating_sub(lines.len() as u16) / 2;
        for (index, line) in lines.into_iter().enumerate() {
            let width = (line.width() as u16).min(area.width);
            let x = area.x + (area.width - width) / 2;
            view.lines
                .push((Rect::new(x, top + index as u16, width, 1), line));
        }
    }

    pub(crate) fn render(&self, view: &ChromeView) -> crate::protocol::FrameData {
        self.render_with_overlay(view, |_| {})
    }

    pub(crate) fn render_with_overlay(
        &self,
        view: &ChromeView,
        overlay: impl FnOnce(&mut ratatui::Frame),
    ) -> crate::protocol::FrameData {
        // One reused in-memory buffer, drawn in a single pass: chrome, the endpoint surface,
        // its popup, then client overlays. Only the final buffer is converted to wire cells.
        let area = view.layout.area;
        let mut slot = self.scratch.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|terminal| terminal.backend().buffer().area != area)
        {
            *slot = Some(
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                    .expect("in-memory terminal creation is infallible"),
            );
        }
        let terminal = slot.as_mut().expect("scratch terminal was just ensured");
        terminal.current_buffer_mut().reset();
        let mut links = surface::Links::default();
        let mut cursor = None;
        let mut graphics = None;
        {
            let mut frame = terminal.get_frame();
            let frame = &mut frame;
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
            if let Some(rect) = view.workspace_selection_band {
                crate::ui::sidebar::render_workspace_selection_band(
                    frame,
                    rect,
                    &self.settings.palette,
                    view.workspace_body.bottom(),
                );
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
            for (rect, label, expanded) in &view.machine_headers {
                crate::ui::sidebar::render_workspace_group_header(
                    frame,
                    *rect,
                    label,
                    *expanded,
                    &self.settings.palette,
                    view.workspace_body.bottom(),
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
            if let Some(surface) = &view.surface {
                let exact = surface::compose_into(
                    frame.buffer_mut(),
                    view.layout.pane_surface,
                    &surface.frame,
                    &mut links,
                    &mut cursor,
                );
                if exact {
                    graphics = Some(surface.frame.graphics.clone());
                }
                if let Some(popup) = surface.popup.as_deref() {
                    if surface::compose_popup_into(
                        frame.buffer_mut(),
                        view.layout.pane_surface,
                        popup,
                        &self.settings.palette,
                        &mut links,
                        &mut cursor,
                    ) {
                        graphics = Some(popup.frame.graphics.clone());
                    }
                }
            }
            overlay(frame);
        }
        let mut rendered = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            terminal.current_buffer_mut(),
            cursor,
            &[],
        );
        links.apply(&mut rendered);
        if let Some(graphics) = graphics {
            rendered.graphics = graphics;
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
        ClientEndpointStatus::AwaitingApproval => "approval pending - click to retry",
        ClientEndpointStatus::Attention => "attention",
        ClientEndpointStatus::Disabled => "disabled",
    }
}
