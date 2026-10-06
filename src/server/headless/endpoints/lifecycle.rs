use super::*;

impl HeadlessServer {
    pub(super) fn close_endpoint_popup(&mut self, pane_id: crate::layout::PaneId) -> bool {
        let Some(popup) = self.app.state.popup_pane_by_pane_id(pane_id) else {
            return false;
        };
        let terminal_id = popup.terminal_id.clone();
        let focused = self
            .endpoint_clients
            .iter()
            .filter_map(|(&id, client)| {
                (client.active
                    && client.outer_focus == Some(true)
                    && self.endpoint_focused_runtime(id).is_some_and(|runtime| {
                        self.app
                            .terminal_runtimes
                            .get(&terminal_id)
                            .is_some_and(|popup| std::ptr::eq(runtime, popup))
                    }))
                .then_some(id)
            })
            .collect::<Vec<_>>();
        if !focused.is_empty() {
            if let Some(runtime) = self.app.terminal_runtimes.get(&terminal_id) {
                runtime.try_send_focus_event(crate::ghostty::FocusEvent::Lost);
            }
        }
        if !self.app.close_popup_pane_by_pane_id(pane_id) {
            return false;
        }
        let mut notified = Vec::new();
        for id in focused {
            if let Some(runtime) = self.endpoint_focused_runtime(id) {
                if !notified.iter().any(|other| std::ptr::eq(*other, runtime)) {
                    runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained);
                    notified.push(runtime);
                }
            }
        }
        true
    }

    pub(super) fn endpoint_focused_runtime(
        &self,
        client_id: u64,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        let target = self.endpoint_target(client_id)?;
        if let Some(popup) = self
            .app
            .state
            .popup_pane_for_workspace(&self.app.state.workspaces.get(target.workspace_index)?.id)
        {
            return self.app.terminal_runtimes.get(&popup.terminal_id);
        }
        self.app.state.runtime_for_pane_in_workspace(
            &self.app.terminal_runtimes,
            target.workspace_index,
            target.pane_focus?,
        )
    }

    fn another_focused_endpoint(
        &self,
        client_id: u64,
        runtime: &crate::terminal::TerminalRuntime,
    ) -> bool {
        self.endpoint_clients.iter().any(|(&other_id, client)| {
            other_id != client_id
                && client.active
                && client.outer_focus == Some(true)
                && self
                    .endpoint_focused_runtime(other_id)
                    .is_some_and(|other| std::ptr::eq(other, runtime))
        })
    }

    pub(super) fn send_endpoint_focus(&self, client_id: u64, focused: bool) {
        let Some(runtime) = self.endpoint_focused_runtime(client_id) else {
            return;
        };
        if self.another_focused_endpoint(client_id, runtime) {
            return;
        }
        runtime.try_send_focus_event(if focused {
            crate::ghostty::FocusEvent::Gained
        } else {
            crate::ghostty::FocusEvent::Lost
        });
    }

    pub(super) fn claim_endpoint_geometry(&mut self, client_id: u64) -> bool {
        let Some(client) = self.endpoint_clients.get_mut(&client_id) else {
            return false;
        };
        if !client.active {
            return false;
        }
        let Some(tab) = client.location.focused_tab_id() else {
            return false;
        };
        if self.endpoint_tab_geometry.get(tab) == Some(&client_id) {
            return false;
        }
        self.endpoint_tab_geometry.insert(tab.into(), client_id);
        client.surface = None;
        client.write_pending = true;
        true
    }

    pub(super) fn set_endpoint_focus(&mut self, client_id: u64, focused: bool) -> bool {
        let stamp = focused.then(|| self.allocate_activity_stamp());
        let Some(client) = self.endpoint_clients.get_mut(&client_id) else {
            return false;
        };
        if client.outer_focus == Some(focused) {
            // Another device can become active without this terminal reporting focus loss.
            return focused && self.claim_endpoint_geometry(client_id);
        }
        client.outer_focus = Some(focused);
        if let Some(stamp) = stamp {
            client.last_activity = stamp;
        }
        if !client.active {
            return false;
        }
        if focused {
            if let Some(tab) = client.location.focused_tab_id() {
                self.endpoint_tab_geometry.insert(tab.into(), client_id);
                client.surface = None;
                client.write_pending = true;
            }
        }
        self.send_endpoint_focus(client_id, focused);
        true
    }

    /// A new activation always advances the projection floor, including active-to-active.
    pub(super) fn set_endpoint_surface_active(
        &mut self,
        client_id: u64,
        active: bool,
    ) -> Option<u64> {
        let stamp = active.then(|| self.allocate_activity_stamp());
        #[cfg(unix)]
        self.retire_endpoint_native(client_id);
        self.retire_direct_graphics_for_client(client_id);
        let client = self.endpoint_clients.get_mut(&client_id)?;
        let changed = client.active != active;
        if !changed && !active {
            return Some(client.projection_revision);
        }
        let was_focused = client.outer_focus == Some(true);
        let held = if active {
            Vec::new()
        } else {
            client.held_inputs.drain()
        };
        client.active = active;
        if let Some(stamp) = stamp {
            client.last_activity = stamp;
        }
        client.presentation_committed = false;
        client.sent_presentation = None;
        client.surface = None;
        client.write_pending = true;
        // The host retires the old endpoint's images when switching its active scope.
        client.graphics_delivery = Default::default();
        if active {
            client.projection_revision = client.projection_revision.saturating_add(1);
            client.snapshot = None;
        } else {
            client.writer.discard_pending_render();
        }
        let revision = client.projection_revision;
        let tab_id = client.location.focused_tab_id().map(str::to_owned);
        if active {
            if let Some(tab_id) = tab_id {
                if !self.endpoint_clients.iter().any(|(&other_id, other)| {
                    other_id != client_id
                        && other.active
                        && other.outer_focus == Some(true)
                        && other.location.focused_tab_id() == Some(tab_id.as_str())
                }) {
                    self.endpoint_tab_geometry.insert(tab_id, client_id);
                }
            }
        } else {
            self.endpoint_tab_geometry
                .retain(|_, owner| *owner != client_id);
            self.release_endpoint_inputs(client_id, held);
        }
        if changed && was_focused {
            self.send_endpoint_focus(client_id, active);
        }
        Some(revision)
    }

    pub(super) fn release_endpoint_inputs(
        &self,
        client_id: u64,
        held: Vec<crate::server::endpoint_input::HeldInput>,
    ) {
        let pixel_mouse = self
            .endpoint_clients
            .get(&client_id)
            .is_some_and(|client| client.pixel_mouse);
        for held in held {
            let runtime = self
                .app
                .state
                .popup_panes
                .iter()
                .find(|popup| popup.terminal_id.to_string() == held.pane_id)
                .and_then(|popup| self.app.terminal_runtimes.get(&popup.terminal_id))
                .or_else(|| {
                    let (workspace, pane) = self.app.parse_pane_id(&held.pane_id)?;
                    self.app.state.runtime_for_pane_in_workspace(
                        &self.app.terminal_runtimes,
                        workspace,
                        pane,
                    )
                });
            let Some(runtime) = runtime else {
                continue;
            };
            let result = match held.release {
                event @ ClientPaneInputEvent::Key { .. } => {
                    crate::server::endpoint_keyboard::encode(runtime, &event).and_then(|bytes| {
                        runtime
                            .try_send_bytes(Bytes::from(bytes))
                            .map_err(|error| error.to_string())
                    })
                }
                ClientPaneInputEvent::Mouse {
                    kind,
                    position,
                    geometry,
                    modifiers,
                    lines,
                } => crate::server::endpoint_mouse::apply(
                    runtime,
                    kind,
                    position,
                    geometry,
                    pixel_mouse,
                    modifiers,
                    lines,
                ),
                _ => continue,
            };
            if let Err(error) = result {
                warn!(client_id, pane_id = %held.pane_id, %error, "failed to release endpoint-owned input");
            }
        }
    }

    /// Effects and the echoed token share one reliable writer item, after a coherent surface.
    pub(super) fn sync_endpoint_presentation(
        &mut self,
        client_id: u64,
        token: Option<String>,
    ) -> bool {
        let Some(client) = self.endpoint_clients.get(&client_id) else {
            return false;
        };
        if !client.active
            || (!client.presentation_committed && token.is_none())
            || !client.surface.as_ref().is_some_and(|surface| {
                surface.boot_id == self.endpoint_boot_id
                    && surface.projection_revision == client.projection_revision
                    && surface.frame.width == client.size.cols
                    && surface.frame.height == client.size.rows
            })
        {
            return false;
        }
        let runtime = self.endpoint_focused_runtime(client_id);
        let input = runtime.and_then(crate::terminal::TerminalRuntime::input_state);
        let mouse = client.mouse_capture
            || input.is_some_and(crate::pane::InputState::mouse_reporting_enabled);
        let sgr_pixels = client.pixel_mouse
            && input.is_some_and(|input| {
                input.mouse_protocol_encoding == crate::input::MouseProtocolEncoding::SgrPixels
            });
        let report_all = runtime.is_some_and(|runtime| {
            let protocol = runtime.keyboard_protocol();
            matches!(protocol, crate::input::KeyboardProtocol::Kitty { flags } if flags & crate::server::endpoint_keyboard::KITTY_FLAG_REPORT_ALL_KEYS != 0)
                || (protocol.reports_event_types() && input.is_some_and(|input| input.modify_other_keys))
        });
        let state = (mouse, sgr_pixels, report_all);
        if token.is_none() && client.sent_presentation == Some(state) {
            return false;
        }
        let mut batch = Vec::new();
        for message in [
            wire::ServerMessage::MouseCapture {
                enabled: mouse,
                sgr_pixels,
            },
            wire::ServerMessage::ClientShellKeyboardReportAll {
                enabled: report_all,
            },
        ] {
            let Ok(bytes) = framed(&message) else {
                return false;
            };
            batch.extend(bytes);
        }
        if let Some(token) = token {
            // The fork has API-owned titles and no automatic ui.window_title setting.
            // Restore the host default on commit; targeted title operations remain client-local.
            let Ok(bytes) = framed(&wire::ServerMessage::WindowTitle { title: None }) else {
                return false;
            };
            batch.extend(bytes);
            let Ok(bytes) = framed(&wire::ServerMessage::EndpointControl {
                kind: crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND.into(),
                data: token,
            }) else {
                return false;
            };
            batch.extend(bytes);
        }
        let client = self
            .endpoint_clients
            .get_mut(&client_id)
            .expect("live client");
        if client.writer.control.send_after_render(batch).is_err() {
            return false;
        }
        client.presentation_committed = true;
        client.sent_presentation = Some(state);
        true
    }
}
