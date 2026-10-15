//! Mobile execution/audio gate. Interruption recovery never overrides a pause.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum State {
    Foreground,
    #[default]
    Background,
    Suspended,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Lifecycle {
    state: State,
    resume_state: State,
    audio_focus: bool,
    overlay_pause: bool,
}

impl Lifecycle {
    pub fn state(&self) -> State {
        self.state
    }
    pub fn foreground(&self) -> bool {
        self.state == State::Foreground
    }
    pub fn paused(&self) -> bool {
        !self.foreground() || !self.audio_focus || self.overlay_pause
    }
    pub fn set_resumed(&mut self, resumed: bool) {
        let state = if resumed {
            State::Foreground
        } else {
            State::Background
        };
        if self.state == State::Suspended {
            self.resume_state = state;
        } else {
            self.state = state;
        }
    }
    pub fn suspend(&mut self) {
        if self.state != State::Suspended {
            self.resume_state = self.state;
            self.state = State::Suspended;
        }
    }
    pub fn resume(&mut self) {
        if self.state == State::Suspended {
            self.state = self.resume_state;
        }
    }
    /// False begins an audio interruption; only an explicit focus grant ends it.
    pub fn set_audio_focus(&mut self, focused: bool) {
        self.audio_focus = focused;
    }
    pub fn set_overlay_pause(&mut self, paused: bool) {
        self.overlay_pause = paused;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: AND-APP-3, AND-APP-15
    #[test]
    fn every_gate_matches_android_pause_and_audio_policy() {
        for foreground in [false, true] {
            for focused in [false, true] {
                for overlay in [false, true] {
                    let mut lifecycle = Lifecycle::default();
                    assert!(lifecycle.paused());
                    lifecycle.set_resumed(foreground);
                    lifecycle.set_audio_focus(focused);
                    lifecycle.set_overlay_pause(overlay);
                    assert_eq!(lifecycle.paused(), !foreground || !focused || overlay);
                    lifecycle.suspend();
                    assert!(lifecycle.paused());
                    lifecycle.suspend();
                    lifecycle.resume();
                    assert_eq!(lifecycle.foreground(), foreground);
                    assert_eq!(lifecycle.paused(), !foreground || !focused || overlay);
                }
            }
        }
    }
    // spec: AND-APP-15
    #[test]
    fn interruption_recovery_does_not_resume_background_or_overlay() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.set_resumed(true);
        lifecycle.set_audio_focus(true);
        assert!(!lifecycle.paused());
        lifecycle.set_audio_focus(false);
        assert!(lifecycle.paused());
        lifecycle.set_resumed(false);
        lifecycle.set_audio_focus(true);
        assert!(lifecycle.paused());
        lifecycle.set_resumed(true);
        lifecycle.set_overlay_pause(true);
        lifecycle.set_audio_focus(false);
        lifecycle.set_audio_focus(true);
        assert!(lifecycle.paused());
        lifecycle.suspend();
        lifecycle.set_resumed(false);
        assert_eq!(lifecycle.state(), State::Suspended);
        lifecycle.resume();
        assert_eq!(lifecycle.state(), State::Background);
        lifecycle.set_overlay_pause(false);
        assert!(lifecycle.paused());
        lifecycle.set_resumed(true);
        assert!(!lifecycle.paused());
    }
}
