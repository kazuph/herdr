use crate::config::{Keybinds, SoundConfig, ToastConfig, ToastDelivery};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;

use crate::layout::{PaneId, PaneInfo, SplitBorder};
use crate::selection::Selection;

// ---------------------------------------------------------------------------
// Selection autoscroll types
// ---------------------------------------------------------------------------

/// Direction of automatic scrolling during text selection drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionAutoscrollDirection {
    Up,
    Down,
}

/// State for automatic scrolling during text selection drag.
///
/// When the cursor hovers in the 1-row hot zone at the top or bottom edge
/// of a pane (or outside the pane), this struct captures the direction and
/// last known mouse position so a recurring 30ms tick can continue scrolling
/// and extending the selection even when the mouse is not moving.
#[derive(Clone, Debug)]
pub(crate) struct SelectionAutoscroll {
    pub direction: SelectionAutoscrollDirection,
    pub last_mouse_screen_col: u16,
    pub last_mouse_screen_row: u16,
    pub inner_rect: Rect,
}
use crate::terminal_theme::TerminalTheme;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Theme palette — all UI colors in one place, ready for theming
// ---------------------------------------------------------------------------

/// All colors used by the UI. Derived from a base accent color for now,
/// but structured so a full theme system can replace it later.
#[derive(Clone)]
#[allow(dead_code)] // all fields defined for theming — some used later
pub struct Palette {
    /// Primary accent (highlight, active borders).
    pub accent: Color,
    /// Background for floating panels, overlays, and modals.
    pub panel_bg: Color,
    /// Subtle surface background for selected/focused items.
    pub surface0: Color,
    /// Slightly lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text (secondary info, numbers).
    pub overlay0: Color,
    /// Slightly brighter overlay text.
    pub overlay1: Color,
    /// Main text color — soft white.
    pub text: Color,
    /// Subdued text (workspace numbers, dim labels).
    pub subtext0: Color,
    /// Branch name / special label color.
    pub mauve: Color,
    /// Done / idle states.
    pub green: Color,
    /// Working / running states.
    pub yellow: Color,
    /// Needs attention / blocked states.
    pub red: Color,
    /// Unseen / done notification accent.
    pub blue: Color,
    /// Notification accent / unseen markers.
    pub teal: Color,
    /// Interrupted / warning states.
    pub peach: Color,
}

