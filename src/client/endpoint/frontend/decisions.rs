//! Pending-decision queue for the machine-aware client.
//!
//! Decision state is server-owned: each endpoint's decisions projection is the
//! only source, and this module only keeps presentation state (which entry is
//! shown, whether the user closed it, the free-text draft, and whether the
//! keyboard has been grabbed by clicking the dialog).
use std::collections::HashSet;

use super::*;
use crate::api::schema::{self, Decision, DecisionOrigin};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

/// Identity of one projected pending decision. The owning endpoint and its
/// connection generation are part of the key so a reconnect never confuses a
/// new server's decision with an old one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PendingKey {
    pub(super) endpoint: ClientEndpointId,
    pub(super) generation: u64,
    pub(super) decision_id: String,
}

#[derive(Clone)]
pub(super) struct PendingDecision {
    pub(super) key: PendingKey,
    pub(super) boot_id: String,
    pub(super) machine: String,
    pub(super) decision: Decision,
}

#[derive(Clone)]
pub(super) struct Dialog {
    key: PendingKey,
    /// Free-text draft for `allow_text` decisions.
    text: String,
    replace_on_type: bool,
    /// Option index highlighted for keyboard activation.
    selected: usize,
    /// Content-band scroll offset.
    scroll: u16,
    /// False until the user clicks inside the dialog; keyboard input keeps
    /// going to the pane while this is false (acceptance condition 3).
    keyboard: bool,
    /// Request id of the in-flight `decision.answer`, if any.
    answering: Option<String>,
}

#[derive(Default)]
pub(super) struct Decisions {
    pub(super) pending: Vec<PendingDecision>,
    pub(super) dialog: Option<Dialog>,
    /// Decisions the user closed without answering; they stay visible through
    /// the indicator instead of popping the dialog back up.
    snoozed: HashSet<PendingKey>,
}

impl Decisions {
    pub(super) fn current(&self) -> Option<&PendingDecision> {
        let dialog = self.dialog.as_ref()?;
        self.pending
            .iter()
            .find(|pending| pending.key == dialog.key)
    }
}

fn gather(frontend: &ClientFrontend) -> Vec<PendingDecision> {
    let mut pending = Vec::new();
    for endpoint in &frontend.runtime.shell.endpoints {
        let Some(generation) = endpoint.generation else {
            continue;
        };
        let Some(snapshot) = endpoint.cache.live_snapshot(generation) else {
            continue;
        };
        let Some(projection) = endpoint.decisions.for_presentation(snapshot) else {
            continue;
        };
        for decision in &projection.decisions {
            if decision.status != schema::DecisionStatus::Pending {
                continue;
            }
            pending.push(PendingDecision {
                key: PendingKey {
                    endpoint: endpoint.endpoint_id.clone(),
                    generation,
                    decision_id: decision.decision_id.clone(),
                },
                boot_id: snapshot.boot_id.clone(),
                machine: endpoint.label.clone(),
                decision: decision.clone(),
            });
        }
    }
    // Oldest first across all machines; ids break timestamp ties so the order
    // is stable between projections.
    pending.sort_by(|a, b| {
        a.decision
            .created_unix_ms
            .cmp(&b.decision.created_unix_ms)
            .then_with(|| a.decision.decision_id.cmp(&b.decision.decision_id))
            .then_with(|| a.machine.cmp(&b.machine))
    });
    pending
}

