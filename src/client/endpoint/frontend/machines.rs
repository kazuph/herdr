//! Machine profiles are host-owned; managing an offline profile needs no endpoint lease.
use super::*;
use crate::app::state::MenuListState;
use crate::machine::{self, MachineCatalog, MachineProfile};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};

pub(super) enum Machines {
    Context {
        profile: MachineProfile,
        x: u16,
        y: u16,
        list: MenuListState,
    },
    RemoveList {
        profiles: Vec<MachineProfile>,
        list: MenuListState,
    },
    Remove(MachineProfile),
    Add(Form),
    Probe {
        form: Form,
        result: Receiver<Result<(), String>>,
    },
}

pub(super) struct Form {
    target: String,
    label: String,
    session: String,
    field: Field,
    input: String,
    replace: bool,
}

#[derive(Clone, Copy)]
enum Field {
    Target,
    Label,
    Session,
}

fn catalog(path: &Path) -> Result<MachineCatalog, String> {
    machine::load_profiles_result(path)
        .map(|profiles| MachineCatalog { profiles })
        .map_err(|e| e.to_string())
}

fn save_add(path: &Path, form: &Form) -> Result<(), String> {
    // The SSH probe runs asynchronously; reload before saving other concurrent profile edits.
    let mut current = catalog(path)?;
    current.add(&form.label, &form.target, &form.session)?;
    machine::save_to_path(path, &current).map_err(|e| e.to_string())
}

fn save_remove(path: &Path, profile: &MachineProfile) -> Result<(), String> {
    let mut current = catalog(path)?;
    if current.profiles.iter().find(|p| p.id == profile.id) != Some(profile) {
        return Err("machine changed; reopen the menu before removing it".into());
    }
    current.remove(&profile.id);
    machine::save_to_path(path, &current).map_err(|e| e.to_string())
}

fn open(frontend: &mut ClientFrontend, page: Machines) {
    frontend.menu = None;
    frontend.context = None;
    frontend.prefix = false;
    selection::clear(frontend);
    frontend.notice = None;
    frontend.machines = Some(page);
    frontend.force_redraw = true;
}

pub(super) fn add(frontend: &mut ClientFrontend) {
    open(
        frontend,
        Machines::Add(Form {
            target: String::new(),
            label: String::new(),
            session: crate::session::DEFAULT_SESSION_NAME.into(),
            field: Field::Target,
            input: String::new(),
            replace: false,
        }),
    );
}

pub(super) fn remove(frontend: &mut ClientFrontend) {
    match catalog(&machine::default_path()) {
        Ok(catalog) if catalog.profiles.is_empty() => {
            frontend.notice = Some("No saved SSH machines.".into())
        }
        Ok(catalog) => open(
            frontend,
            Machines::RemoveList {
                profiles: catalog.profiles,
                list: MenuListState::new(0),
            },
        ),
        Err(error) => frontend.notice = Some(error),
    }
}

pub(super) fn context(frontend: &mut ClientFrontend, endpoint: &ClientEndpointId, x: u16, y: u16) {
    let ClientEndpointId::Ssh(id) = endpoint else {
        return;
    };
    match catalog(&machine::default_path()).and_then(|c| c.resolve_any(id).cloned()) {
        Ok(profile) => open(
            frontend,
            Machines::Context {
                profile,
                x,
                y,
                list: MenuListState::new(0),
            },
        ),
        Err(error) => frontend.notice = Some(error),
    }
}

impl Machines {
    fn labels(&self) -> Vec<&str> {
        match self {
            Self::Context { .. } => vec!["add machine...", "remove machine..."],
            Self::RemoveList { profiles, .. } => {
                profiles.iter().map(|p| p.label.as_str()).collect()
            }
            _ => Vec::new(),
        }
    }
    fn rect(&self, area: Rect) -> Option<Rect> {
        match self {
            Self::Context { x, y, .. } => Some(crate::app::context_menu_rect_from(
                area,
                *x,
                *y,
                &self.labels(),
            )),
            Self::RemoveList { .. } => Some(crate::app::global_menu_rect_from(
                area,
                Rect::new(area.x, area.bottom(), 0, 0),
                &self.labels(),
                |_| false,
            )),
            Self::Remove(_) => crate::ui::confirm_close_popup_rect(area),
            Self::Add(_) | Self::Probe { .. } => crate::ui::centered_popup_rect(area, 56, 7),
        }
    }
}