impl Palette {
    /// Catppuccin Mocha — the default.
    pub fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),
            surface_dim: Color::Rgb(30, 30, 46),
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
        }
    }

    /// Catppuccin Latte — the light Catppuccin flavor.
    pub fn catppuccin_latte() -> Self {
        Self {
            accent: Color::Rgb(30, 102, 245),
            panel_bg: Color::Rgb(239, 241, 245),
            surface0: Color::Rgb(204, 208, 218),
            surface1: Color::Rgb(188, 192, 204),
            surface_dim: Color::Rgb(230, 233, 239),
            overlay0: Color::Rgb(156, 160, 176),
            overlay1: Color::Rgb(140, 143, 161),
            text: Color::Rgb(76, 79, 105),
            subtext0: Color::Rgb(108, 111, 133),
            mauve: Color::Rgb(136, 57, 239),
            green: Color::Rgb(64, 160, 43),
            yellow: Color::Rgb(223, 142, 29),
            red: Color::Rgb(210, 15, 57),
            blue: Color::Rgb(30, 102, 245),
            teal: Color::Rgb(23, 146, 153),
            peach: Color::Rgb(254, 100, 11),
        }
    }

    /// Terminal 16-color theme.
    pub fn terminal() -> Self {
        Self {
            accent: Color::Blue,
            panel_bg: Color::Reset,
            surface0: Color::Reset,
            surface1: Color::DarkGray,
            surface_dim: Color::DarkGray,
            overlay0: Color::Gray,
            overlay1: Color::White,
            text: Color::Reset,
            subtext0: Color::Gray,
            mauve: Color::Gray,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::LightRed,
            blue: Color::Blue,
            teal: Color::Cyan,
            peach: Color::Yellow,
        }
    }

    /// Tokyo Night — blue-purple aesthetic.
    pub fn tokyo_night() -> Self {
        Self {
            accent: Color::Rgb(122, 162, 247), // blue
            panel_bg: Color::Rgb(26, 27, 38),
            surface0: Color::Rgb(36, 40, 59),
            surface1: Color::Rgb(65, 72, 104),
            surface_dim: Color::Rgb(26, 27, 38),
            overlay0: Color::Rgb(86, 95, 137),
            overlay1: Color::Rgb(105, 113, 150),
            text: Color::Rgb(192, 202, 245),
            subtext0: Color::Rgb(169, 177, 214),
            mauve: Color::Rgb(187, 154, 247),
            green: Color::Rgb(158, 206, 106),
            yellow: Color::Rgb(224, 175, 104),
            red: Color::Rgb(247, 118, 142),
            blue: Color::Rgb(122, 162, 247),
            teal: Color::Rgb(125, 207, 255),
            peach: Color::Rgb(255, 158, 100),
        }
    }

    /// Tokyo Night Day — the light Tokyo Night style.
    pub fn tokyo_night_day() -> Self {
        Self {
            accent: Color::Rgb(46, 125, 233),
            panel_bg: Color::Rgb(225, 226, 231),
            surface0: Color::Rgb(196, 200, 218),
            surface1: Color::Rgb(168, 174, 203),
            surface_dim: Color::Rgb(210, 211, 218),
            overlay0: Color::Rgb(137, 144, 179),
            overlay1: Color::Rgb(104, 112, 154),
            text: Color::Rgb(55, 96, 191),
            subtext0: Color::Rgb(97, 114, 176),
            mauve: Color::Rgb(120, 71, 189),
            green: Color::Rgb(88, 117, 57),
            yellow: Color::Rgb(140, 108, 62),
            red: Color::Rgb(245, 42, 101),
            blue: Color::Rgb(46, 125, 233),
            teal: Color::Rgb(17, 140, 116),
            peach: Color::Rgb(177, 92, 0),
        }
    }

    /// Dracula — purple/pink/green.
    pub fn dracula() -> Self {
        Self {
            accent: Color::Rgb(189, 147, 249), // purple
            panel_bg: Color::Rgb(40, 42, 54),
            surface0: Color::Rgb(68, 71, 90),
            surface1: Color::Rgb(98, 114, 164),
            surface_dim: Color::Rgb(40, 42, 54),
            overlay0: Color::Rgb(98, 114, 164),
            overlay1: Color::Rgb(130, 140, 180),
            text: Color::Rgb(248, 248, 242),
            subtext0: Color::Rgb(210, 210, 220),
            mauve: Color::Rgb(255, 121, 198), // pink
            green: Color::Rgb(80, 250, 123),
            yellow: Color::Rgb(241, 250, 140),
            red: Color::Rgb(255, 85, 85),
            blue: Color::Rgb(139, 233, 253), // cyan-ish
            teal: Color::Rgb(139, 233, 253),
            peach: Color::Rgb(255, 184, 108),
        }
    }

    /// Nord — frosty blue palette.
    pub fn nord() -> Self {
        Self {
            accent: Color::Rgb(136, 192, 208), // frost
            panel_bg: Color::Rgb(46, 52, 64),
            surface0: Color::Rgb(59, 66, 82),
            surface1: Color::Rgb(67, 76, 94),
            surface_dim: Color::Rgb(46, 52, 64),
            overlay0: Color::Rgb(76, 86, 106),
            overlay1: Color::Rgb(100, 110, 130),
            text: Color::Rgb(236, 239, 244),
            subtext0: Color::Rgb(216, 222, 233),
            mauve: Color::Rgb(180, 142, 173),
            green: Color::Rgb(163, 190, 140),
            yellow: Color::Rgb(235, 203, 139),
            red: Color::Rgb(191, 97, 106),
            blue: Color::Rgb(129, 161, 193),
            teal: Color::Rgb(143, 188, 187),
            peach: Color::Rgb(208, 135, 112),
        }
    }

    /// Gruvbox Dark — warm retro palette.
    pub fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(215, 153, 33), // yellow
            panel_bg: Color::Rgb(40, 40, 40),
            surface0: Color::Rgb(60, 56, 54),
            surface1: Color::Rgb(80, 73, 69),
            surface_dim: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(168, 153, 132),
            text: Color::Rgb(235, 219, 178),
            subtext0: Color::Rgb(213, 196, 161),
            mauve: Color::Rgb(211, 134, 155),
            green: Color::Rgb(184, 187, 38),
            yellow: Color::Rgb(250, 189, 47),
            red: Color::Rgb(251, 73, 52),
            blue: Color::Rgb(131, 165, 152),
            teal: Color::Rgb(142, 192, 124),
            peach: Color::Rgb(254, 128, 25),
        }
    }

    /// Gruvbox Light — the light retro palette.
    pub fn gruvbox_light() -> Self {
        Self {
            accent: Color::Rgb(7, 102, 120),
            panel_bg: Color::Rgb(251, 241, 199),
            surface0: Color::Rgb(235, 219, 178),
            surface1: Color::Rgb(213, 196, 161),
            surface_dim: Color::Rgb(242, 229, 188),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(124, 111, 100),
            text: Color::Rgb(60, 56, 54),
            subtext0: Color::Rgb(80, 73, 69),
            mauve: Color::Rgb(143, 63, 113),
            green: Color::Rgb(121, 116, 14),
            yellow: Color::Rgb(181, 118, 20),
            red: Color::Rgb(157, 0, 6),
            blue: Color::Rgb(7, 102, 120),
            teal: Color::Rgb(66, 123, 88),
            peach: Color::Rgb(175, 58, 3),
        }
    }

    /// One Dark — Atom's classic dark theme.
    pub fn one_dark() -> Self {
        Self {
            accent: Color::Rgb(97, 175, 239), // blue
            panel_bg: Color::Rgb(40, 44, 52),
            surface0: Color::Rgb(44, 49, 58),
            surface1: Color::Rgb(62, 68, 81),
            surface_dim: Color::Rgb(40, 44, 52),
            overlay0: Color::Rgb(92, 99, 112),
            overlay1: Color::Rgb(115, 122, 135),
            text: Color::Rgb(171, 178, 191),
            subtext0: Color::Rgb(150, 156, 168),
            mauve: Color::Rgb(198, 120, 221),
            green: Color::Rgb(152, 195, 121),
            yellow: Color::Rgb(229, 192, 123),
            red: Color::Rgb(224, 108, 117),
            blue: Color::Rgb(97, 175, 239),
            teal: Color::Rgb(86, 182, 194),
            peach: Color::Rgb(209, 154, 102),
        }
    }

    /// One Light — Atom's classic light theme.
    pub fn one_light() -> Self {
        Self {
            accent: Color::Rgb(64, 120, 242),
            panel_bg: Color::Rgb(250, 250, 250),
            surface0: Color::Rgb(240, 240, 241),
            surface1: Color::Rgb(229, 229, 230),
            surface_dim: Color::Rgb(245, 245, 246),
            overlay0: Color::Rgb(160, 161, 167),
            overlay1: Color::Rgb(104, 107, 119),
            text: Color::Rgb(56, 58, 66),
            subtext0: Color::Rgb(104, 107, 119),
            mauve: Color::Rgb(166, 38, 164),
            green: Color::Rgb(80, 161, 79),
            yellow: Color::Rgb(193, 132, 1),
            red: Color::Rgb(228, 86, 73),
            blue: Color::Rgb(64, 120, 242),
            teal: Color::Rgb(1, 132, 188),
            peach: Color::Rgb(152, 104, 1),
        }
    }

    /// Solarized Dark — Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210), // blue
            panel_bg: Color::Rgb(0, 43, 54),
            surface0: Color::Rgb(7, 54, 66),
            surface1: Color::Rgb(88, 110, 117),
            surface_dim: Color::Rgb(0, 43, 54),
            overlay0: Color::Rgb(88, 110, 117),
            overlay1: Color::Rgb(101, 123, 131),
            text: Color::Rgb(147, 161, 161),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
        }
    }

    /// Solarized Light — Ethan Schoonover's light variant.
    pub fn solarized_light() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210),
            panel_bg: Color::Rgb(253, 246, 227),
            surface0: Color::Rgb(238, 232, 213),
            surface1: Color::Rgb(147, 161, 161),
            surface_dim: Color::Rgb(238, 232, 213),
            overlay0: Color::Rgb(147, 161, 161),
            overlay1: Color::Rgb(88, 110, 117),
            text: Color::Rgb(101, 123, 131),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
        }
    }

    /// Kanagawa — inspired by Katsushika Hokusai.
    pub fn kanagawa() -> Self {
        Self {
            accent: Color::Rgb(126, 156, 216), // blue
            panel_bg: Color::Rgb(31, 31, 40),
            surface0: Color::Rgb(42, 42, 55),
            surface1: Color::Rgb(54, 54, 70),
            surface_dim: Color::Rgb(31, 31, 40),
            overlay0: Color::Rgb(114, 113, 105),
            overlay1: Color::Rgb(135, 134, 125),
            text: Color::Rgb(220, 215, 186),
            subtext0: Color::Rgb(200, 195, 170),
            mauve: Color::Rgb(149, 127, 184),
            green: Color::Rgb(118, 148, 106),
            yellow: Color::Rgb(192, 163, 110),
            red: Color::Rgb(195, 64, 67),
            blue: Color::Rgb(126, 156, 216),
            teal: Color::Rgb(127, 180, 202),
            peach: Color::Rgb(255, 160, 102),
        }
    }

    /// Kanagawa Lotus — the light Kanagawa variant.
    pub fn kanagawa_lotus() -> Self {
        Self {
            accent: Color::Rgb(77, 105, 155),
            panel_bg: Color::Rgb(242, 236, 188),
            surface0: Color::Rgb(220, 213, 172),
            surface1: Color::Rgb(201, 203, 209),
            surface_dim: Color::Rgb(213, 206, 163),
            overlay0: Color::Rgb(160, 156, 172),
            overlay1: Color::Rgb(138, 137, 128),
            text: Color::Rgb(84, 84, 100),
            subtext0: Color::Rgb(67, 67, 108),
            mauve: Color::Rgb(98, 76, 131),
            green: Color::Rgb(111, 137, 78),
            yellow: Color::Rgb(119, 113, 63),
            red: Color::Rgb(200, 64, 83),
            blue: Color::Rgb(77, 105, 155),
            teal: Color::Rgb(78, 140, 162),
            peach: Color::Rgb(204, 109, 0),
        }
    }

    /// Rosé Pine — muted, elegant.
    pub fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(196, 167, 231), // iris
            panel_bg: Color::Rgb(25, 23, 36),
            surface0: Color::Rgb(31, 29, 46),
            surface1: Color::Rgb(38, 35, 58),
            surface_dim: Color::Rgb(25, 23, 36),
            overlay0: Color::Rgb(110, 106, 134),
            overlay1: Color::Rgb(144, 140, 170),
            text: Color::Rgb(224, 222, 244),
            subtext0: Color::Rgb(200, 197, 220),
            mauve: Color::Rgb(196, 167, 231),  // iris
            green: Color::Rgb(49, 116, 143),   // pine
            yellow: Color::Rgb(246, 193, 119), // gold
            red: Color::Rgb(235, 111, 146),    // love
            blue: Color::Rgb(49, 116, 143),    // pine
            teal: Color::Rgb(156, 207, 216),   // foam
            peach: Color::Rgb(234, 154, 151),  // rose
        }
    }

    /// Rosé Pine Dawn — the light Rosé Pine variant.
    pub fn rose_pine_dawn() -> Self {
        Self {
            accent: Color::Rgb(144, 122, 169),
            panel_bg: Color::Rgb(250, 244, 237),
            surface0: Color::Rgb(242, 233, 225),
            surface1: Color::Rgb(255, 250, 243),
            surface_dim: Color::Rgb(242, 233, 225),
            overlay0: Color::Rgb(152, 147, 165),
            overlay1: Color::Rgb(121, 117, 147),
            text: Color::Rgb(70, 66, 97),
            subtext0: Color::Rgb(121, 117, 147),
            mauve: Color::Rgb(144, 122, 169),
            green: Color::Rgb(40, 105, 131),
            yellow: Color::Rgb(234, 157, 52),
            red: Color::Rgb(180, 99, 122),
            blue: Color::Rgb(40, 105, 131),
            teal: Color::Rgb(86, 148, 159),
            peach: Color::Rgb(215, 130, 126),
        }
    }

    /// Vesper — minimal high-contrast monochrome with peach and mint accents.
    pub fn vesper() -> Self {
        Self {
            accent: Color::Rgb(255, 199, 153),
            panel_bg: Color::Rgb(26, 26, 26),
            surface0: Color::Rgb(35, 35, 35),
            surface1: Color::Rgb(40, 40, 40),
            surface_dim: Color::Rgb(16, 16, 16),
            overlay0: Color::Rgb(92, 92, 92),
            overlay1: Color::Rgb(126, 126, 126),
            text: Color::Rgb(255, 255, 255),
            subtext0: Color::Rgb(160, 160, 160),
            mauve: Color::Rgb(255, 209, 168),
            green: Color::Rgb(153, 255, 228),
            yellow: Color::Rgb(255, 199, 153),
            red: Color::Rgb(255, 128, 128),
            blue: Color::Rgb(176, 176, 176),
            teal: Color::Rgb(102, 221, 204),
            peach: Color::Rgb(255, 199, 153),
        }
    }

    /// Resolve a theme by name. Returns None for unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().replace([' ', '_'], "-").as_str() {
            "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin()),
            "catppuccin-latte" | "latte" | "light" => Some(Self::catppuccin_latte()),
            "terminal" => Some(Self::terminal()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some(Self::tokyo_night_day()),
            "dracula" => Some(Self::dracula()),
            "nord" => Some(Self::nord()),
            "gruvbox" | "gruvbox-dark" => Some(Self::gruvbox()),
            "gruvbox-light" => Some(Self::gruvbox_light()),
            "one-dark" | "onedark" => Some(Self::one_dark()),
            "one-light" | "onelight" => Some(Self::one_light()),
            "solarized" | "solarized-dark" => Some(Self::solarized()),
            "solarized-light" => Some(Self::solarized_light()),
            "kanagawa" => Some(Self::kanagawa()),
            "kanagawa-lotus" | "lotus" => Some(Self::kanagawa_lotus()),
            "rose-pine" | "rosepine" => Some(Self::rose_pine()),
            "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some(Self::rose_pine_dawn()),
            "vesper" => Some(Self::vesper()),
            _ => None,
        }
    }

    /// Apply custom color overrides on top of this palette.
    pub fn with_overrides(mut self, custom: &crate::config::CustomThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCardArea {
    pub ws_idx: usize,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceSectionHeaderArea {
    pub section: crate::workspace::WorkspaceSection,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarWidthToggleRects {
    pub button: Rect,
}

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewLayout {
    Desktop,
    Mobile,
}

pub struct ViewState {
    pub layout: ViewLayout,
    pub sidebar_rect: Rect,
    pub workspace_card_areas: Vec<WorkspaceCardArea>,
    pub workspace_section_header_areas: Vec<WorkspaceSectionHeaderArea>,
    pub tab_bar_rect: Rect,
    pub tab_hit_areas: Vec<Rect>,
    pub tab_scroll_left_hit_area: Rect,
    pub tab_scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub terminal_area: Rect,
    pub pane_action_bar_rect: Rect,
    pub pane_action_cycle_layout_rect: Rect,
    pub pane_action_rotate_rect: Rect,
    pub pane_action_equalize_rect: Rect,
    pub sidebar_width_toggle_rects: SidebarWidthToggleRects,
    pub mobile_header_rect: Rect,
    pub mobile_menu_hit_area: Rect,
    pub toast_hit_area: Rect,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Onboarding,
    ReleaseNotes,
    ProductAnnouncement,
    Navigate,
    Prefix,
    Copy,
    Terminal,
    RenameWorkspace,
    RenameTab,
    RenamePane,
    NewLinkedWorktree,
    OpenExistingWorktree,
    ConfirmRemoveWorktree,
    Resize,
    ConfirmClose,
    ConfirmDanger,
    ContextMenu,
    Settings,
    GlobalMenu,
    KeybindHelp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyModeState {
    pub pane_id: PaneId,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub entry_offset_from_bottom: usize,
    pub selection: Option<CopyModeSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSelection {
    Character,
    Linewise { anchor_row: u32 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentPanelScope {
    CurrentWorkspace,
    #[default]
    AllWorkspaces,
    SortedAllWorkspaces,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspacePanelDensity {
    #[default]
    Full,
    Slim,
}

// ---------------------------------------------------------------------------
// Settings UI state
// ---------------------------------------------------------------------------

/// Which section of the settings panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Theme,
    Sound,
    Toast,
}

impl SettingsSection {
    pub const ALL: &[Self] = &[Self::Theme, Self::Sound, Self::Toast];

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Sound => "sound",
            Self::Toast => "toasts",
        }
    }
}

/// All built-in theme names in display order.
pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListState {
    pub highlighted: usize,
}

impl MenuListState {
    pub fn new(highlighted: usize) -> Self {
        Self { highlighted }
    }

    pub fn move_prev(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.highlighted = (self.highlighted + 1).min(item_count - 1);
        }
    }

    pub fn hover(&mut self, idx: Option<usize>) {
        if let Some(idx) = idx {
            self.highlighted = idx;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionListState {
    pub selected: usize,
}

impl SelectionListState {
    pub fn new(selected: usize) -> Self {
        Self { selected }
    }

    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selected = (self.selected + 1).min(item_count - 1);
        }
    }

    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
    }
}

pub struct SettingsState {
    /// Which section tab is active.
    pub section: SettingsSection,
    /// Selected item index within the current section.
    pub list: SelectionListState,
    /// The palette before opening settings (for cancel/restore).
    pub original_palette: Option<Palette>,
    /// The theme name before opening settings.
    pub original_theme: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorktreeCreateState {
    pub source_workspace_id: String,
    pub source_repo_root: std::path::PathBuf,
    pub repo_name: String,
    pub branch: String,
    pub checkout_path: std::path::PathBuf,
    pub error: Option<String>,
    pub creating: bool,
}

#[derive(Debug, Clone)]
pub struct WorktreeOpenEntry {
    pub path: std::path::PathBuf,
    pub branch: Option<String>,
    pub already_open_ws_idx: Option<usize>,
}

pub struct WorktreeOpenState {
    pub source_repo_root: std::path::PathBuf,
    pub entries: Vec<WorktreeOpenEntry>,
    pub selected: usize,
    pub error: Option<String>,
}

pub struct WorktreeRemoveState {
    pub workspace_id: String,
    pub repo_root: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub error: Option<String>,
    pub removing: bool,
    pub force_confirmation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeActionRequest {
    New { ws_idx: usize },
    Open { ws_idx: usize },
    Remove { ws_idx: usize },
}

pub(crate) enum DragTarget {
    WorkspaceReorder {
        source_ws_idx: usize,
        insert_idx: Option<usize>,
        target_section: Option<crate::workspace::WorkspaceSection>,
    },
    TabReorder {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: Option<usize>,
    },
    WorkspaceListScrollbar {
        grab_row_offset: u16,
    },
    AgentPanelScrollbar {
        grab_row_offset: u16,
    },
    PaneSplit {
        path: Vec<bool>,
        direction: Direction,
        area: Rect,
    },
    PaneScrollbar {
        pane_id: crate::layout::PaneId,
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    KeybindHelpScrollbar {
        grab_row_offset: u16,
    },
    SidebarDivider,
    SidebarSectionDivider,
}

/// Active mouse drag on a split border or sidebar divider.
pub(crate) struct DragState {
    pub target: DragTarget,
}

pub(crate) struct WorkspacePressState {
    pub ws_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct TabPressState {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PaneClickState {
    pub pane_id: PaneId,
    pub col: u16,
    pub row: u16,
    pub at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuKind {
    Workspace {
        ws_idx: usize,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        pane_id: PaneId,
        has_manual_label: bool,
        has_layout_actions: bool,
        is_zoomed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPreset {
    Claude,
    Codex,
    Agy,
}

impl AgentPreset {
    pub(crate) fn menu_label(self) -> &'static str {
        match self {
            Self::Claude => "New Claude Code agent",
            Self::Codex => "New Codex agent",
            Self::Agy => "New agy agent",
        }
    }

    pub(crate) fn base_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }

    pub(crate) fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Codex => &["codex"],
            Self::Agy => &["agy"],
        }
    }

    pub(crate) fn from_menu_label(label: &str) -> Option<Self> {
        [Self::Claude, Self::Codex, Self::Agy]
            .into_iter()
            .find(|preset| preset.menu_label() == label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStartTarget {
    Workspace { ws_idx: usize },
    Pane { pane_id: PaneId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingAgentStartRequest {
    pub target: AgentStartTarget,
    pub preset: AgentPreset,
}

/// Right-click context menu state.
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub x: u16,
    pub y: u16,
    pub list: MenuListState,
}

impl ContextMenuState {
    pub fn items(&self) -> &[&'static str] {
        match self.kind {
            ContextMenuKind::Workspace { .. } => &[
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "New worktree",
                "Open worktree",
                "Remove worktree",
                "Duplicate",
                "--",
                "Rename",
                "Close",
                "--",
                "⭐ favorite",
                "💼 work",
                "🏠 personal",
                "No section",
            ],
            ContextMenuKind::Tab { .. } => &["New tab", "Rename", "Close"],
            ContextMenuKind::Pane {
                has_manual_label: true,
                has_layout_actions: true,
                is_zoomed: false,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "Clear pane name",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Move to left split",
                "Move to right split",
                "Move to upper split",
                "Move to lower split",
                "Equalize pane sizes",
                "Cycle pane layout",
                "Rotate panes",
                "Rotate panes reverse",
                "--",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: true,
                has_layout_actions: true,
                is_zoomed: true,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "Clear pane name",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Move to left split",
                "Move to right split",
                "Move to upper split",
                "Move to lower split",
                "Equalize pane sizes",
                "Cycle pane layout",
                "Rotate panes",
                "Rotate panes reverse",
                "--",
                "Unzoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: true,
                has_layout_actions: false,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "Clear pane name",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: false,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Move to left split",
                "Move to right split",
                "Move to upper split",
                "Move to lower split",
                "Equalize pane sizes",
                "Cycle pane layout",
                "Rotate panes",
                "Rotate panes reverse",
                "--",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                has_layout_actions: true,
                is_zoomed: true,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Move to left split",
                "Move to right split",
                "Move to upper split",
                "Move to lower split",
                "Equalize pane sizes",
                "Cycle pane layout",
                "Rotate panes",
                "Rotate panes reverse",
                "--",
                "Unzoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                has_layout_actions: false,
                ..
            } => &[
                "Split vertical",
                "Split horizontal",
                "--",
                "Rename pane",
                "--",
                "New Claude Code agent",
                "New Codex agent",
                "New agy agent",
                "--",
                "Close pane",
            ],
        }
    }

    pub fn is_separator(&self, idx: usize) -> bool {
        self.items().get(idx).copied() == Some("--")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    pub kind: ToastKind,
    pub title: String,
    pub context: String,
    pub target: Option<ToastTarget>,
}

/// Rate limiter for agent toast/desktop notifications.
///
/// Without it, agent state flapping (hook and heuristic detection racing
/// around a transition) fires several notifications within seconds, and a
/// `Finished` banner can bury a `NeedsAttention` banner the user was about to
/// click.
#[derive(Debug, Default)]
pub struct NotificationThrottle {
    last_by_pane: std::collections::HashMap<PaneId, (ToastKind, std::time::Instant)>,
    last_attention_at: Option<std::time::Instant>,
}

/// Repeats of the same notification kind for the same pane are dropped
/// within this window.
const NOTIFICATION_DUPLICATE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);
/// `Finished` notifications are dropped for this long after any
/// `NeedsAttention` notification, so completion banners cannot bury a
/// question the user still has to answer.
const NOTIFICATION_ATTENTION_SHIELD: std::time::Duration = std::time::Duration::from_secs(10);

impl NotificationThrottle {
    pub fn allow(&mut self, pane_id: PaneId, kind: ToastKind, now: std::time::Instant) -> bool {
        if kind == ToastKind::Finished
            && self
                .last_attention_at
                .is_some_and(|at| now.duration_since(at) < NOTIFICATION_ATTENTION_SHIELD)
        {
            return false;
        }
        if self
            .last_by_pane
            .get(&pane_id)
            .is_some_and(|(last_kind, at)| {
                *last_kind == kind && now.duration_since(*at) < NOTIFICATION_DUPLICATE_COOLDOWN
            })
        {
            return false;
        }
        self.last_by_pane.insert(pane_id, (kind, now));
        if kind == ToastKind::NeedsAttention {
            self.last_attention_at = Some(now);
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWriteRequest {
    pub content: Vec<u8>,
    pub line_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionCopyStatus {
    pub line_count: u32,
}

pub struct ReleaseNotesState {
    pub version: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct ProductAnnouncementState {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct KeybindHelpState {
    pub scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidthSource {
    ConfigDefault,
    Persisted,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarWidthPreset {
    Narrow,
    Normal,
    Wide,
}

impl SidebarWidthPreset {
    pub(crate) fn button_label(self) -> String {
        match self {
            Self::Narrow => " NARROW ".to_string(),
            Self::Normal => " NORMAL ".to_string(),
            Self::Wide => " WIDE ".to_string(),
        }
    }

    pub(crate) fn width(self, state: &AppState) -> u16 {
        match self {
            Self::Narrow => state.sidebar_min_width,
            Self::Normal => state
                .default_sidebar_width
                .clamp(state.sidebar_min_width, state.sidebar_max_width),
            Self::Wide => {
                let normal = Self::Normal.width(state);
                let scaled_max = (u32::from(state.sidebar_max_width) * 2).div_ceil(3) as u16;
                scaled_max
                    .max(normal.saturating_add(1))
                    .clamp(state.sidebar_min_width, state.sidebar_max_width)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerousAction {
    StopServer,
    Restart,
    RestoreAgents,
}

impl DangerousAction {
    pub fn title(self) -> &'static str {
        match self {
            Self::StopServer => "Stop server?",
            Self::Restart => "Restart Herdr?",
            Self::RestoreAgents => "Restore agents?",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::StopServer => "Stops the Herdr server and all running panes.",
            Self::Restart => "Restarts the Herdr server. The saved session can be restored.",
            Self::RestoreAgents => "Types resume commands into panes with recorded agent sessions.",
        }
    }

    pub fn confirm_label(self) -> &'static str {
        match self {
            Self::StopServer => "stop",
            Self::Restart => "restart",
            Self::RestoreAgents => "restore",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingAgentSessionInfo {
    pub workspace_number: usize,
    pub workspace_label: String,
    pub pane_label: String,
    pub cwd: std::path::PathBuf,
    pub agent: String,
    pub title: Option<String>,
    pub reason: String,
}

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
pub struct AppState {
    pub terminals:
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    pub terminal_runtimes:
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalRuntime>,
    /// Terminal ids whose size is currently owned by a direct attach client.
    pub direct_attach_resize_locks: std::collections::HashSet<crate::terminal::TerminalId>,
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub selected: usize,
    pub mode: Mode,
    pub vim_mode_enabled: bool,
    pub vim_insert_mode: bool,
    pub(crate) copy_mode: Option<CopyModeState>,
    pub pane_focus_back: Vec<PaneFocusLocation>,
    pub pane_focus_forward: Vec<PaneFocusLocation>,
    pub should_quit: bool,
    /// In monolithic --no-session mode, detach exits the app because there is no server to detach from.
    pub detach_exits: bool,
    /// Set when the current client should detach from the persistent session.
    /// The server's event loop checks this and handles client detach.
    pub detach_requested: bool,
    pub request_new_workspace: bool,
    pub requested_new_workspace_section: Option<crate::workspace::WorkspaceSection>,
    pub request_new_tab: bool,
    pub request_reload_config: bool,
    /// Set when UI interaction requested agent restore to run from the App loop.
    pub request_agent_restore: bool,
    /// Stop the server and relaunch herdr after shutdown completes.
    pub request_restart: bool,
    /// Set when the headless server should ask attached clients to reload
    /// their client-local sound config from disk.
    pub request_client_sound_config_reload: bool,
    /// Set when UI interaction requested a clipboard write that must be
    /// handled by the outer App/event loop instead of directly from AppState.
    pub request_clipboard_write: Option<ClipboardWriteRequest>,
    pub creating_new_tab: bool,
    pub requested_new_tab_name: Option<String>,
    pub worktree_directory: std::path::PathBuf,
    pub worktree_create: Option<WorktreeCreateState>,
    pub worktree_open: Option<WorktreeOpenState>,
    pub worktree_remove: Option<WorktreeRemoveState>,
    pub pending_worktree_action: Option<WorktreeActionRequest>,
    pub pending_duplicate_workspace: Option<usize>,
    pub(crate) pending_agent_start: Option<PendingAgentStartRequest>,
    pub pending_danger_action: Option<DangerousAction>,
    pub rename_pane_target: Option<PaneId>,
    pub request_complete_onboarding: bool,
    pub name_input: String,
    pub name_input_replace_on_type: bool,
    pub release_notes: Option<ReleaseNotesState>,
    pub product_announcement: Option<ProductAnnouncementState>,
    pub keybind_help: KeybindHelpState,
    pub workspace_scroll: usize,
    pub agent_panel_scroll: usize,
    pub tab_scroll: usize,
    pub tab_scroll_follow_active: bool,
    pub mobile_switcher_scroll: usize,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    pub(crate) workspace_press: Option<WorkspacePressState>,
    pub(crate) tab_press: Option<TabPressState>,
    pub(crate) last_pane_click: Option<PaneClickState>,
    pub selection: Option<Selection>,
    pub selection_autoscroll: Option<SelectionAutoscroll>,
    pub context_menu: Option<ContextMenuState>,
    // Notifications
    pub update_available: Option<String>,
    pub update_install_command: String,
    pub latest_release_notes_available: bool,
    pub update_dismissed: bool,
    pub config_diagnostic: Option<String>,
    pub toast: Option<ToastNotification>,
    pub notification_throttle: NotificationThrottle,
    pub selection_copy_status: Option<SelectionCopyStatus>,
    /// Last reported focus state for the outer terminal hosting herdr.
    /// None means unsupported or not yet reported, which preserves active-pane suppression.
    pub outer_terminal_focus: Option<bool>,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    pub default_sidebar_width: u16,
    pub sidebar_width: u16,
    pub sidebar_min_width: u16,
    pub sidebar_max_width: u16,
    pub sidebar_width_source: SidebarWidthSource,
    pub sidebar_width_auto: bool,
    pub sidebar_collapsed: bool,
    /// Ratio of sidebar height allocated to the workspaces section.
    pub sidebar_section_split: f32,
    pub collapsed_workspace_sections:
        std::collections::BTreeSet<crate::workspace::WorkspaceSection>,
    pub workspace_panel_density: WorkspacePanelDensity,
    pub agent_panel_scope: AgentPanelScope,
    /// Capture mouse input for Herdr's own mouse UI. When false, Herdr only
    /// captures mouse while the focused pane app requests mouse reporting.
    pub mouse_capture: bool,
    pub confirm_close: bool,
    pub prompt_new_tab_name: bool,
    pub show_tab_bar: bool,
    pub show_agent_labels_on_pane_borders: bool,
    pub kitty_graphics_enabled: bool,
    pub default_shell: String,
    pub pane_scrollback_limit_bytes: usize,
    #[allow(dead_code)] // kept for backward compat; palette.accent is the source of truth
    pub accent: Color,
    pub sound: SoundConfig,
    pub local_sound_playback: bool,
    pub toast_config: ToastConfig,
    pub agent_restore_config: crate::config::AgentRestoreConfig,
    pub agent_session_ledger: crate::persist::agent_ledger::AgentSessionLedger,
    pub agent_session_ledger_path: Option<std::path::PathBuf>,
    pub keybinds: Keybinds,
    /// Frame counter for spinner animations (wraps around).
    pub spinner_tick: u32,
    /// UI color palette — all sidebar/UI colors centralized for theming.
    pub palette: Palette,
    /// Currently applied theme name (for settings UI).
    pub theme_name: String,
    /// Settings panel state.
    pub settings: SettingsState,
    /// Highlight state for the bottom-right global launcher menu.
    pub global_menu: MenuListState,
    /// Resolved host terminal default colors for theming embedded panes.
    pub host_terminal_theme: TerminalTheme,
    /// Set when a persisted session snapshot would change.
    pub session_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneFocusLocation {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: PaneId,
}

impl AppState {
    pub(crate) fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    pub(crate) fn set_sidebar_width_preset(&mut self, preset: SidebarWidthPreset) {
        self.sidebar_width = preset.width(self);
        self.sidebar_width_source = match preset {
            SidebarWidthPreset::Normal => SidebarWidthSource::ConfigDefault,
            SidebarWidthPreset::Narrow | SidebarWidthPreset::Wide => SidebarWidthSource::Manual,
        };
        self.sidebar_width_auto = false;
        self.mark_session_dirty();
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound.enabled
    }

    pub fn toast_delivery(&self) -> ToastDelivery {
        self.toast_config.delivery
    }

    pub(crate) fn global_menu_attention_badge_visible(&self) -> bool {
        self.update_available.is_some()
    }

    pub(crate) fn global_menu_item_has_badge(&self, item: &str) -> bool {
        item == "update ready" && self.update_available.is_some()
    }

    pub(crate) fn settings_section_has_badge(&self, section: SettingsSection) -> bool {
        let _ = section;
        false
    }

    pub fn focused_pane_requests_mouse_capture(&self) -> bool {
        self.mode == Mode::Terminal
            && self
                .active
                .and_then(|idx| self.focused_runtime_in_workspace(idx))
                .and_then(crate::terminal::TerminalRuntime::input_state)
                .is_some_and(crate::pane::InputState::mouse_reporting_enabled)
    }

    pub fn should_capture_host_mouse(&self) -> bool {
        self.mouse_capture || self.focused_pane_requests_mouse_capture()
    }

    pub fn is_prefix_key(&self, key: crate::input::TerminalKey) -> bool {
        crate::config::terminal_key_matches_combo(key, (self.prefix_code, self.prefix_mods))
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (24, 80)
        }
    }

    /// Returns true when the given (workspace, tab, pane) refers to the
    /// currently focused pane in the active workspace's active tab.
    pub(crate) fn runtime_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        #[cfg(test)]
        if let Some(runtime) = self.workspaces.get(ws_idx)?.test_runtimes.get(&pane_id) {
            return Some(runtime);
        }
        #[cfg(test)]
        if let Some(runtime) = self
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find_map(|tab| tab.runtimes.get(&pane_id))
        {
            return Some(runtime);
        }
        let terminal_id = self.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        self.terminal_runtimes.get(terminal_id)
    }

    #[cfg(test)]
    pub(crate) fn runtime_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.workspaces.iter().find_map(|ws| {
            #[cfg(test)]
            if let Some(runtime) = ws.test_runtimes.get(&pane_id) {
                return Some(runtime);
            }
            #[cfg(test)]
            if let Some(runtime) = ws.tabs.iter().find_map(|tab| tab.runtimes.get(&pane_id)) {
                return Some(runtime);
            }
            let terminal_id = ws.terminal_id(pane_id)?;
            self.terminal_runtimes.get(terminal_id)
        })
    }

    pub(crate) fn focused_runtime_in_workspace(
        &self,
        ws_idx: usize,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        self.runtime_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub fn is_active_pane(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if ws_idx != active_ws_idx {
            return false;
        }
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        if tab_idx != ws.active_tab_index() {
            return false;
        }
        ws.active_tab().map(|tab| tab.layout.focused()) == Some(pane_id)
    }

    pub fn missing_agent_session_infos(&self) -> Vec<MissingAgentSessionInfo> {
        let mut infos = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let workspace_label = ws.display_name_from(&self.terminals, &self.terminal_runtimes);
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                let tab_id = format!("{}:{}", ws.id, tab_idx + 1);
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.terminals.get(&pane.attached_terminal_id) else {
                        continue;
                    };
                    let Some(agent) = terminal
                        .effective_agent_label()
                        .or(terminal.agent_name.as_deref())
                        .or_else(|| {
                            terminal
                                .pending_restore
                                .as_ref()
                                .map(|restore| restore.agent.as_str())
                        })
                    else {
                        continue;
                    };
                    if terminal
                        .agent_session_id
                        .as_deref()
                        .is_some_and(crate::agent_sessions::is_safe_session_id)
                    {
                        continue;
                    }
                    let ledger_entry = self
                        .agent_session_ledger
                        .get(&ws.id, &tab_id, pane_id.raw())
                        .filter(|entry| entry.agent == agent);
                    if ledger_entry.is_some_and(|entry| {
                        crate::agent_sessions::is_safe_session_id(&entry.session_id)
                    }) {
                        continue;
                    }
                    let reason = if terminal.agent_session_id.as_deref().is_some() {
                        "invalid terminal session id"
                    } else if self
                        .agent_session_ledger
                        .get(&ws.id, &tab_id, pane_id.raw())
                        .filter(|entry| entry.agent == agent)
                        .is_some()
                    {
                        "invalid ledger session id"
                    } else {
                        "missing session id"
                    };
                    let pane_number = ws
                        .public_pane_number(*pane_id)
                        .unwrap_or(pane_id.raw() as usize);
                    infos.push(MissingAgentSessionInfo {
                        workspace_number: ws_idx + 1,
                        workspace_label: workspace_label.clone(),
                        pane_label: format!("{}-{pane_number}", ws_idx + 1),
                        cwd: terminal.cwd.clone(),
                        agent: agent.to_string(),
                        title: terminal
                            .agent_task_title
                            .clone()
                            .or_else(|| terminal.pane_title.clone())
                            .or_else(|| terminal.manual_label.clone()),
                        reason: reason.to_string(),
                    });
                }
            }
        }
        infos
    }
}

#[cfg(test)]
pub fn key_matches(
    key: &crossterm::event::KeyEvent,
    expected_code: KeyCode,
    expected_mods: KeyModifiers,
) -> bool {
    crate::config::terminal_key_matches_combo(
        crate::input::TerminalKey::from(*key),
        (expected_code, expected_mods),
    )
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl AppState {
    /// Create an AppState for testing — no channels, no PTYs.
    pub fn test_new() -> Self {
        Self {
            terminals: std::collections::HashMap::new(),
            terminal_runtimes: std::collections::HashMap::new(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            mode: Mode::Navigate,
            vim_mode_enabled: false,
            vim_insert_mode: false,
            copy_mode: None,
            pane_focus_back: Vec::new(),
            pane_focus_forward: Vec::new(),
            should_quit: false,
            detach_exits: false,
            detach_requested: false,
            request_new_workspace: false,
            requested_new_workspace_section: None,
            request_new_tab: false,
            request_reload_config: false,
            request_agent_restore: false,
            request_restart: false,
            request_client_sound_config_reload: false,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            worktree_directory: crate::worktree::expand_tilde_path("~/.herdr/worktrees"),
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            pending_worktree_action: None,
            pending_duplicate_workspace: None,
            pending_agent_start: None,
            pending_danger_action: None,
            rename_pane_target: None,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            release_notes: None,
            product_announcement: None,
            keybind_help: KeybindHelpState { scroll: 0 },
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: ViewState {
                layout: ViewLayout::Desktop,
                sidebar_rect: Rect::default(),
                workspace_card_areas: Vec::new(),
                workspace_section_header_areas: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                pane_action_bar_rect: Rect::default(),
                pane_action_cycle_layout_rect: Rect::default(),
                pane_action_rotate_rect: Rect::default(),
                pane_action_equalize_rect: Rect::default(),
                sidebar_width_toggle_rects: SidebarWidthToggleRects::default(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            workspace_press: None,
            tab_press: None,
            last_pane_click: None,
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            update_available: None,
            update_install_command: crate::update::update_install_command().into(),
            latest_release_notes_available: false,
            update_dismissed: false,
            config_diagnostic: None,
            toast: None,
            notification_throttle: NotificationThrottle::default(),
            selection_copy_status: None,
            outer_terminal_focus: None,
            prefix_code: KeyCode::Char('b'),
            prefix_mods: KeyModifiers::CONTROL,
            default_sidebar_width: 26,
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 72,
            sidebar_width_source: SidebarWidthSource::ConfigDefault,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            sidebar_section_split: 0.5,
            collapsed_workspace_sections: std::collections::BTreeSet::new(),
            workspace_panel_density: WorkspacePanelDensity::Full,
            agent_panel_scope: AgentPanelScope::AllWorkspaces,
            mouse_capture: true,
            confirm_close: true,
            prompt_new_tab_name: true,
            show_tab_bar: true,
            show_agent_labels_on_pane_borders: false,
            kitty_graphics_enabled: false,
            default_shell: String::new(),
            pane_scrollback_limit_bytes: crate::config::DEFAULT_SCROLLBACK_LIMIT_BYTES,
            accent: Color::Cyan,
            sound: SoundConfig {
                enabled: false,
                ..SoundConfig::default()
            },
            local_sound_playback: false,
            toast_config: ToastConfig::default(),
            agent_restore_config: crate::config::AgentRestoreConfig::default(),
            agent_session_ledger: crate::persist::agent_ledger::AgentSessionLedger::default(),
            agent_session_ledger_path: None,
            keybinds: Keybinds::default(),
            spinner_tick: 0,
            palette: Palette::catppuccin(),
            theme_name: "catppuccin".to_string(),
            settings: SettingsState {
                section: SettingsSection::Theme,
                list: SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
            },
            global_menu: MenuListState::new(0),
            host_terminal_theme: TerminalTheme::default(),
            session_dirty: false,
        }
    }

    /// Populate missing `TerminalState` entries for every pane so tests that
    /// read or write terminal metadata don't need to manually create them.
    pub fn ensure_test_terminals(&mut self) {
        use crate::terminal::TerminalState;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    if !self.terminals.contains_key(&pane.attached_terminal_id) {
                        let cwd = ws.identity_cwd.clone();
                        self.terminals.insert(
                            pane.attached_terminal_id.clone(),
                            TerminalState::new(pane.attached_terminal_id.clone(), cwd),
                        );
                    }
                }
            }
        }
    }

    pub fn insert_test_runtime(
        &mut self,
        pane_id: crate::layout::PaneId,
        runtime: crate::terminal::TerminalRuntime,
    ) {
        let Some(terminal_id) = self
            .workspaces
            .iter()
            .find_map(|ws| ws.terminal_id(pane_id).cloned())
        else {
            return;
        };
        self.terminal_runtimes.insert(terminal_id, runtime);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn notification_throttle_drops_same_kind_repeats_within_cooldown() {
        let mut throttle = NotificationThrottle::default();
        let pane = PaneId::from_raw(1);
        let t0 = std::time::Instant::now();

        assert!(throttle.allow(pane, ToastKind::NeedsAttention, t0));
        assert!(!throttle.allow(
            pane,
            ToastKind::NeedsAttention,
            t0 + std::time::Duration::from_secs(3)
        ));
        assert!(throttle.allow(
            pane,
            ToastKind::NeedsAttention,
            t0 + std::time::Duration::from_secs(11)
        ));
    }

    #[test]
    fn missing_agent_session_infos_lists_live_agents_without_session_ids() {
        let mut state = AppState::test_new();
        state.workspaces = vec![
            crate::workspace::Workspace::test_new("missing"),
            crate::workspace::Workspace::test_new("recorded"),
        ];
        state.ensure_test_terminals();

        let missing_pane = state.workspaces[0].tabs[0].root_pane;
        let missing_terminal_id = state.workspaces[0].tabs[0]
            .terminal_id(missing_pane)
            .unwrap()
            .clone();
        state
            .terminals
            .get_mut(&missing_terminal_id)
            .unwrap()
            .detected_agent = Some(crate::detect::Agent::Claude);

        let recorded_pane = state.workspaces[1].tabs[0].root_pane;
        let recorded_terminal_id = state.workspaces[1].tabs[0]
            .terminal_id(recorded_pane)
            .unwrap()
            .clone();
        let recorded = state.terminals.get_mut(&recorded_terminal_id).unwrap();
        recorded.detected_agent = Some(crate::detect::Agent::Codex);
        recorded.agent_session_id = Some("019ef3a2-749c-7b52-b324-2c20cb0b2379".into());

        let infos = state.missing_agent_session_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].workspace_number, 1);
        assert_eq!(infos[0].workspace_label, "missing");
        assert_eq!(infos[0].pane_label, "1-1");
        assert_eq!(infos[0].agent, "claude");
        assert_eq!(infos[0].reason, "missing session id");
        assert_eq!(infos[0].cwd, state.terminals[&missing_terminal_id].cwd);
    }

    #[test]
    fn missing_agent_session_infos_accepts_safe_pane_ledger_entry() {
        let mut state = AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("ledger")];
        state.ensure_test_terminals();

        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let workspace_id = state.workspaces[0].id.clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        state
            .agent_session_ledger
            .upsert(crate::persist::agent_ledger::AgentSessionLedgerEntry {
                pane_id: pane_id.raw(),
                terminal_id: terminal_id.to_string(),
                workspace_id: workspace_id.clone(),
                tab_id: format!("{workspace_id}:1"),
                cwd: terminal.cwd.clone(),
                agent: "claude".into(),
                session_id: "11111111-2222-3333-4444-555555555555".into(),
                observed_at: 1,
                source: "test".into(),
                title: None,
            });

        assert!(state.missing_agent_session_infos().is_empty());
    }

    #[test]
    fn notification_throttle_shields_attention_from_finished_burial() {
        let mut throttle = NotificationThrottle::default();
        let asking_pane = PaneId::from_raw(1);
        let other_pane = PaneId::from_raw(2);
        let t0 = std::time::Instant::now();

        assert!(throttle.allow(asking_pane, ToastKind::NeedsAttention, t0));
        // Finished from the same pane or any other pane must not bury the
        // question banner the user is about to click.
        assert!(!throttle.allow(
            asking_pane,
            ToastKind::Finished,
            t0 + std::time::Duration::from_secs(2)
        ));
        assert!(!throttle.allow(
            other_pane,
            ToastKind::Finished,
            t0 + std::time::Duration::from_secs(5)
        ));
        assert!(throttle.allow(
            other_pane,
            ToastKind::Finished,
            t0 + std::time::Duration::from_secs(11)
        ));
    }

    #[test]
    fn notification_throttle_always_lets_new_attention_through() {
        let mut throttle = NotificationThrottle::default();
        let t0 = std::time::Instant::now();

        assert!(throttle.allow(PaneId::from_raw(1), ToastKind::Finished, t0));
        assert!(throttle.allow(
            PaneId::from_raw(2),
            ToastKind::NeedsAttention,
            t0 + std::time::Duration::from_secs(1)
        ));
        assert!(throttle.allow(
            PaneId::from_raw(3),
            ToastKind::NeedsAttention,
            t0 + std::time::Duration::from_secs(2)
        ));
    }

    #[test]
    fn notification_throttle_drops_duplicate_finished_per_pane() {
        let mut throttle = NotificationThrottle::default();
        let pane = PaneId::from_raw(1);
        let t0 = std::time::Instant::now();

        assert!(throttle.allow(pane, ToastKind::Finished, t0));
        assert!(!throttle.allow(
            pane,
            ToastKind::Finished,
            t0 + std::time::Duration::from_secs(4)
        ));
    }

    #[test]
    fn built_in_theme_names_resolve() {
        for name in THEME_NAMES {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn light_theme_aliases_resolve() {
        for name in ["light", "latte", "tokyo-day", "onelight", "lotus", "dawn"] {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn key_matches_requires_exact_modifiers() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        assert!(!key_matches(
            &KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
    }

    #[test]
    fn key_matches_letters_case_insensitively() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            KeyCode::Char('b'),
            KeyModifiers::SHIFT,
        ));
    }
}