/// Recompute the pending set from every live endpoint projection, drop the
/// dialog when its decision resolved/disconnected, and open the oldest
/// un-snoozed decision.
pub(super) fn observe(frontend: &mut ClientFrontend) {
    let pending = gather(frontend);
    let keys: HashSet<PendingKey> = pending.iter().map(|entry| entry.key.clone()).collect();
    let open_key = frontend
        .decisions
        .dialog
        .as_ref()
        .filter(|dialog| keys.contains(&dialog.key))
        .map(|dialog| dialog.key.clone());
    frontend.decisions.pending = pending;
    frontend.decisions.snoozed.retain(|key| keys.contains(key));
    match open_key {
        // The shown decision is still pending; keep its presentation state.
        Some(key) => {
            frontend.decisions.dialog = frontend.decisions.dialog.take().map(|mut dialog| {
                dialog.key = key;
                dialog
            });
        }
        // Otherwise open the oldest pending decision the user has not closed.
        None => {
            frontend.decisions.dialog = frontend
                .decisions
                .pending
                .iter()
                .find(|pending| !frontend.decisions.snoozed.contains(&pending.key))
                .map(|pending| Dialog {
                    key: pending.key.clone(),
                    text: String::new(),
                    replace_on_type: false,
                    selected: 0,
                    scroll: 0,
                    keyboard: false,
                    answering: None,
                });
        }
    }
}

fn close(frontend: &mut ClientFrontend) {
    if let Some(dialog) = frontend.decisions.dialog.take() {
        frontend.decisions.snoozed.insert(dialog.key);
    }
}

/// The owning endpoint must still be live at the exact generation/boot the
/// decision was projected from; unlike modals, a decision's endpoint does not
/// have to be the active one.
fn pending_current(frontend: &ClientFrontend, pending: &PendingDecision) -> bool {
    frontend
        .runtime
        .shell
        .endpoint(&pending.key.endpoint)
        .is_some_and(|endpoint| {
            endpoint
                .cache
                .live_snapshot(pending.key.generation)
                .is_some_and(|snapshot| snapshot.boot_id == pending.boot_id)
        })
}

fn answer(frontend: &mut ClientFrontend, option_index: usize) -> io::Result<()> {
    let Some(dialog) = frontend.decisions.dialog.as_ref() else {
        return Ok(());
    };
    if dialog.answering.is_some() {
        return Ok(());
    }
    let (endpoint, decision_id, option_id, text) = {
        let Some(pending) = frontend.decisions.current() else {
            frontend.decisions.dialog = None;
            return Ok(());
        };
        if !pending_current(frontend, pending) {
            frontend.decisions.dialog = None;
            return Ok(());
        }
        let Some(option) = pending.decision.options.get(option_index) else {
            return Ok(());
        };
        let text = pending
            .decision
            .allow_text
            .then(|| dialog.text.trim().to_owned())
            .filter(|text| !text.is_empty());
        (
            pending.key.endpoint.clone(),
            pending.decision.decision_id.clone(),
            option.id.clone(),
            text,
        )
    };
    let method = schema::Method::DecisionAnswer(schema::DecisionAnswerParams {
        decision_id,
        option_id: Some(option_id),
        text,
        responder: Some("client".into()),
    });
    match frontend.runtime.command_on(&endpoint, method) {
        Ok((request_id, update)) => {
            if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                dialog.answering = Some(request_id);
            }
            let _ = frontend.update(update)?;
        }
        Err(error) => frontend.notice = Some(error),
    }
    Ok(())
}

pub(super) fn completed(
    frontend: &mut ClientFrontend,
    completed: &super::super::commands::EndpointCommandResult,
) {
    let Some(dialog) = frontend.decisions.dialog.as_mut() else {
        return;
    };
    if dialog.answering.as_deref() != Some(completed.request_id.as_str()) {
        return;
    }
    dialog.answering = None;
    if let Err(error) = &completed.result {
        frontend.notice = Some(error.message.clone());
    }
}

/// While the dialog grabbed the keyboard the pane cursor would mislead the
/// user about where input lands.
pub(super) fn hides_terminal_cursor(frontend: &ClientFrontend) -> bool {
    frontend
        .decisions
        .dialog
        .as_ref()
        .is_some_and(|dialog| dialog.keyboard)
}