pub(super) fn graphics_rect(frontend: &ClientFrontend) -> Option<Rect> {
    frontend
        .machines
        .as_ref()?
        .rect(Rect::new(0, 0, frontend.cols, frontend.rows))
}

pub(super) fn render(frontend: &ClientFrontend, frame: &mut ratatui::Frame) {
    let Some(page) = &frontend.machines else {
        return;
    };
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    let palette = &frontend.chrome.settings.palette;
    match page {
        Machines::Context { list, .. } | Machines::RemoveList { list, .. } => {
            if let Some(rect) = page.rect(area) {
                crate::ui::render_global_menu_from(
                    frame,
                    rect,
                    palette,
                    &page.labels(),
                    list.highlighted,
                    |_| false,
                );
            }
        }
        Machines::Remove(profile) => crate::ui::render_confirm_close_dialog(
            frame,
            area,
            "remove machine?",
            &format!("{} — remote sessions keep running", profile.label),
            palette,
        ),
        Machines::Add(form) => crate::ui::render_rename_dialog(
            frame,
            area,
            match form.field {
                Field::Target => "add machine: SSH target (e.g. mini)",
                Field::Label => "add machine: display name (shift-tab back)",
                Field::Session => "add machine: remote session (shift-tab back)",
            },
            &form.input,
            palette,
        ),
        Machines::Probe { form, .. } => crate::ui::render_pending_input_dialog(
            frame,
            area,
            "checking SSH connection... (esc cancels)",
            &form.target,
            palette,
        ),
    }
}

pub(super) fn observe(frontend: &mut ClientFrontend) -> bool {
    let Some(Machines::Probe { result, .. }) = frontend.machines.as_ref() else {
        return false;
    };
    let done = match result.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => Err("SSH check stopped; machine was not saved".into()),
    };
    let Some(Machines::Probe { form, .. }) = frontend.machines.take() else {
        return false;
    };
    match done.and_then(|()| save_add(&machine::default_path(), &form)) {
        Ok(()) => frontend.notice = None,
        Err(error) => {
            frontend.notice = Some(error);
            frontend.machines = Some(Machines::Add(form));
        }
    }
    frontend.force_redraw = true;
    true
}

