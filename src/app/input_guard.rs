//! Protect external input while a manifest-defined confirmation is visible.
use std::time::Instant;

use super::App;
use crate::{api::schema::ErrorBody, detect::manifest::DetectionInput, layout::PaneId};

// The parent-approved redraw rule requires two consecutive clear scans.
const CLEAR_SCANS_REQUIRED: u8 = 2;

impl App {
    fn current_input_confirmation(&self, ws_idx: usize, pane_id: PaneId) -> Option<String> {
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)?
            .terminal_id(pane_id)
            .and_then(|id| self.state.terminals.get(id))?;
        let agent = terminal
            .effective_known_agent()
            .or(terminal.detected_agent)
            .or_else(|| terminal.managed_agent_kind())?;
        let runtime = self.lookup_runtime_sender(ws_idx, pane_id)?;
        let screen =
            runtime.detection_text_unwrapped_extra(super::screen_prompts::SCREEN_PROMPT_EXTRA_ROWS);
        crate::detect::manifest::visible_confirmation_with_input(
            agent,
            DetectionInput {
                screen: &screen,
                osc_title: &runtime.agent_osc_title(),
                osc_progress: &runtime.agent_osc_progress(),
            },
        )
        .map(str::to_owned)
    }

    pub(super) fn pane_has_visible_blocker(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.current_input_confirmation(ws_idx, pane_id).is_some()
    }

    pub(super) fn hold_external_input(&mut self, pane_id: PaneId) {
        self.input_guard_waiting.entry(pane_id).or_insert(0);
        self.input_guard_scan_at.get_or_insert(Instant::now());
    }

    fn forget_input_observations(&mut self, pane_id: PaneId) {
        self.input_guard_observed
            .retain(|(_, target), _| *target != pane_id);
    }

    pub(super) fn guard_external_input(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        force: bool,
        caller: Option<&str>,
    ) -> Result<(), ErrorBody> {
        let Some(screen) = self.current_input_confirmation(ws_idx, pane_id) else {
            self.forget_input_observations(pane_id);
            return Ok(());
        };
        self.hold_external_input(pane_id);
        let Some((signature, description)) = blocker_description(&screen) else {
            self.forget_input_observations(pane_id);
            return Err(ErrorBody { code: "pane_confirmation_unsupported".into(), message: format!("待機中：現在の確認画面の題名と選択肢を読み取れません。強制入力は未対応です。\n{screen}") });
        };
        self.input_guard_observed
            .retain(|(_, target), observed| *target != pane_id || observed == &signature);
        let caller = caller
            .and_then(|id| self.parse_pane_id(id))
            .and_then(|(ws, pane)| self.public_pane_id(ws, pane));
        if let Some(caller) = caller {
            let key = (caller, pane_id);
            if force && self.input_guard_observed.get(&key) == Some(&signature) {
                return Ok(());
            }
            if !force {
                self.input_guard_observed.insert(key, signature);
            }
        }
        Err(ErrorBody { code: "pane_confirmation_pending".into(), message: format!("待機中：{description}\n入力は送信していません。この返答を -f なしで確認した同じ呼び出し元から、同じペイン・同じ質問に限り -f（API: force=true と caller_pane_id）で再試行できます。呼び出し元不明の force は拒否します。") })
    }

    pub(crate) fn input_guard_tasks(&mut self, now: Instant) {
        if self.input_guard_scan_at.is_none_or(|at| now < at) {
            return;
        }
        self.input_guard_scan_at = None;
        let panes = self.input_guard_waiting.keys().copied().collect::<Vec<_>>();
        for pane_id in panes {
            let ws_idx = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.pane_state(pane_id).is_some());
            let Some(ws_idx) = ws_idx else {
                self.input_guard_waiting.remove(&pane_id);
                self.forget_input_observations(pane_id);
                continue;
            };
            if let Some(screen) = self.current_input_confirmation(ws_idx, pane_id) {
                self.input_guard_waiting.insert(pane_id, 0);
                let signature = blocker_description(&screen).map(|(signature, _)| signature);
                self.input_guard_observed.retain(|(_, target), observed| {
                    *target != pane_id || Some(observed.as_str()) == signature.as_deref()
                });
                continue;
            }
            self.forget_input_observations(pane_id);
            // Delayed answers and preview walks own the pane until their writes
            // finish; their intermediate redraws are not clear-screen evidence.
            if self.screen_prompt_input_in_flight(pane_id) {
                self.input_guard_waiting.insert(pane_id, 0);
                continue;
            }
            let count = self.input_guard_waiting.entry(pane_id).or_default();
            *count += 1;
            if *count < CLEAR_SCANS_REQUIRED {
                continue;
            }
            self.input_guard_waiting.remove(&pane_id);
            self.flush_msg_nudges_for_available_pane(ws_idx, pane_id);
        }
        if !self.input_guard_waiting.is_empty() {
            self.input_guard_scan_at =
                Some(now + super::screen_prompts::SCREEN_PROMPT_SCAN_INTERVAL);
        }
    }
}

fn blocker_description(screen: &str) -> Option<(String, String)> {
    if !screen.contains("Accessing workspace:") {
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
    }
    // Captured with `herdr agent read --source detection` on 2026-10-22.
    // Workspace trust is a bare selection form, not a numbered decision form.
    if screen.contains("Accessing workspace:") {
        let lines = screen.lines().collect::<Vec<_>>();
        let controls = lines
            .iter()
            .position(|line| line.trim().trim_start_matches('❯').trim() == "No, exit")?;
        if !lines[controls..]
            .iter()
            .any(|line| line.trim().trim_start_matches('❯').trim() == "Yes, I trust this folder")
        {
            return None;
        }
        let title = lines[..controls].join("\n").trim().to_string();
        return Some((
            format!("workspace-trust:{title}"),
            format!("{title}\nNo, exit / Yes, I trust this folder"),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn captured_workspace_trust_preserves_title_and_bare_choices() {
        let screen = include_str!("../detect/fixtures/claude-workspace-trust-live.txt");
        let (signature, description) =
            super::blocker_description(screen).expect("captured trust prompt");
        assert!(description.contains("fix/hold-mail-while-asking"));
        assert!(description.contains("No, exit / Yes, I trust this folder"));
        assert_eq!(
            super::blocker_description(
                &screen
                    .replace("❯ No, exit", "  No, exit")
                    .replace("  Yes, I trust this folder", "❯ Yes, I trust this folder")
            )
            .unwrap()
            .0,
            signature
        );
    }
}