fn origin_rows(origin: &DecisionOrigin) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut push = |label: &str, value: &Option<String>| {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            rows.push((label.to_owned(), value.to_owned()));
        }
    };
    push("pane", &origin.pane_id);
    push("agent", &origin.agent);
    push("cwd", &origin.cwd);
    push("branch", &origin.branch);
    push("command", &origin.command);
    push("reason", &origin.reason);
    rows
}

fn remaining(decision: &Decision) -> Option<String> {
    let expires = decision.expires_unix_ms?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let left = expires.saturating_sub(now) / 1000;
    Some(if left >= 120 {
        format!("{}m", left / 60)
    } else {
        format!("{left}s")
    })
}

fn facts(
    frontend: &ClientFrontend,
    pending: &PendingDecision,
) -> Option<crate::ui::DecisionDialogFacts> {
    let dialog = frontend.decisions.dialog.as_ref()?;
    let total = frontend.decisions.pending.len();
    let position = frontend
        .decisions
        .pending
        .iter()
        .position(|entry| entry.key == pending.key)
        .map_or(1, |index| index + 1);
    Some(crate::ui::DecisionDialogFacts {
        machine: pending.machine.clone(),
        position,
        total,
        title: pending.decision.title.clone(),
        body: pending.decision.body.clone(),
        origin: pending
            .decision
            .origin
            .as_ref()
            .map(origin_rows)
            .unwrap_or_default(),
        options: pending
            .decision
            .options
            .iter()
            .map(|option| option.label.clone())
            .collect(),
        selected: dialog.selected,
        allow_text: pending.decision.allow_text,
        text: dialog.text.clone(),
        remaining: remaining(&pending.decision),
        scroll: dialog.scroll,
        keyboard_active: dialog.keyboard,
    })
}

pub(super) fn dialog_rects(
    frontend: &ClientFrontend,
    pending: &PendingDecision,
) -> Option<crate::ui::DecisionDialogRects> {
    let facts = facts(frontend, pending)?;
    crate::ui::decision_dialog_rects(
        Rect::new(0, 0, frontend.cols, frontend.rows),
        &facts,
        &frontend.chrome.settings.palette,
    )
}

/// Right-aligned badge in the free span of the pane action bar while pending
/// decisions are snoozed. Returns the clickable rect and its label.
pub(super) fn indicator_rect(
    view: &super::super::chrome::ChromeView,
    count: usize,
) -> Option<(Rect, String)> {
    let free = notice::area(view)?;
    let label = format!(" ! {count} ");
    let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
    (free.width > width).then(|| (Rect::new(free.right() - width, free.y, width, 1), label))
}

fn indicator(frontend: &mut ClientFrontend) -> Option<(Rect, String)> {
    if frontend.decisions.pending.is_empty() || frontend.decisions.dialog.is_some() {
        return None;
    }
    let view = frontend
        .chrome
        .compute_view(&frontend.runtime.shell, frontend.cols, frontend.rows);
    indicator_rect(&view, frontend.decisions.pending.len())
}

