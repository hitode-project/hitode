use super::Event;
use adv_sync::input::KeyState;
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Default)]
pub(crate) struct Generation(AtomicU64);
#[derive(Clone, Copy)]
pub(crate) struct Stamp {
    epoch: u64,
    control: Option<u64>,
}
impl Generation {
    pub(crate) fn invalidate(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn stamp(&self, event: &Event, keys: &KeyState) -> Option<Stamp> {
        let control = if let Event::Input {
            message: message @ (0x100 | 0x101 | 0x104 | 0x105),
            wparam: 0x11,
            ..
        } = event
        {
            if (keys.get(0x11) & 0x80 != 0) != matches!(*message, 0x100 | 0x104) {
                return None;
            }
            Some(keys.observation(0x11))
        } else {
            None
        };
        Some(Stamp {
            epoch: self.0.load(Ordering::Acquire),
            control,
        })
    }
    pub(crate) fn current(&self, stamp: Stamp, keys: &KeyState) -> bool {
        stamp.epoch == self.0.load(Ordering::Acquire)
            && stamp
                .control
                .is_none_or(|observation| observation == keys.observation(0x11))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // spec: HOST-AND-3, HOST-AND-4
    #[test]
    fn pause_and_control_release_invalidate_queued_input() {
        let generation = Generation::default();
        let keys = KeyState::default();
        let event = Event::Input {
            message: 0x100,
            wparam: 0x11,
            lparam: 1,
        };
        keys.set(0x11, true);
        let stamp = generation.stamp(&event, &keys).unwrap();
        keys.set(0x11, false);
        assert!(!generation.current(stamp, &keys));
        assert!(generation.stamp(&event, &keys).is_none());
        let stamp = generation
            .stamp(
                &Event::Input {
                    message: 0x201,
                    wparam: 1,
                    lparam: 0,
                },
                &keys,
            )
            .unwrap();
        generation.invalidate();
        assert!(!generation.current(stamp, &keys));
    }
}
