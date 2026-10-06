use ratatui::layout::{Constraint, Layout, Rect};

use super::{ChromeLayout, ChromeSettings};
use crate::config::SidebarCollapsedModeConfig;

pub(super) fn compute(
    settings: &ChromeSettings,
    cols: u16,
    rows: u16,
    tab_count: usize,
    workspace_available: bool,
) -> ChromeLayout {
    let area = Rect::new(0, 0, cols, rows);
    if cols > 0 && cols <= settings.mobile_width_threshold {
        let (mobile_header, pane_surface) = crate::ui::mobile_shell_regions(area);
        return ChromeLayout {
            area,
            mobile_header,
            pane_surface,
            ..ChromeLayout::default()
        };
    }
    let sidebar_width = if settings.sidebar_collapsed {
        match settings.sidebar_collapsed_mode {
            // Existing ui.rs COLLAPSED_WIDTH; this also includes the separator column.
            SidebarCollapsedModeConfig::Compact => 4,
            SidebarCollapsedModeConfig::Hidden => 0,
        }
    } else {
        settings
            .sidebar_width
            .clamp(settings.sidebar_min_width, settings.sidebar_max_width)
    };
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(1)]).areas(area);
    let (tab_bar, pane_surface, pane_actions) = if workspace_available {
        crate::ui::desktop_shell_regions(
            settings.show_tab_bar,
            settings.hide_single_tab,
            tab_count,
            true,
            main,
        )
    } else {
        (Rect::default(), main, Rect::default())
    };
    ChromeLayout {
        area,
        sidebar,
        tab_bar,
        pane_surface,
        pane_actions,
        mobile_header: Rect::default(),
    }
}