pub(super) fn render_indicator(
    frontend: &ClientFrontend,
    frame: &mut ratatui::Frame,
    view: &super::super::chrome::ChromeView,
) {
    if frontend.decisions.pending.is_empty() || frontend.decisions.dialog.is_some() {
        return;
    }
    let Some((rect, label)) = indicator_rect(view, frontend.decisions.pending.len()) else {
        return;
    };
    let palette = &frontend.chrome.settings.palette;
    frame.render_widget(
        ratatui::widgets::Paragraph::new(ratatui::text::Span::styled(
            label,
            ratatui::style::Style::default()
                .fg(crate::ui::panel_contrast_fg(palette))
                .bg(palette.peach)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        rect,
    );
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame, area: Rect) {
    let Some(pending) = frontend.decisions.current() else {
        return;
    };
    let Some(facts) = facts(frontend, pending) else {
        return;
    };
    crate::ui::render_decision_dialog(frame, area, &facts, &frontend.chrome.settings.palette);
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained | RawInputEvent::OuterFocusLost
    ) {
        return Ok(false);
    }
    // The indicator reopens the dialog even while it is closed.
    if let RawInputEvent::Mouse(mouse) = event {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some((rect, _)) = indicator(frontend) {
                if rect.contains((mouse.column, mouse.row).into()) {
                    frontend.decisions.snoozed.clear();
                    observe(frontend);
                    return Ok(true);
                }
            }
        }
    }
    let Some(pending) = frontend.decisions.current().cloned() else {
        return Ok(false);
    };
    match event {
        RawInputEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(rects) = dialog_rects(frontend, &pending) else {
                    return Ok(false);
                };
                let position = (mouse.column, mouse.row).into();
                if !rects.popup.contains(position) {
                    // Clicks outside the dialog keep reaching the pane.
                    return Ok(false);
                }
                if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                    dialog.keyboard = true;
                }
                if rects.close.contains(position) {
                    close(frontend);
                } else if let Some(index) = rects
                    .options
                    .iter()
                    .position(|rect| rect.contains(position))
                {
                    answer(frontend, index)?;
                }
                Ok(true)
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let Some(rects) = dialog_rects(frontend, &pending) else {
                    return Ok(false);
                };
                let position = (mouse.column, mouse.row).into();
                if !rects.popup.contains(position) {
                    return Ok(false);
                }
                let palette = frontend.chrome.settings.palette.clone();
                let max = facts(frontend, &pending)
                    .map(|facts| {
                        crate::ui::decision_dialog_scroll_max(&facts, rects.content, &palette)
                    })
                    .unwrap_or(0);
                if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                    dialog.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                        dialog.scroll.saturating_sub(1)
                    } else {
                        (dialog.scroll + 1).min(max)
                    };
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        _ => {
            // Keys and paste keep reaching the pane until the user clicks
            // inside the dialog.
            if !frontend
                .decisions
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.keyboard)
            {
                return Ok(false);
            }
            match event {
                RawInputEvent::Key(raw) if raw.kind != KeyEventKind::Release => {
                    let key = raw.as_key_event();
                    match key.code {
                        KeyCode::Esc => close(frontend),
                        KeyCode::Up => {
                            let options = pending.decision.options.len().max(1);
                            if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                                dialog.selected = (dialog.selected + options - 1) % options;
                            }
                        }
                        KeyCode::Down => {
                            let options = pending.decision.options.len().max(1);
                            if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                                dialog.selected = (dialog.selected + 1) % options;
                            }
                        }
                        KeyCode::Enter => {
                            let Some(selected) = frontend
                                .decisions
                                .dialog
                                .as_ref()
                                .map(|dialog| dialog.selected)
                            else {
                                return Ok(true);
                            };
                            answer(frontend, selected)?;
                        }
                        KeyCode::PageUp | KeyCode::PageDown => {
                            let palette = frontend.chrome.settings.palette.clone();
                            let max = facts(frontend, &pending)
                                .map(|facts| {
                                    crate::ui::decision_dialog_scroll_max(
                                        &facts,
                                        dialog_rects(frontend, &pending)
                                            .map(|rects| rects.content)
                                            .unwrap_or_default(),
                                        &palette,
                                    )
                                })
                                .unwrap_or(0);
                            if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                                dialog.scroll = if key.code == KeyCode::PageUp {
                                    dialog.scroll.saturating_sub(4)
                                } else {
                                    (dialog.scroll + 4).min(max)
                                };
                            }
                        }
                        _ => {
                            if pending.decision.allow_text {
                                if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                                    crate::input::rename::edit_key(
                                        &mut dialog.text,
                                        &mut dialog.replace_on_type,
                                        key,
                                    );
                                }
                            }
                        }
                    }
                    Ok(true)
                }
                RawInputEvent::Paste(text) => {
                    if pending.decision.allow_text {
                        if let Some(dialog) = frontend.decisions.dialog.as_mut() {
                            crate::input::rename::insert(
                                &mut dialog.text,
                                &mut dialog.replace_on_type,
                                text,
                            );
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::TerminalKey;
    use crate::protocol::endpoint_decisions::EndpointDecisionsProjection;
    use crate::protocol::endpoint_wire::ClientShellSnapshot;
    use crossterm::event::KeyModifiers;

    fn snapshot(boot: &str, revision: u64) -> ClientShellSnapshot {
        let mut snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-snapshot-v1.json"
        )))
        .unwrap();
        snapshot.boot_id = boot.into();
        snapshot.revision = revision;
        snapshot
    }

    fn options() -> EndpointConnectOptions {
        EndpointConnectOptions {
            surface_size: wire::ClientSurfaceSize {
                cols: 140,
                rows: 30,
            },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: false,
        }
    }

    fn make_frontend() -> ClientFrontend {
        let config = crate::config::Config::default();
        let mut shell = ClientShellState::new();
        shell.begin_connection(&ClientEndpointId::Local, 1);
        shell.receive_snapshot(&ClientEndpointId::Local, 1, snapshot("boot-local", 1));
        let options = options();
        let runtime = EndpointRuntime::new(
            shell,
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        ClientFrontend::from_runtime(
            runtime,
            &config,
            ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None),
            (140, 30),
            options,
        )
    }

    fn add_remote(frontend: &mut ClientFrontend) {
        let profiles = vec![crate::machine::MachineProfile {
            id: "remote".into(),
            label: "Remote Mac".into(),
            target: "ssh://remote".into(),
            session: "herdr".into(),
            enabled: true,
        }];
        frontend.runtime.shell.set_endpoint_catalog(&profiles);
        let remote = ClientEndpointId::Ssh("remote".into());
        frontend.runtime.shell.begin_connection(&remote, 1);
        frontend
            .runtime
            .shell
            .receive_snapshot(&remote, 1, snapshot("boot-remote", 1));
    }

    fn decision(id: &str, created_unix_ms: u64) -> Decision {
        Decision {
            decision_id: id.into(),
            kind: schema::DecisionKind::Ask,
            title: format!("title {id}"),
            body: Some(format!("body {id}")),
            options: vec![
                schema::DecisionOption {
                    id: "yes".into(),
                    label: "Yes".into(),
                    role: schema::DecisionOptionRole::Approve,
                },
                schema::DecisionOption {
                    id: "no".into(),
                    label: "No".into(),
                    role: schema::DecisionOptionRole::Reject,
                },
            ],
            allow_text: false,
            origin: None,
            created_unix_ms,
            expires_unix_ms: None,
            status: schema::DecisionStatus::Pending,
            answer: None,
        }
    }

    fn set_projection(
        frontend: &mut ClientFrontend,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        decisions: Vec<Decision>,
    ) {
        let endpoint = frontend
            .runtime
            .shell
            .endpoint_mut(endpoint_id)
            .expect("endpoint");
        let snapshot = endpoint
            .cache
            .live_snapshot(generation)
            .expect("live snapshot");
        let projection = EndpointDecisionsProjection {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            decisions,
        };
        assert!(endpoint
            .decisions
            .replace(generation, &endpoint.cache, projection));
    }

    fn set_pending(
        frontend: &mut ClientFrontend,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        decisions: Vec<Decision>,
    ) {
        set_projection(frontend, endpoint_id, generation, decisions);
        observe(frontend);
    }

    fn mouse_down(column: u16, row: u16) -> RawInputEvent {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }

    fn key(code: KeyCode) -> RawInputEvent {
        RawInputEvent::Key(TerminalKey::new(code, KeyModifiers::empty()))
    }

    #[test]
    fn endpoint_decision_shows_oldest_pending_across_machines_with_queue_position() {
        let mut frontend = make_frontend();
        add_remote(&mut frontend);
        let remote = ClientEndpointId::Ssh("remote".into());
        set_projection(&mut frontend, &remote, 1, vec![decision("new", 20)]);
        set_projection(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("old", 10)],
        );
        observe(&mut frontend);
        assert_eq!(frontend.decisions.pending[0].decision.decision_id, "old");
        assert_eq!(frontend.decisions.pending[1].decision.decision_id, "new");
        let pending = frontend.decisions.current().expect("dialog decision");
        assert_eq!(pending.decision.decision_id, "old");
        assert_eq!(pending.machine, "Local");
        let facts = facts(&frontend, pending).expect("facts");
        assert_eq!(facts.position, 1);
        assert_eq!(facts.total, 2);
    }

    #[test]
    fn endpoint_decision_close_snoozes_and_indicator_click_reopens() {
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        assert!(frontend.decisions.dialog.is_some());
        assert!(indicator(&mut frontend).is_none());
        close(&mut frontend);
        assert!(frontend.decisions.dialog.is_none());
        let Some((rect, _)) = indicator(&mut frontend) else {
            panic!("indicator must remain while a decision is pending");
        };
        // A fresh observe does not reopen the snoozed decision.
        observe(&mut frontend);
        assert!(frontend.decisions.dialog.is_none());
        assert!(input(&mut frontend, &mouse_down(rect.x, rect.y)).unwrap());
        assert!(frontend.decisions.dialog.is_some());
    }

    #[test]
    fn endpoint_decision_keyboard_stays_on_pane_until_dialog_clicked() {
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        assert!(!hides_terminal_cursor(&frontend));
        // Keyboard input is not consumed before the dialog is clicked.
        assert!(!input(&mut frontend, &key(KeyCode::Esc)).unwrap());
        assert!(!input(&mut frontend, &key(KeyCode::Enter)).unwrap());
        assert!(frontend.decisions.dialog.is_some());
        let pending = frontend.decisions.current().cloned().expect("decision");
        let rects = dialog_rects(&frontend, &pending).expect("rects");
        assert!(input(
            &mut frontend,
            &mouse_down(rects.popup.x + 1, rects.popup.y + 1)
        )
        .unwrap());
        assert!(hides_terminal_cursor(&frontend));
        // Esc now closes instead of reaching the pane.
        assert!(input(&mut frontend, &key(KeyCode::Esc)).unwrap());
        assert!(frontend.decisions.dialog.is_none());
    }

    #[test]
    fn endpoint_decision_option_click_and_enter_route_the_answer() {
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        let pending = frontend.decisions.current().cloned().expect("decision");
        let rects = dialog_rects(&frontend, &pending).expect("rects");
        // Without a live endpoint connection the send fails, but the click must
        // still be consumed by the option row rather than reaching the pane.
        assert!(input(
            &mut frontend,
            &mouse_down(rects.options[0].x, rects.options[0].y)
        )
        .unwrap());
        assert!(frontend.notice.is_some());
        frontend.notice = None;
        // Enter after grabbing the keyboard answers the highlighted option.
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        let pending = frontend.decisions.current().cloned().expect("decision");
        let rects = dialog_rects(&frontend, &pending).expect("rects");
        input(
            &mut frontend,
            &mouse_down(rects.popup.x + 1, rects.popup.y + 1),
        )
        .unwrap();
        assert!(input(&mut frontend, &key(KeyCode::Down)).unwrap());
        assert_eq!(frontend.decisions.dialog.as_ref().unwrap().selected, 1);
        assert!(input(&mut frontend, &key(KeyCode::Enter)).unwrap());
        assert!(frontend.notice.is_some());
    }

    #[test]
    fn endpoint_decision_resolved_elsewhere_and_disconnect_remove_it() {
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        assert!(frontend.decisions.dialog.is_some());
        // The server resolves it elsewhere: the next projection no longer lists it.
        set_pending(&mut frontend, &ClientEndpointId::Local, 1, Vec::new());
        assert!(frontend.decisions.dialog.is_none());
        assert!(frontend.decisions.pending.is_empty());
        // A disconnected machine's decisions leave the queue entirely.
        let mut frontend = make_frontend();
        add_remote(&mut frontend);
        let remote = ClientEndpointId::Ssh("remote".into());
        set_pending(&mut frontend, &remote, 1, vec![decision("r", 1)]);
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("l", 2)],
        );
        assert_eq!(frontend.decisions.pending.len(), 2);
        frontend.runtime.shell.disconnect(&remote, 1);
        observe(&mut frontend);
        assert_eq!(frontend.decisions.pending.len(), 1);
        assert_eq!(frontend.decisions.pending[0].decision.decision_id, "l");
        // Reconnecting re-opens that machine's decisions.
        frontend.runtime.shell.begin_connection(&remote, 2);
        frontend
            .runtime
            .shell
            .receive_snapshot(&remote, 2, snapshot("boot-remote", 3));
        set_pending(&mut frontend, &remote, 2, vec![decision("r2", 0)]);
        assert_eq!(frontend.decisions.pending.len(), 2);
    }

    #[test]
    fn endpoint_decision_renders_machine_title_options_and_count() {
        let mut frontend = make_frontend();
        let mut with_origin = decision("a", 1);
        with_origin.origin = Some(DecisionOrigin {
            pane_id: Some("p1".into()),
            agent: Some("claude".into()),
            cwd: Some("/tmp/work".into()),
            branch: Some("main".into()),
            command: Some("rm -rf build".into()),
            reason: Some("destructive".into()),
            machine: None,
        });
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![with_origin],
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| render(&frontend, frame, Rect::new(0, 0, 100, 30)))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for needle in [
            "decision",
            "Local",
            "1/1",
            "title a",
            "body a",
            "pane: p1",
            "agent: claude",
            "command: rm -rf build",
            "reason: destructive",
            "Yes",
            "No",
            "close",
        ] {
            assert!(text.contains(needle), "missing {needle:?}\n{text}");
        }
    }

    #[test]
    fn endpoint_decision_input_after_pending_drop_never_panics() {
        let mut frontend = make_frontend();
        set_pending(
            &mut frontend,
            &ClientEndpointId::Local,
            1,
            vec![decision("a", 1)],
        );
        let pending = frontend.decisions.current().cloned().expect("decision");
        let rects = dialog_rects(&frontend, &pending).expect("rects");
        input(
            &mut frontend,
            &mouse_down(rects.popup.x + 1, rects.popup.y + 1),
        )
        .unwrap();
        // The decision leaves the queue while the dialog is up; every input
        // path must degrade to a no-op instead of expecting a dialog.
        set_pending(&mut frontend, &ClientEndpointId::Local, 1, Vec::new());
        assert!(frontend.decisions.dialog.is_none());
        for event in [
            key(KeyCode::Esc),
            key(KeyCode::Down),
            key(KeyCode::Enter),
            key(KeyCode::PageDown),
            RawInputEvent::Paste("text".into()),
            mouse_down(10, 10),
        ] {
            input(&mut frontend, &event).unwrap();
        }
        // Rendering with pending state but no dialog must be a no-op too.
        frontend.decisions.dialog = Some(Dialog {
            key: PendingKey {
                endpoint: ClientEndpointId::Local,
                generation: 1,
                decision_id: "gone".into(),
            },
            text: String::new(),
            selected: 0,
            scroll: 0,
            keyboard: true,
            replace_on_type: false,
            answering: None,
        });
        // The stale dialog never reaches the renderer: current() is None, so
        // render/facts/dialog_rects all return early instead of unwrapping.
        assert!(frontend.decisions.current().is_none());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| render(&frontend, frame, Rect::new(0, 0, 100, 30)))
            .unwrap();
    }
}
