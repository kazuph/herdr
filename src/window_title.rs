//! Client-local terminal title composition; no runtime or protocol state.
use std::io::{self, Write};

use crate::api::schema::AgentStatus;
use crate::config::WindowTitleMode;

pub(crate) fn session_title(name: Option<&str>) -> String {
    name.map_or_else(|| "herdr".into(), |name| format!("herdr:{name}"))
}

pub(crate) fn summary_title(statuses: impl IntoIterator<Item = AgentStatus>, name: &str) -> String {
    let mut blocked = false;
    let mut working = false;
    let mut done = false;
    for status in statuses {
        match status {
            AgentStatus::Blocked => blocked = true,
            AgentStatus::Working => working = true,
            AgentStatus::Done => done = true,
            AgentStatus::Idle | AgentStatus::Unknown => {}
        }
    }
    let marks = [(blocked, "🔴"), (working, "🟡"), (done, "🔵")]
        .into_iter()
        .filter_map(|(present, mark)| present.then_some(mark))
        .collect::<String>();
    if marks.is_empty() {
        name.into()
    } else {
        format!("{marks} {name}")
    }
}

pub(crate) struct WindowTitle {
    pub(crate) mode: WindowTitleMode,
    name: String,
    last_written: Option<Vec<u8>>,
}

impl WindowTitle {
    pub(crate) fn new(mode: WindowTitleMode) -> Self {
        Self {
            mode,
            name: session_title(crate::session::active_name().as_deref()),
            last_written: None,
        }
    }

    pub(crate) fn format(&self, statuses: impl IntoIterator<Item = AgentStatus>) -> String {
        match self.mode {
            WindowTitleMode::Summary => summary_title(statuses, &self.name),
            WindowTitleMode::Plain => self.name.clone(),
        }
    }

    pub(crate) fn write(&mut self, writer: &mut impl Write, title: &str) -> io::Result<()> {
        let bytes = crate::client::window_title_osc(Some(title));
        if self.last_written.as_ref() != Some(&bytes) {
            writer.write_all(&bytes)?;
            writer.flush()?;
            self.last_written = Some(bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_title_summary_marks_are_ordered_unique_and_space_based() {
        use AgentStatus::*;
        assert_eq!(
            summary_title([Done, Working, Blocked, Done, Working], "herdr"),
            "🔴🟡🔵 herdr"
        );
        assert_eq!(summary_title([Blocked], "herdr"), "🔴 herdr");
        assert_eq!(summary_title([Blocked, Working], "herdr"), "🔴🟡 herdr");
        assert_eq!(summary_title([Idle], "herdr"), "herdr");
        assert_eq!(summary_title([Unknown], "herdr"), "herdr");
        assert_eq!(summary_title([], "herdr"), "herdr");
    }

    #[test]
    fn window_title_session_names_and_plain_reload() {
        assert_eq!(session_title(None), "herdr");
        assert_eq!(session_title(Some("work")), "herdr:work");
        let mut title = WindowTitle::new(WindowTitleMode::Summary);
        title.name = session_title(Some("work"));
        assert_eq!(title.format([AgentStatus::Working]), "🟡 herdr:work");
        title.mode = WindowTitleMode::Plain;
        assert_eq!(title.format([AgentStatus::Blocked]), "herdr:work");
        assert_eq!(title.format([]), "herdr:work");
        title.mode = WindowTitleMode::Summary;
        assert_eq!(title.format([AgentStatus::Done]), "🔵 herdr:work");
    }

    #[test]
    fn window_title_writes_initial_once_then_only_changed_sanitized_string() {
        let mut title = WindowTitle::new(WindowTitleMode::Summary);
        let mut output = Vec::new();
        title.write(&mut output, "herdr").unwrap();
        assert_eq!(output, b"\x1b]0;herdr\x07");
        title.write(&mut output, "herdr").unwrap();
        title.write(&mut output, "herdr\x1b\x07\u{9c}").unwrap();
        assert_eq!(output, b"\x1b]0;herdr\x07");
        title.write(&mut output, "🔴 herdr").unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\x1b]0;herdr\x07\x1b]0;🔴 herdr\x07"
        );
    }

    #[test]
    fn window_title_config_defaults_and_rejects_unknown_modes() {
        let default: crate::config::Config = toml::from_str("").unwrap();
        assert_eq!(default.ui.window_title, WindowTitleMode::Summary);
        let plain: crate::config::Config =
            toml::from_str("[ui]\nwindow_title = \"plain\"").unwrap();
        assert_eq!(plain.ui.window_title, WindowTitleMode::Plain);
        assert!(
            toml::from_str::<crate::config::Config>("[ui]\nwindow_title = \"invalid\"").is_err()
        );
    }
}
