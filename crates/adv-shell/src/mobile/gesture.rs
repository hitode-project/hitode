//! Touch gestures shared by mobile hosts (AND-APP-14). Positions are view
//! pixels, distances are given in points and scaled by `set_scale`, and
//! timestamps are monotonic milliseconds.
//!
//! Direct mode: a tap clicks left at its point, a one-finger vertical swipe
//! turns the wheel, and a finger held still for the long-press time presses
//! the left button there and drags until it lifts. Trackpad mode moves a
//! cursor with one finger; a tap clicks at the cursor and a held finger drags
//! from it. In both, two fingers moving turn the wheel, a two-finger tap
//! clicks right and a three-finger tap asks for the keyboard.
//!
//! The timing follows Moonlight's touch handlers (moonlight-stream,
//! GPL-3.0-or-later; AbsoluteTouchContext and RelativeTouchContext): the
//! long press fires from a timer rather than the next move, clicks hold their
//! button for 100 ms so games that poll the button state see them, and a tap
//! landing near the previous one reuses its point, which steadies double
//! clicks.
use super::TouchAction;

/// Movement within this distance leaves a touch a tap or a long press.
pub const SLOP_POINTS: f32 = 10.0;
/// Finger travel per wheel notch.
pub const NOTCH_POINTS: f32 = 16.0;
pub const LONG_PRESS_MS: u64 = 650;
/// How long a click holds its button.
pub const CLICK_MS: u64 = 100;
/// A tap this soon after the last one, this close to it, clicks its point.
const DOUBLE_TAP_MS: u64 = 250;
const DOUBLE_TAP_POINTS: f32 = 20.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TouchMode {
    #[default]
    Direct,
    Trackpad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Move the mouse to this point (direct mode).
    Point(f32, f32),
    /// Move the trackpad cursor by this distance.
    Move(f32, f32),
    /// Press or release a button at the mouse position.
    Press(Button),
    Release(Button),
    /// Whole wheel notches, as games count WHEEL_DELTA units; positive when
    /// the fingers move down, which scrolls toward earlier content as on a
    /// touch screen.
    Scroll(i32),
    Keyboard,
}

/// What the fingers on the view are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    /// One finger, still within the slop: a tap or a long press.
    #[default]
    Pending,
    Scrolling,
    /// The long press pressed the left button.
    Dragging,
    /// Trackpad cursor movement.
    Moving,
    /// Two or more fingers; `moved` rules out a tap.
    Chord {
        fingers: usize,
        moved: bool,
    },
    /// Fingers that no longer form a gesture, until all lift.
    Spent,
}

#[derive(Default)]
pub struct Gestures {
    mode: TouchMode,
    /// View pixels per point; zero means one.
    scale: f32,
    /// Live pointers with their landing and latest positions.
    pointers: Vec<(i32, (f32, f32), (f32, f32))>,
    phase: Phase,
    /// When the long press fires.
    long_press: Option<u64>,
    /// When a click's button comes back up.
    release: Option<(u64, Button)>,
    /// Wheel travel short of a whole notch, in notches.
    wheel: f32,
    /// Where and when the last tap clicked.
    last_tap: Option<(u64, (f32, f32))>,
}

impl Gestures {
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    /// Changes the mode; a gesture in progress ends.
    pub fn set_mode(&mut self, mode: TouchMode) -> Vec<Gesture> {
        let events = self.cancel();
        self.mode = mode;
        events
    }

