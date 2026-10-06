//! Connection-owned presses are released against their original opaque pane.

use std::collections::HashMap;

use crate::protocol::endpoint_wire::{
    ClientKeyCode, ClientKeyKind, ClientMouseButton, ClientMouseKind, ClientPaneInputEvent,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PressId {
    Physical(u32),
    Semantic(ClientKeyCode),
    Mouse(ClientMouseButton),
}

fn key_id(code: &ClientKeyCode, physical: Option<u32>) -> PressId {
    physical.map_or_else(|| PressId::Semantic(code.clone()), PressId::Physical)
}

pub(super) struct HeldInput {
    pub(super) pane_id: String,
    pub(super) release: ClientPaneInputEvent,
}

#[derive(Default)]
pub(super) struct HeldInputs(HashMap<PressId, HeldInput>);

impl HeldInputs {
    /// Called only after the target runtime accepted this individual event.
    pub(super) fn track(&mut self, pane_id: &str, event: &ClientPaneInputEvent) {
        let (id, release) = match event {
            ClientPaneInputEvent::Key {
                code,
                modifiers,
                kind: ClientKeyKind::Press,
                shifted_codepoint,
                tracks_release: true,
                physical_key_id,
                windows_record,
                ..
            } => (
                key_id(code, *physical_key_id),
                ClientPaneInputEvent::Key {
                    code: code.clone(),
                    modifiers: *modifiers,
                    kind: ClientKeyKind::Release,
                    repeat_count: 1,
                    shifted_codepoint: *shifted_codepoint,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: *physical_key_id,
                    windows_record: *windows_record,
                },
            ),
            ClientPaneInputEvent::Key {
                code,
                kind: ClientKeyKind::Release,
                physical_key_id,
                ..
            } => {
                self.0.remove(&key_id(code, *physical_key_id));
                return;
            }
            ClientPaneInputEvent::Mouse {
                kind: ClientMouseKind::Down(button) | ClientMouseKind::Drag(button),
                position,
                geometry,
                modifiers,
                ..
            } => {
                let id = PressId::Mouse(*button);
                if matches!(
                    event,
                    ClientPaneInputEvent::Mouse {
                        kind: ClientMouseKind::Drag(_),
                        ..
                    }
                ) && !self.0.contains_key(&id)
                {
                    return;
                }
                (
                    id,
                    ClientPaneInputEvent::Mouse {
                        kind: ClientMouseKind::Up(*button),
                        position: *position,
                        geometry: *geometry,
                        modifiers: *modifiers,
                        lines: 1,
                    },
                )
            }
            ClientPaneInputEvent::Mouse {
                kind: ClientMouseKind::Up(button),
                ..
            } => {
                self.0.remove(&PressId::Mouse(*button));
                return;
            }
            _ => return,
        };
        self.0.insert(
            id,
            HeldInput {
                pane_id: pane_id.into(),
                release,
            },
        );
    }

    pub(super) fn drain(&mut self) -> Vec<HeldInput> {
        self.0.drain().map(|(_, held)| held).collect()
    }
}

pub(super) fn apply_events(
    runtime: &crate::terminal::TerminalRuntime,
    events: Vec<ClientPaneInputEvent>,
    pixel_mouse: bool,
    page_lines: Option<usize>,
) -> (Vec<ClientPaneInputEvent>, Option<String>) {
    let mut accepted = Vec::new();
    let mut failure = None;
    for event in events {
        let result = match &event {
            ClientPaneInputEvent::Paste(text) => runtime
                .try_send_paste(text.clone())
                .map_err(|error| error.to_string()),
            ClientPaneInputEvent::TextCommit(text) => runtime
                .try_send_bytes(bytes::Bytes::copy_from_slice(text.as_bytes()))
                .map_err(|error| error.to_string()),
            ClientPaneInputEvent::Key {
                code,
                modifiers,
                kind,
                ..
            } if matches!(
                code,
                crate::protocol::endpoint_wire::ClientKeyCode::PageUp
                    | crate::protocol::endpoint_wire::ClientKeyCode::PageDown
            ) && *modifiers == 0
                && page_lines.is_some()
                && runtime
                    .input_state()
                    .is_some_and(crate::pane::InputState::plain_page_keys_use_host_scrollback) =>
            {
                if *kind != crate::protocol::endpoint_wire::ClientKeyKind::Release {
                    // Preserve the fork's shell-transcript page policy using this viewer's
                    // actual geometry. Application cursor/pager keys still reach the PTY.
                    if let Some(lines) = page_lines {
                        if *code == crate::protocol::endpoint_wire::ClientKeyCode::PageUp {
                            runtime.scroll_up(lines);
                        } else {
                            runtime.scroll_down(lines);
                        }
                    }
                }
                Ok(())
            }
            ClientPaneInputEvent::Key { .. } => {
                let bytes = match crate::server::endpoint_keyboard::encode(runtime, &event) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                };
                runtime.scroll_reset();
                runtime
                    .try_send_bytes(bytes::Bytes::from(bytes))
                    .map_err(|error| error.to_string())
            }
            ClientPaneInputEvent::Mouse {
                kind,
                position,
                geometry,
                modifiers,
                lines,
            } => crate::server::endpoint_mouse::apply(
                runtime,
                *kind,
                *position,
                *geometry,
                pixel_mouse,
                *modifiers,
                *lines,
            ),
        };
        if let Err(error) = result {
            failure = Some(error);
            break;
        }
        accepted.push(event);
    }
    (accepted, failure)
}
