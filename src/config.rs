use crossterm::event::{KeyCode, KeyModifiers};

mod io;
mod keybinds;
mod model;
mod sound;
mod theme;

pub use self::{
    io::{
        config_diagnostic_summary, config_dir, config_path, load_live_config,
        remove_keybinding_config_sections, remove_section_key, state_dir, upsert_section_bool,
        upsert_section_value,
    },
    keybinds::{
        format_key_combo, normalize_key_combo, terminal_key_matches_combo, ActionKeybinds,
        BindingConfig, CommandKeybindConfig, CustomCommandAction, CustomCommandKeybind,
        IndexedKeybind, Keybinds, LiveKeybindConfig,
    },
    model::{
        validated_sidebar_bounds, AgentPanelScopeConfig, AgentRestoreConfig, AgentStartConfig,
        Config, ConfigReloadReport, ConfigReloadStatus, ToastConfig, ToastDelivery,
        WorkspacePanelDensityConfig,
    },
    sound::SoundConfig,
    theme::{parse_color, CustomThemeColors, ThemeConfig},
};

pub(crate) use self::io::upsert_top_level_bool;

pub const CONFIG_PATH_ENV_VAR: &str = "HERDR_CONFIG_PATH";
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

#[cfg(test)]
pub(crate) fn app_dir_name() -> &'static str {
    io::app_dir_name()
}

impl Config {
    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
    }

    pub fn prefix_key(&self) -> (KeyCode, KeyModifiers) {
        self.validated_keybinds().1
    }

    /// Parsed keybinds for Herdr actions.
    pub fn keybinds(&self) -> Keybinds {
        self.validated_keybinds().3
    }

    pub fn collect_diagnostics(&self) -> Vec<String> {
        let (prefix_diag, _, keybind_diags, _) = self.validated_keybinds();
        prefix_diag
            .into_iter()
            .chain(keybind_diags)
            .chain(self.ui.sound.diagnostics())
            .collect()
    }

    pub(crate) fn live_keybinds_with_diagnostics(
        &self,
    ) -> Result<(LiveKeybindConfig, Vec<String>), Vec<String>> {
        let (prefix_diag, prefix, keybind_diags, keybinds) = self.validated_keybinds();
        if let Some(prefix_diag) = prefix_diag {
            Err(std::iter::once(prefix_diag).chain(keybind_diags).collect())
        } else {
            Ok((LiveKeybindConfig { prefix, keybinds }, keybind_diags))
        }
    }
}
