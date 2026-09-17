use crate::api::schema::{AgentViewClearParams, AgentViewSetParams, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_agent_view_set(
        &mut self,
        id: String,
        mut params: AgentViewSetParams,
    ) -> String {
        if let Err(message) = crate::app::agent_view::validate_agent_view(&mut params) {
            return encode_error(id, "invalid_agent_view", message);
        }
        if let Some(plugin_id) = params.source.strip_prefix("plugin:") {
            let Some(plugin_id) = super::plugins::normalize_plugin_id(plugin_id) else {
                return encode_error(
                    id,
                    "invalid_agent_view",
                    "plugin-owned agent view source has an invalid plugin id",
                );
            };
            let Some(plugin) = self.state.installed_plugins.get(&plugin_id) else {
                return encode_error(id, "plugin_not_found", "plugin not found");
            };
            if !plugin.enabled {
                return encode_error(id, "plugin_disabled", "plugin is disabled");
            }
        }
        let source = params.source.clone();
        let label = params.label.clone();
        self.replace_agent_view_override(Some(params));
        encode_success(
            id,
            ResponseResult::AgentView {
                active: true,
                source: Some(source),
                label,
            },
        )
    }

    pub(super) fn handle_agent_view_clear(
        &mut self,
        id: String,
        params: AgentViewClearParams,
    ) -> String {
        let source = match params.source {
            Some(source) => match crate::app::agent_view::validate_agent_view_source(&source) {
                Ok(source) => Some(source),
                Err(message) => return encode_error(id, "invalid_agent_view", message),
            },
            None => None,
        };
        if source.as_deref().is_none_or(|source| {
            self.state
                .agent_view_override
                .as_ref()
                .is_some_and(|active| active.source == source)
        }) {
            self.replace_agent_view_override(None);
        }
        let active = self.state.agent_view_override.as_ref();
        encode_success(
            id,
            ResponseResult::AgentView {
                active: active.is_some(),
                source: active.map(|view| view.source.clone()),
                label: active.and_then(|view| view.label.clone()),
            },
        )
    }

    pub(crate) fn clear_agent_view_for_source(&mut self, source: &str) -> bool {
        if self
            .state
            .agent_view_override
            .as_ref()
            .is_some_and(|active| active.source == source)
        {
            self.replace_agent_view_override(None);
            true
        } else {
            false
        }
    }

    fn replace_agent_view_override(&mut self, view: Option<AgentViewSetParams>) {
        self.state.agent_view_override = view;
        self.state.agent_panel_scroll = 0;
        self.state.mobile_switcher_scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewValue,
    };

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn working_only_view() -> AgentViewSetParams {
        AgentViewSetParams {
            source: "example.views".to_string(),
            label: Some("working".to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                value: AgentViewValue::String("working".to_string()),
            }),
            sort: Vec::new(),
        }
    }

    #[test]
    fn agent_view_set_and_clear_round_trip() {
        let mut app = test_app();
        let response = app.handle_agent_view_set("set".into(), working_only_view());
        let parsed: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("success response");
        let crate::api::schema::ResponseResult::AgentView {
            active,
            source,
            label,
        } = parsed.result
        else {
            panic!("expected agent view response");
        };
        assert!(active);
        assert_eq!(source.as_deref(), Some("example.views"));
        assert_eq!(label.as_deref(), Some("working"));
        assert!(app.state.agent_view_override.is_some());
        assert_eq!(app.state.agent_panel_scroll, 0);

        let response =
            app.handle_agent_view_clear("clear".into(), AgentViewClearParams { source: None });
        let parsed: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("success response");
        let crate::api::schema::ResponseResult::AgentView { active, .. } = parsed.result else {
            panic!("expected agent view response");
        };
        assert!(!active);
        assert!(app.state.agent_view_override.is_none());
    }

    #[test]
    fn agent_view_set_rejects_unknown_source_plugin() {
        let mut app = test_app();
        let mut view = working_only_view();
        view.source = "plugin:example.missing".to_string();
        let response = app.handle_agent_view_set("set".into(), view);
        assert!(response.contains("plugin_not_found"), "{response}");
    }

    #[test]
    fn agent_view_set_rejects_invalid_spec() {
        let mut app = test_app();
        let mut view = working_only_view();
        view.source = "bad source".to_string();
        let response = app.handle_agent_view_set("set".into(), view);
        assert!(response.contains("invalid_agent_view"), "{response}");
    }
}