    /// When `tick` next has work, if ever.
    pub fn deadline(&self) -> Option<u64> {
        match (self.long_press, self.release.map(|r| r.0)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Fires the long press and click releases due by `now`.
    pub fn tick(&mut self, now: u64) -> Vec<Gesture> {
        let mut events = Vec::new();
        if let Some((at, button)) = self.release
            && at <= now
        {
            self.release = None;
            events.push(Gesture::Release(button));
        }
        if self.long_press.is_some_and(|at| at <= now) {
            self.long_press = None;
            if self.phase == Phase::Pending
                && let Some(&(_, origin, _)) = self.pointers.first()
            {
                self.phase = Phase::Dragging;
                self.flush_release(&mut events);
                if self.mode == TouchMode::Direct {
                    events.push(Gesture::Point(origin.0, origin.1));
                }
                events.push(Gesture::Press(Button::Left));
            }
        }
        events
    }

    pub fn update(
        &mut self,
        id: i32,
        action: TouchAction,
        x: f32,
        y: f32,
        now: u64,
    ) -> Vec<Gesture> {
        if !matches!(action, TouchAction::Cancel) && (!x.is_finite() || !y.is_finite()) {
            return Vec::new();
        }
        let mut events = self.tick(now);
        events.extend(match action {
            TouchAction::Down => self.down(id, x, y, now),
            TouchAction::Move => self.moved(id, x, y),
            TouchAction::Up => self.up(id, x, y, now),
            TouchAction::Cancel => self.cancel(),
        });
        events
    }

    fn points(&self, points: f32) -> f32 {
        points * if self.scale > 0.0 { self.scale } else { 1.0 }
    }

    fn down(&mut self, id: i32, x: f32, y: f32, now: u64) -> Vec<Gesture> {
        if self.pointers.iter().any(|p| p.0 == id) {
            return Vec::new();
        }
        if self.pointers.is_empty() {
            self.phase = Phase::Pending;
            self.wheel = 0.0;
            self.long_press = Some(now + LONG_PRESS_MS);
        } else {
            self.long_press = None;
            self.phase = match self.phase {
                Phase::Pending => Phase::Chord {
                    fingers: 2,
                    moved: false,
                },
                Phase::Chord { fingers, moved } => Phase::Chord {
                    fingers: fingers + 1,
                    moved,
                },
                // A drag, scroll or cursor movement keeps its first finger.
                phase => phase,
            };
        }
        self.pointers.push((id, (x, y), (x, y)));
        Vec::new()
    }

    fn moved(&mut self, id: i32, x: f32, y: f32) -> Vec<Gesture> {
        let mut events = Vec::new();
        let slop = self.points(SLOP_POINTS);
        let primary = self.pointers.first().map(|p| p.0) == Some(id);
        let Some(p) = self.pointers.iter_mut().find(|p| p.0 == id) else {
            return events;
        };
        let (origin, last) = (p.1, p.2);
        // UIKit reports resting fingers too.
        if last == (x, y) {
            return events;
        }
        p.2 = (x, y);
        let travelled = (x - origin.0).hypot(y - origin.1) > slop;
        match self.phase {
            Phase::Pending if self.mode == TouchMode::Trackpad => {
                // The cursor follows from the first pixel; only a tap needs
                // the finger to stay within the slop.
                events.push(Gesture::Move(x - last.0, y - last.1));
                if travelled {
                    self.phase = Phase::Moving;
                    self.long_press = None;
                }
            }
            Phase::Pending if travelled => {
                self.phase = Phase::Scrolling;
                self.long_press = None;
                self.scroll(y - origin.1, &mut events);
            }
            Phase::Scrolling if primary => self.scroll(y - last.1, &mut events),
            Phase::Dragging if primary => events.push(match self.mode {
                TouchMode::Direct => Gesture::Point(x, y),
                TouchMode::Trackpad => Gesture::Move(x - last.0, y - last.1),
            }),
            Phase::Moving if primary => events.push(Gesture::Move(x - last.0, y - last.1)),
            Phase::Chord { fingers, moved } => {
                let moved = moved || travelled;
                // Two fingers turn the wheel by their mean vertical movement.
                if fingers == 2 && moved {
                    self.scroll((y - last.1) / 2.0, &mut events);
                }
                self.phase = Phase::Chord { fingers, moved };
            }
            _ => {}
        }
        events
    }

    /// Turns the wheel by whole notches for `pixels` of finger travel,
    /// keeping the remainder.
    fn scroll(&mut self, pixels: f32, events: &mut Vec<Gesture>) {
        self.wheel += pixels / self.points(NOTCH_POINTS);
        let whole = self.wheel.trunc();
        if whole != 0.0 {
            self.wheel -= whole;
            events.push(Gesture::Scroll(whole as i32));
        }
    }

    fn up(&mut self, id: i32, x: f32, y: f32, now: u64) -> Vec<Gesture> {
        let Some(index) = self.pointers.iter().position(|p| p.0 == id) else {
            return Vec::new();
        };
        let (_, origin, _) = self.pointers.remove(index);
        let mut events = Vec::new();
        match self.phase {
            Phase::Dragging if index == 0 => {
                if self.mode == TouchMode::Direct {
                    events.push(Gesture::Point(x, y));
                }
                events.push(Gesture::Release(Button::Left));
                self.phase = Phase::Spent;
            }
            Phase::Scrolling | Phase::Moving if index == 0 => self.phase = Phase::Spent,
            _ => {}
        }
        if !self.pointers.is_empty() {
            return events;
        }
        self.long_press = None;
        match self.phase {
            Phase::Pending => {
                let point = match self.last_tap {
                    Some((at, last))
                        if now.saturating_sub(at) <= DOUBLE_TAP_MS
                            && (origin.0 - last.0).hypot(origin.1 - last.1)
                                <= self.points(DOUBLE_TAP_POINTS) =>
                    {
                        last
                    }
                    _ => origin,
                };
                self.last_tap = Some((now, point));
                self.click(Some(point), Button::Left, now, &mut events);
            }
            Phase::Chord {
                fingers: 2,
                moved: false,
            } => self.click(Some(origin), Button::Right, now, &mut events),
            Phase::Chord {
                fingers: 3,
                moved: false,
            } => events.push(Gesture::Keyboard),
            _ => {}
        }
        self.phase = Phase::Pending;
        events
    }

    /// Presses a button, at `point` in direct mode, and schedules its release.
    fn click(
        &mut self,
        point: Option<(f32, f32)>,
        button: Button,
        now: u64,
        events: &mut Vec<Gesture>,
    ) {
        self.flush_release(events);
        if let (TouchMode::Direct, Some((x, y))) = (self.mode, point) {
            events.push(Gesture::Point(x, y));
        }
        events.push(Gesture::Press(button));
        self.release = Some((now + CLICK_MS, button));
    }

    /// Releases a click's button early, before anything else presses.
    fn flush_release(&mut self, events: &mut Vec<Gesture>) {
        if let Some((_, button)) = self.release.take() {
            events.push(Gesture::Release(button));
        }
    }

    fn cancel(&mut self) -> Vec<Gesture> {
        let mut events = Vec::new();
        self.flush_release(&mut events);
        if self.phase == Phase::Dragging {
            events.push(Gesture::Release(Button::Left));
        }
        *self = Self {
            mode: self.mode,
            scale: self.scale,
            last_tap: self.last_tap,
            ..Self::default()
        };
        events
    }
}

/// The Win32 mouse message for a gesture at `at`, a packed client position,
/// with `buttons` the MK_ flags held before it. Returns the flags held after.
pub fn mouse(gesture: Gesture, at: u32, buttons: u32) -> (u32, Option<(u32, u32, u32)>) {
    const MOVE: u32 = 0x200;
    let flag = |button| match button {
        Button::Left => 1,
        Button::Right => 2,
    };
    match gesture {
        Gesture::Point(..) | Gesture::Move(..) => (buttons, Some((MOVE, buttons, at))),
        Gesture::Press(button) => {
            let held = buttons | flag(button);
            let message = if button == Button::Left { 0x201 } else { 0x204 };
            (held, Some((message, held, at)))
        }
        Gesture::Release(button) => {
            let held = buttons & !flag(button);
            let message = if button == Button::Left { 0x202 } else { 0x205 };
            (held, Some((message, held, at)))
        }
        Gesture::Scroll(notches) => {
            let delta = notches.saturating_mul(120).clamp(-32768, 32767) as i16 as u16 as u32;
            (buttons, Some((0x20a, delta << 16 | buttons, at)))
        }
        Gesture::Keyboard => (buttons, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Button::*;
    use Gesture::{Keyboard, Point, Press, Release, Scroll};
    use TouchAction::*;

    fn direct() -> Gestures {
        Gestures::default()
    }

    fn trackpad() -> Gestures {
        let mut g = Gestures::default();
        g.set_mode(TouchMode::Trackpad);
        g
    }

    // spec: AND-APP-14
    #[test]
    fn a_tap_clicks_and_holds_its_button_briefly() {
        let mut g = direct();
        g.update(1, Down, 2., 3., 0);
        assert!(g.update(1, Move, 2., 12.9, 10).is_empty());
        assert_eq!(
            g.update(1, Up, 2., 12., 300),
            vec![Point(2., 3.), Press(Left)]
        );
        assert_eq!(g.deadline(), Some(400));
        assert!(g.tick(399).is_empty());
        assert_eq!(g.tick(400), vec![Release(Left)]);
        assert_eq!(g.deadline(), None);
    }

    // spec: AND-APP-14
    #[test]
    fn a_second_tap_nearby_clicks_the_same_point() {
        let mut g = direct();
        g.set_scale(3.);
        g.update(1, Down, 100., 100., 0);
        g.update(1, Up, 100., 100., 50);
        g.update(1, Down, 150., 130., 120);
        // The second press releases the first click before pressing again.
        assert_eq!(
            g.update(1, Up, 150., 130., 160),
            vec![Release(Left), Point(100., 100.), Press(Left)]
        );
        // Later, or farther, a tap clicks its own point.
        assert_eq!(g.update(1, Down, 300., 300., 1000), vec![Release(Left)]);
        assert_eq!(
            g.update(1, Up, 300., 300., 1050),
            vec![Point(300., 300.), Press(Left)]
        );
    }

    // spec: AND-APP-14
    #[test]
    fn a_swipe_turns_the_wheel_in_whole_notches() {
        let mut g = direct();
        g.set_scale(2.);
        g.update(1, Down, 0., 0., 0);
        // 20 pixels are within the 10-point slop at two pixels a point.
        assert!(g.update(1, Move, 0., 20., 10).is_empty());
        assert_eq!(g.update(1, Move, 0., 40., 20), vec![Scroll(1)]);
        assert!(g.update(1, Move, 0., 60., 30).is_empty());
        assert_eq!(g.update(1, Move, 0., 140., 40), vec![Scroll(3)]);
        assert_eq!(g.update(1, Move, 0., 0., 50), vec![Scroll(-4)]);
        // The long press no longer fires, and the swipe makes no tap.
        assert!(g.tick(1000).is_empty());
        assert!(g.update(1, Up, 0., 0., 1100).is_empty());
    }

    // spec: AND-APP-14
    #[test]
    fn a_held_finger_drags_without_moving_first() {
        let mut g = direct();
        g.update(1, Down, 5., 5., 0);
        assert!(g.update(1, Move, 12., 5., 300).is_empty());
        assert_eq!(g.deadline(), Some(LONG_PRESS_MS));
        assert_eq!(g.tick(LONG_PRESS_MS), vec![Point(5., 5.), Press(Left)]);
        assert_eq!(g.update(1, Move, 40., 5., 700), vec![Point(40., 5.)]);
        // A second finger leaves the drag alone.
        assert!(g.update(2, Down, 50., 50., 710).is_empty());
        assert!(g.update(2, Up, 50., 50., 720).is_empty());
        assert_eq!(
            g.update(1, Up, 41., 6., 800),
            vec![Point(41., 6.), Release(Left)]
        );
        g.update(1, Down, 0., 0., 1000);
        g.tick(1000 + LONG_PRESS_MS);
        assert_eq!(g.update(1, Cancel, 0., 0., 1700), vec![Release(Left)]);
    }

    // spec: AND-APP-14
    #[test]
    fn multi_finger_taps_click_right_or_ask_for_the_keyboard() {
        for mut g in [direct(), trackpad()] {
            g.update(1, Down, 4., 4., 0);
            g.update(2, Down, 40., 4., 30);
            assert!(g.update(1, Up, 4., 4., 100).is_empty());
            let right = g.update(2, Up, 40., 4., 110);
            assert_eq!(right.last(), Some(&Press(Right)));
            assert_eq!(g.tick(210), vec![Release(Right)]);
            g.update(1, Down, 0., 0., 300);
            g.update(2, Down, 10., 0., 310);
            g.update(3, Down, 20., 0., 320);
            g.update(3, Up, 20., 0., 400);
            g.update(1, Up, 0., 0., 401);
            assert_eq!(g.update(2, Up, 10., 0., 402), vec![Keyboard]);
            // Two fingers that travel turn the wheel and make no tap.
            g.update(1, Down, 0., 0., 500);
            g.update(2, Down, 10., 0., 510);
            assert_eq!(g.update(2, Move, 10., 40., 520), vec![Scroll(1)]);
            g.update(1, Up, 0., 0., 530);
            assert!(g.update(2, Up, 10., 40., 540).is_empty());
            assert!(g.tick(5000).is_empty());
        }
    }

    // spec: AND-APP-14
    #[test]
    fn trackpad_fingers_move_the_cursor_tap_and_drag() {
        let mut g = trackpad();
        g.update(1, Down, 0., 0., 0);
        assert_eq!(g.update(1, Move, 3., 0., 1), vec![Gesture::Move(3., 0.)]);
        assert_eq!(g.update(1, Up, 3., 0., 100), vec![Press(Left)]);
        assert_eq!(g.tick(200), vec![Release(Left)]);
        g.update(1, Down, 0., 0., 1000);
        assert_eq!(
            g.update(1, Move, 20., 0., 1010),
            vec![Gesture::Move(20., 0.)]
        );
        assert!(g.tick(2000).is_empty());
        assert!(g.update(1, Up, 20., 0., 2010).is_empty());
        g.update(1, Down, 0., 0., 3000);
        assert_eq!(g.tick(3000 + LONG_PRESS_MS), vec![Press(Left)]);
        assert_eq!(
            g.update(1, Move, 0., 30., 3700),
            vec![Gesture::Move(0., 30.)]
        );
        assert_eq!(g.update(1, Up, 0., 30., 3800), vec![Release(Left)]);
    }

    // spec: AND-APP-14
    #[test]
    fn mouse_messages_track_the_buttons() {
        assert_eq!(mouse(Point(0., 0.), 7, 0), (0, Some((0x200, 0, 7))));
        assert_eq!(mouse(Press(Left), 7, 2), (3, Some((0x201, 3, 7))));
        assert_eq!(mouse(Release(Right), 7, 3), (1, Some((0x205, 1, 7))));
        assert_eq!(mouse(Scroll(-1), 7, 0), (0, Some((0x20a, 0xff88_0000, 7))));
        assert_eq!(mouse(Keyboard, 7, 0), (0, None));
        let mut g = direct();
        g.update(1, Down, 0., 0., 0);
        assert!(g.update(99, Up, 0., 0., 1).is_empty());
        assert!(g.update(1, Cancel, 0., 0., 2).is_empty());
        assert!(g.tick(5000).is_empty());
    }
}