pub(super) fn input(frontend: &mut ClientFrontend, event: &RawInputEvent) -> io::Result<bool> {
    if matches!(
        event,
        RawInputEvent::OuterFocusGained
            | RawInputEvent::OuterFocusLost
            | RawInputEvent::HostDefaultColor { .. }
    ) {
        return Ok(false);
    }
    let Some(mut page) = frontend.machines.take() else {
        return Ok(false);
    };
    frontend.force_redraw = true;
    let area = Rect::new(0, 0, frontend.cols, frontend.rows);
    let rect = page.rect(area);
    let labels = page
        .labels()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut accept = false;
    let mut cancel = false;
    match event {
        RawInputEvent::Key(key) if key.kind != KeyEventKind::Release => {
            cancel = key.code == KeyCode::Esc;
            match &mut page {
                Machines::Context { list, .. } | Machines::RemoveList { list, .. } => {
                    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
                    let mut input = crate::app::ContextMenuInput {
                        list,
                        items: &labels,
                        closed: false,
                    };
                    accept = input.key(key.as_key_event()).is_some();
                    cancel = input.closed;
                }
                Machines::Add(form) => {
                    accept = key.code == KeyCode::Enter;
                    if key.code == KeyCode::BackTab {
                        match form.field {
                            Field::Target => {}
                            Field::Label => {
                                form.field = Field::Target;
                                form.input = form.target.clone();
                            }
                            Field::Session => {
                                form.field = Field::Label;
                                form.input = form.label.clone();
                            }
                        }
                        form.replace = true;
                        frontend.notice = None;
                    } else if key.code == KeyCode::Char('c')
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        crate::input::rename::clear(&mut form.input, &mut form.replace);
                    } else if !accept && !cancel {
                        crate::input::rename::edit_key(
                            &mut form.input,
                            &mut form.replace,
                            key.as_key_event(),
                        );
                    }
                }
                Machines::Remove(_) => accept = key.code == KeyCode::Enter,
                Machines::Probe { .. } => {}
            }
        }
        RawInputEvent::Paste(text) => {
            if let Machines::Add(form) = &mut page {
                crate::input::rename::insert(&mut form.input, &mut form.replace, text);
            }
        }
        RawInputEvent::Mouse(mouse) => {
            if let Some(rect) = rect {
                let inner = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .inner(rect);
                let position = (mouse.column, mouse.row).into();
                let click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
                match &mut page {
                    Machines::Context { list, .. } | Machines::RemoveList { list, .. } => {
                        if inner.contains(position) {
                            let index = usize::from(mouse.row - inner.y);
                            if index < labels.len() {
                                list.highlighted = index;
                                accept = click;
                            }
                        } else {
                            cancel = click;
                        }
                    }
                    Machines::Add(form) => {
                        let (save, clear, close) = crate::ui::rename_button_rects(inner);
                        accept = click && save.contains(position);
                        cancel = click && close.contains(position);
                        if click && clear.contains(position) {
                            crate::input::rename::clear(&mut form.input, &mut form.replace);
                        }
                    }
                    Machines::Remove(_) => {
                        let (save, close) = crate::ui::confirm_close_button_rects(inner);
                        accept = click && save.contains(position);
                        cancel = click && close.contains(position);
                    }
                    Machines::Probe { .. } => {
                        let (_, _, close) = crate::ui::rename_button_rects(inner);
                        cancel = click && close.contains(position);
                    }
                }
            }
        }
        _ => {}
    }
    if cancel {
        frontend.notice = None;
        return Ok(true);
    }
    if accept {
        match page {
            Machines::Context { profile, list, .. } => {
                if list.highlighted == 0 {
                    add(frontend);
                } else {
                    open(frontend, Machines::Remove(profile));
                }
            }
            Machines::RemoveList { profiles, list } => {
                if let Some(profile) = profiles.get(list.highlighted) {
                    open(frontend, Machines::Remove(profile.clone()));
                }
            }
            Machines::Remove(profile) => {
                if let Err(error) = save_remove(&machine::default_path(), &profile) {
                    frontend.notice = Some(error);
                }
            }
            Machines::Add(mut form) => {
                let value = form.input.trim().to_string();
                if value.is_empty() {
                    frontend.machines = Some(Machines::Add(form));
                    return Ok(true);
                }
                match form.field {
                    Field::Target => {
                        form.target = value.clone();
                        form.input = value;
                        form.field = Field::Label;
                        form.replace = true;
                        frontend.machines = Some(Machines::Add(form));
                    }
                    Field::Label => {
                        form.label = value;
                        form.input = form.session.clone();
                        form.field = Field::Session;
                        form.replace = true;
                        frontend.machines = Some(Machines::Add(form));
                    }
                    Field::Session => {
                        form.session = value;
                        let validation = catalog(&machine::default_path())
                            .and_then(|mut c| c.add(&form.label, &form.target, &form.session));
                        match validation {
                            Err(error) => {
                                frontend.notice = Some(error);
                                frontend.machines = Some(Machines::Add(form));
                            }
                            Ok(_) => {
                                let target = form.target.clone();
                                let (tx, result) = std::sync::mpsc::channel();
                                match std::thread::Builder::new()
                                    .name("machine-ssh-probe".into())
                                    .spawn(move || {
                                        let _ = tx.send(
                                            machine::probe_ssh_reachable(&target)
                                                .map_err(|e| e.to_string()),
                                        );
                                    }) {
                                    Ok(_) => {
                                        frontend.machines = Some(Machines::Probe { form, result })
                                    }
                                    Err(error) => {
                                        frontend.notice = Some(error.to_string());
                                        frontend.machines = Some(Machines::Add(form));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Machines::Probe { .. } => frontend.machines = Some(page),
        }
    } else {
        frontend.machines = Some(page);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_menu_offline_add_form_and_cancel_need_no_runtime_lease() {
        let config = crate::config::Config::default();
        let options = EndpointConnectOptions {
            surface_size: wire::ClientSurfaceSize {
                cols: 160,
                rows: 40,
            },
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: false,
        };
        let runtime = EndpointRuntime::new(
            ClientShellState::new(),
            EndpointRegistry::empty(),
            EndpointSupervisors::new(&[], Instant::now()),
            options,
        );
        let mut frontend = ClientFrontend::from_runtime(
            runtime,
            &config,
            ChromeSettings::from_config(&config, crate::app::state::Palette::catppuccin(), None),
            (160, 40),
            options,
        );
        assert!(!frontend.runtime.input_lease_current());
        menu::open(&mut frontend);
        let view = frontend
            .chrome
            .compute_view(&frontend.runtime.shell, 160, 40);
        let rendered = frontend.chrome.render_with_overlay(&view, |frame| {
            menu::render(&frontend, frame, view.menu_launcher)
        });
        let text: String = rendered
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect();
        assert!(text.contains("add machine..."));
        assert!(text.contains("remove machine..."));
        for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[A\x1b[A\r") {
            frontend.dispatch_input(event).unwrap();
        }
        assert!(matches!(frontend.machines, Some(Machines::Add(_))));
        for event in crate::raw_input::parse_raw_input_bytes_sync(b"mini\rMac mini\r") {
            frontend.dispatch_input(event).unwrap();
        }
        let Some(Machines::Add(form)) = &frontend.machines else {
            panic!("missing form")
        };
        assert_eq!(form.target, "mini");
        assert_eq!(form.label, "Mac mini");
        assert_eq!(form.input, crate::session::DEFAULT_SESSION_NAME);
        assert!(matches!(form.field, Field::Session));
        for event in crate::raw_input::parse_raw_input_bytes_sync(b"\x1b") {
            frontend.dispatch_input(event).unwrap();
        }
        assert!(frontend.machines.is_none());
        assert_eq!(
            frontend.runtime.shell.active_endpoint_id,
            ClientEndpointId::Local
        );
        assert!(!frontend.runtime.input_lease_current());
    }

    #[test]
    fn machine_menu_persistence_preserves_other_profiles_and_rejects_stale_removal() {
        let root = std::env::temp_dir().join(format!(
            "herdr-machine-menu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("machines.json");
        let form = Form {
            target: "mini".into(),
            label: "Mac mini".into(),
            session: "fork".into(),
            field: Field::Session,
            input: "fork".into(),
            replace: false,
        };
        save_add(&path, &form).unwrap();
        let original = catalog(&path).unwrap().profiles[0].clone();
        let mut edited = catalog(&path).unwrap();
        let other = edited.add("PC", "pc", "fork").unwrap();
        machine::save_to_path(&path, &edited).unwrap();
        assert!(save_add(&path, &form).is_err());
        assert_eq!(catalog(&path).unwrap(), edited);
        edited.rename(&original.id, "Renamed mini").unwrap();
        machine::save_to_path(&path, &edited).unwrap();
        assert!(save_remove(&path, &original).is_err());
        assert_eq!(catalog(&path).unwrap(), edited);
        let renamed = edited.resolve_any(&original.id).unwrap().clone();
        save_remove(&path, &renamed).unwrap();
        assert_eq!(
            catalog(&path).unwrap().profiles,
            vec![edited.resolve_any(&other).unwrap().clone()]
        );
        std::fs::write(&path, b"invalid catalog").unwrap();
        assert!(save_add(&path, &form).is_err());
        assert!(save_remove(&path, &renamed).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid catalog");
        std::fs::remove_dir_all(root).unwrap();
    }
}
