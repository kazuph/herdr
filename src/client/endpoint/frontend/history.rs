//! Fork focus history belongs to the client; opaque panes remain on their endpoint.
use super::*;
use crate::app::NavigateAction;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    endpoint: ClientEndpointId,
    boot: String,
    pane: String,
}

#[derive(Clone, Copy)]
enum Direction {
    Back,
    Forward,
}

#[derive(Default)]
pub(super) struct FocusHistory {
    current: Option<Target>,
    previous: Option<Target>,
    back: Vec<Target>,
    forward: Vec<Target>,
    pending: Option<(Direction, Target)>,
}

impl FocusHistory {
    fn observe(&mut self, target: Target) {
        if self.current.as_ref() == Some(&target) {
            return;
        }
        let previous = self.current.replace(target.clone());
        let movement = self
            .pending
            .take()
            .filter(|(_, expected)| *expected == target);
        if let Some(previous) = previous {
            self.previous = Some(previous.clone());
            match movement {
                Some((Direction::Back, _)) => {
                    if let Some(index) = self.back.iter().rposition(|entry| entry == &target) {
                        self.back.truncate(index);
                    }
                    self.forward.push(previous);
                }
                Some((Direction::Forward, _)) => {
                    if let Some(index) = self.forward.iter().rposition(|entry| entry == &target) {
                        self.forward.truncate(index);
                    }
                    self.back.push(previous);
                }
                None => {
                    if self.back.last() != Some(&previous) {
                        self.back.push(previous);
                    }
                    self.forward.clear();
                }
            }
        }
    }

    fn navigate(
        &mut self,
        direction: Direction,
        exists: impl Fn(&Target) -> bool,
    ) -> Option<Target> {
        let entries = match direction {
            Direction::Back => &self.back,
            Direction::Forward => &self.forward,
        };
        let target = entries
            .iter()
            .rev()
            .find(|target| Some(*target) != self.current.as_ref() && exists(target))?
            .clone();
        // Failed or abandoned activation must not consume a history entry.
        self.pending = Some((direction, target.clone()));
        Some(target)
    }

    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }
}

fn exists(shell: &ClientShellState, target: &Target) -> bool {
    shell.endpoint(&target.endpoint).is_some_and(|endpoint| {
        endpoint.generation.is_some_and(|generation| {
            endpoint
                .cache
                .live_snapshot(generation)
                .is_some_and(|snapshot| {
                    snapshot.boot_id == target.boot
                        && snapshot
                            .panes
                            .iter()
                            .any(|pane| pane.pane_id == target.pane)
                })
        })
    })
}

pub(super) fn observe(frontend: &mut ClientFrontend) {
    if !frontend.runtime.input_lease_current() {
        return;
    }
    let Some(surface) = frontend.runtime.shell.pane_surface.as_ref() else {
        return;
    };
    let Some(pane) = surface.panes.iter().find(|pane| pane.focused) else {
        return;
    };
    frontend.focus_history.observe(Target {
        endpoint: frontend.runtime.shell.active_endpoint_id.clone(),
        boot: surface.boot_id.clone(),
        pane: pane.pane_id.clone(),
    });
}

pub(super) fn action(frontend: &mut ClientFrontend, action: NavigateAction) -> io::Result<bool> {
    if !matches!(
        action,
        NavigateAction::FocusHistoryBack
            | NavigateAction::FocusHistoryForward
            | NavigateAction::LastPane
    ) {
        return Ok(false);
    }
    if !frontend.runtime.input_lease_current() {
        return Ok(true);
    }
    let shell = &frontend.runtime.shell;
    let target = match action {
        NavigateAction::FocusHistoryBack => frontend
            .focus_history
            .navigate(Direction::Back, |target| exists(shell, target)),
        NavigateAction::FocusHistoryForward => frontend
            .focus_history
            .navigate(Direction::Forward, |target| exists(shell, target)),
        NavigateAction::LastPane => {
            frontend.focus_history.cancel();
            frontend
                .focus_history
                .previous
                .clone()
                .filter(|target| exists(shell, target))
        }
        _ => None,
    };
    if let Some(target) = target {
        let update = frontend.runtime.activate(
            target.endpoint,
            Some(super::super::FocusTarget::Pane(target.pane)),
            Instant::now(),
        );
        frontend.update(update)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn local(pane: &str) -> Target {
        Target {
            endpoint: ClientEndpointId::Local,
            boot: "owned-boot".into(),
            pane: pane.into(),
        }
    }
    fn step(history: &mut FocusHistory, direction: Direction) -> Target {
        let target = history.navigate(direction, |_| true).unwrap();
        history.observe(target.clone());
        target
    }
    #[test]
    fn history_back_forward_and_normal_focus_preserve_fork_stacks() {
        let mut history = FocusHistory::default();
        for pane in ["p1", "p2", "p3"] {
            history.observe(local(pane));
        }
        assert_eq!(step(&mut history, Direction::Back), local("p2"));
        assert_eq!(step(&mut history, Direction::Back), local("p1"));
        assert_eq!(step(&mut history, Direction::Forward), local("p2"));
        assert_eq!(step(&mut history, Direction::Forward), local("p3"));
        step(&mut history, Direction::Back);
        history.observe(local("p4"));
        assert!(history.navigate(Direction::Forward, |_| true).is_none());
    }
    #[test]
    fn history_skips_closed_panes_and_failed_activation_keeps_entries() {
        let mut history = FocusHistory::default();
        for pane in ["p1", "p2", "p3"] {
            history.observe(local(pane));
        }
        assert_eq!(
            history.navigate(Direction::Back, |target| target.pane != "p2"),
            Some(local("p1"))
        );
        history.cancel();
        assert_eq!(history.back, vec![local("p1"), local("p2")]);
        let target = history
            .navigate(Direction::Back, |target| target.pane != "p2")
            .unwrap();
        history.observe(target);
        assert!(history.back.is_empty());
        assert_eq!(history.forward, vec![local("p3")]);
    }
    #[test]
    fn history_same_opaque_panes_on_distinct_endpoints_toggle_without_aliasing() {
        let mut history = FocusHistory::default();
        let left = local("p1");
        let right = Target {
            endpoint: ClientEndpointId::Ssh("other".into()),
            ..left.clone()
        };
        history.observe(left.clone());
        history.observe(right.clone());
        assert_eq!(history.previous, Some(left.clone()));
        history.observe(left.clone());
        assert_eq!(history.previous, Some(right.clone()));
        history.observe(left);
        assert_eq!(history.previous, Some(right));
    }
}
