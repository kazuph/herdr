//! Protect external input while a manifest-defined confirmation is visible.
use std::time::Instant;

use super::App;
use crate::{api::schema::ErrorBody, detect::manifest::DetectionInput, layout::PaneId};

impl App {
    pub(super) fn pane_has_visible_blocker(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .and_then(|id| self.state.terminals.get(id))
        else {
            return false;
        };
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return false;
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return false;
        };
        crate::detect::manifest::visible_blocker_with_input(
            agent,
            DetectionInput {
                screen: &runtime.detection_text(),
                osc_title: &runtime.agent_osc_title(),
                osc_progress: &runtime.agent_osc_progress(),
            },
        )
    }

    pub(super) fn hold_external_input(&mut self, pane_id: PaneId) {
        self.input_guard_waiting.insert(pane_id);
        self.input_guard_scan_at.get_or_insert(Instant::now());
    }

    pub(super) fn guard_external_input(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        force: bool,
    ) -> Result<(), ErrorBody> {
        if !self.pane_has_visible_blocker(ws_idx, pane_id) {
            self.input_guard_observed.remove(&pane_id);
            return Ok(());
        }
        self.hold_external_input(pane_id);
        let screen = self
            .lookup_runtime_sender(ws_idx, pane_id)
            .map(|runtime| {
                runtime
                    .detection_text_unwrapped_extra(super::screen_prompts::SCREEN_PROMPT_EXTRA_ROWS)
            })
            .unwrap_or_default();
        let Some((signature, description)) = blocker_description(&screen) else {
            self.input_guard_observed.remove(&pane_id);
            return Err(ErrorBody { code: "pane_confirmation_unsupported".into(), message: format!("待機中：確認画面の題名と選択肢を読み取れません。強制入力は未対応です。\n{screen}") });
        };
        if force && self.input_guard_observed.get(&pane_id) == Some(&signature) {
            return Ok(());
        }
        // An unseen -f cannot grant itself permission for a second blind -f.
        if !force {
            self.input_guard_observed.insert(pane_id, signature);
        } else if self.input_guard_observed.get(&pane_id) != Some(&signature) {
            self.input_guard_observed.remove(&pane_id);
        }
        Err(ErrorBody { code: "pane_confirmation_pending".into(), message: format!("待機中：{description}\n入力は送信していません。この返答を -f なしで確認してから、同じペイン・同じ質問に限り -f（API: force=true）で再試行できます。") })
    }

    pub(crate) fn input_guard_tasks(&mut self, now: Instant) {
        if self.input_guard_scan_at.is_none_or(|at| now < at) {
            return;
        }
        self.input_guard_scan_at = None;
        let panes = self.input_guard_waiting.iter().copied().collect::<Vec<_>>();
        for pane_id in panes {
            let ws_idx = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.pane_state(pane_id).is_some());
            let Some(ws_idx) = ws_idx else {
                self.input_guard_waiting.remove(&pane_id);
                self.input_guard_observed.remove(&pane_id);
                continue;
            };
            if self.pane_has_visible_blocker(ws_idx, pane_id) {
                let signature = self
                    .lookup_runtime_sender(ws_idx, pane_id)
                    .and_then(|runtime| {
                        blocker_description(&runtime.detection_text_unwrapped_extra(
                            super::screen_prompts::SCREEN_PROMPT_EXTRA_ROWS,
                        ))
                    })
                    .map(|(signature, _)| signature);
                if self.input_guard_observed.get(&pane_id) != signature.as_ref() {
                    self.input_guard_observed.remove(&pane_id);
                }
                continue;
            }
            self.input_guard_waiting.remove(&pane_id);
            self.input_guard_observed.remove(&pane_id);
            self.flush_msg_nudges_for_available_pane(ws_idx, pane_id);
        }
        if !self.input_guard_waiting.is_empty() {
            self.input_guard_scan_at =
                Some(now + super::screen_prompts::SCREEN_PROMPT_SCAN_INTERVAL);
        }
    }
}

fn blocker_description(screen: &str) -> Option<(String, String)> {
    if let Some(prompt) = crate::screen_prompt::parse_screen_prompt(screen) {
        let description = format!(
            "{}\n{}",
            prompt.title(),
            prompt
                .choices
                .iter()
                .map(|(n, label)| format!("{n}. {label}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return Some((prompt.signature(), description));
    }
    // MCP elicitation has unnumbered controls and is not a decision-answer form.
    let lines = screen.lines().collect::<Vec<_>>();
    let start = lines.iter().rposition(|line| {
        line.trim().starts_with("MCP server ") && line.contains("requests your input")
    })?;
    let controls = lines[start..].iter().position(|line| {
        let line = line.trim().trim_start_matches('❯').trim();
        line.starts_with("Accept") || line.starts_with("Decline")
    })? + start;
    let title = lines[start..controls].join("\n").trim().to_string();
    let signature = format!("mcp:{title}:Accept:Decline");
    Some((signature, format!("{title}\nAccept / Decline")))
}
