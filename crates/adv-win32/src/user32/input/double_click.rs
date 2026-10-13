//! Guest USER double-click policy. Only physical input is classified; posted
//! guest messages retain their supplied type. Timing uses frontend wall time,
//! independently of guest execution speed, fast-forward and virtual clocks.
use std::time::{Duration, Instant};

pub(in crate::user32) struct DoubleClick {
    pub time_ms: u32,
    pub width: u32,
    pub height: u32,
    last: Option<Click>,
}

struct Click {
    hwnd: u32,
    button: u32,
    position: (i32, i32),
    time: Instant,
    released: bool,
}

impl Default for DoubleClick {
    fn default() -> Self {
        Self {
            time_ms: 500,
            width: 4,
            height: 4,
            last: None,
        }
    }
}

impl DoubleClick {
    pub fn set_time(&mut self, milliseconds: u32) {
        self.time_ms = if milliseconds == 0 {
            500
        } else {
            milliseconds.min(5000)
        };
    }

    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn forget_window(&mut self, hwnd: u32) {
        if self.last.as_ref().is_some_and(|last| last.hwnd == hwnd) {
            self.reset();
        }
    }

    pub fn message(
        &mut self,
        hwnd: u32,
        style: u32,
        message: u32,
        wparam: u32,
        position: (i32, i32),
        time: Instant,
    ) -> u32 {
        let (button, down, up) = match message {
            0x201..=0x203 => (1, 0x201, 0x202),
            0x204..=0x206 => (2, 0x204, 0x205),
            0x207..=0x209 => (4, 0x207, 0x208),
            0x20b..=0x20d => (4 + (wparam >> 16), 0x20b, 0x20c),
            _ => return message,
        };
        if message == up {
            if let Some(last) = self.last.as_mut() {
                if last.hwnd == hwnd && last.button == button {
                    last.released = true;
                } else {
                    self.reset();
                }
            }
            return message;
        }
        if message != down || style & 8 == 0 {
            // CS_DBLCLKS
            self.reset();
            return message;
        }
        // Half-open centered rectangles, with widened arithmetic at desktop edges.
        let near = |value: i32, origin: i32, extent: u32| {
            let delta = i64::from(value) - i64::from(origin);
            let lower = -(i64::from(extent) / 2);
            delta >= lower && delta < lower + i64::from(extent)
        };
        let double = self.last.as_ref().is_some_and(|last| {
            last.released
                && last.hwnd == hwnd
                && last.button == button
                && time
                    .checked_duration_since(last.time)
                    .is_some_and(|elapsed| {
                        elapsed <= Duration::from_millis(u64::from(self.time_ms))
                    })
                && near(position.0, last.position.0, self.width)
                && near(position.1, last.position.1, self.height)
        });
        if double {
            // Four clicks form two double-click sequences, never a triple click.
            self.reset();
            down + 2
        } else {
            self.last = Some(Click {
                hwnd,
                button,
                position,
                time,
                released: false,
            });
            message
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // spec: WIN32-USER-8
    fn each_button_forms_two_double_clicks_and_requires_an_up() {
        for (down, wparam) in [
            (0x201, 1),
            (0x204, 2),
            (0x207, 0x10),
            (0x20b, 0x10020),
            (0x20b, 0x20020),
        ] {
            let mut clicks = DoubleClick::default();
            let time = Instant::now();
            for expected in [down, down + 2, down, down + 2] {
                assert_eq!(
                    clicks.message(10, 8, down, wparam, (20, 30), time),
                    expected
                );
                assert_eq!(
                    clicks.message(10, 8, down + 1, wparam, (20, 30), time),
                    down + 1
                );
            }
            assert_eq!(clicks.message(10, 8, down, wparam, (20, 30), time), down);
            assert_eq!(clicks.message(10, 8, down, wparam, (20, 30), time), down);
        }
    }

    #[test]
    // spec: WIN32-USER-8
    fn classification_respects_class_window_button_time_and_rectangle() {
        let time = Instant::now();
        for (hwnd, style, down, wparam, position, elapsed, expected) in [
            (10, 8, 0x201, 1, (18, 31), 500, 0x203),
            (10, 8, 0x201, 1, (22, 30), 50, 0x201),
            (10, 8, 0x201, 1, (20, 28), 50, 0x203),
            (10, 8, 0x201, 1, (20, 32), 50, 0x201),
            (10, 8, 0x201, 1, (20, 30), 501, 0x201),
            (11, 8, 0x201, 1, (20, 30), 50, 0x201),
            (10, 0, 0x201, 1, (20, 30), 50, 0x201),
            (10, 8, 0x204, 2, (20, 30), 50, 0x204),
            (10, 8, 0x20b, 0x20020, (20, 30), 50, 0x20b),
        ] {
            let mut clicks = DoubleClick::default();
            clicks.message(10, 8, 0x201, 1, (20, 30), time);
            clicks.message(10, 8, 0x202, 0, (20, 30), time);
            assert_eq!(
                clicks.message(
                    hwnd,
                    style,
                    down,
                    wparam,
                    position,
                    time + Duration::from_millis(elapsed)
                ),
                expected
            );
        }
    }

    #[test]
    // spec: WIN32-USER-9
    fn changing_profile_and_destroying_a_window_keep_no_host_state() {
        let mut clicks = DoubleClick::default();
        clicks.set_time(u32::MAX);
        assert_eq!(clicks.time_ms, 5000);
        clicks.set_time(0);
        assert_eq!(clicks.time_ms, 500);
        let time = Instant::now();
        clicks.message(10, 8, 0x201, 1, (i32::MAX, i32::MIN), time);
        clicks.message(10, 8, 0x202, 0, (i32::MAX, i32::MIN), time);
        clicks.forget_window(11);
        assert!(clicks.last.is_some());
        clicks.forget_window(10);
        assert!(clicks.last.is_none());
        clicks.width = 0;
        clicks.message(10, 8, 0x201, 1, (0, 0), time);
        clicks.message(10, 8, 0x202, 0, (0, 0), time);
        assert_eq!(clicks.message(10, 8, 0x201, 1, (0, 0), time), 0x201);
    }
}
