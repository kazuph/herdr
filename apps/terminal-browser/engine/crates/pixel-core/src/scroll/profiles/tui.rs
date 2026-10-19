use super::super::{ScrollProfile, ScrollState};

#[derive(Debug, Clone, Copy)]
pub struct Tui;

impl ScrollProfile for Tui {
    fn step(&self, state: &mut ScrollState, _dt: f32, _max: f32) {
        state.position = state.target;
        state.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lands_in_one_step() {
        let mut state = ScrollState::default();
        state.tick(&Tui, 100.0, 500.0);
        assert!(state.step(&Tui, 1.0 / 60.0, 500.0));
        assert_eq!(state.position, 100.0);
        assert!(state.settled());
    }
}
