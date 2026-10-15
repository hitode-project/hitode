//! Semantic gestures for hosts that do not use the immediate mouse policy.
//! Positions are view pixels and timestamps are monotonic milliseconds.
use super::TouchAction;

pub const DRAG_PIXELS: f32 = 8.0;
pub const LONG_PRESS_MS: u64 = 500;
pub const SCROLL_PIXELS: f32 = 48.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    Tap(f32, f32),
    DragStart(f32, f32),
    DragMove(f32, f32),
    DragEnd(f32, f32),
    LongPress(f32, f32),
    RightClick(f32, f32),
    /// Fractional wheel notches, positive for an upward view-space movement.
    Scroll(f32),
}

impl Gesture {
    /// Convert semantic events to Win32 mouse messages in the presented canvas.
    pub fn mouse(
        self,
        client: (u32, u32),
        canvas: (u32, u32),
        scaling: crate::Scaling,
        cursor: (f32, f32),
    ) -> Vec<(u32, u32, u32)> {
        let (x, y) = match self {
            Self::Tap(x, y)
            | Self::DragStart(x, y)
            | Self::DragMove(x, y)
            | Self::DragEnd(x, y)
            | Self::LongPress(x, y)
            | Self::RightClick(x, y) => (x, y),
            Self::Scroll(_) => cursor,
        };
        let position = super::position(client, canvas, scaling, x, y);
        match self {
            Self::Tap(..) => vec![(0x201, 1, position), (0x202, 0, position)],
            Self::DragStart(..) => vec![(0x201, 1, position)],
            Self::DragMove(..) => vec![(0x200, 1, position)],
            Self::DragEnd(..) => vec![(0x202, 0, position)],
            Self::LongPress(..) | Self::RightClick(..) => {
                vec![(0x204, 2, position), (0x205, 0, position)]
            }
            Self::Scroll(notches) => vec![(
                0x20a,
                ((notches * 120.0).round() as i16 as u16 as u32) << 16,
                position,
            )],
        }
    }
}

#[derive(Default)]
pub struct Gestures {
    pointers: Vec<(i32, f32, f32)>,
    origin: (f32, f32),
    started: u64,
    dragging: bool,
    held: bool,
    chord: bool,
    scrolled: bool,
    scroll_pending: f32,
}

impl Gestures {
    /// Hosts call this from their long-press timer even when no pointer moves.
    pub fn advance(&mut self, now: u64) -> Option<Gesture> {
        if self.pointers.len() == 1
            && !self.chord
            && !self.dragging
            && !self.held
            && now.saturating_sub(self.started) >= LONG_PRESS_MS
        {
            self.held = true;
            return Some(Gesture::LongPress(self.origin.0, self.origin.1));
        }
        None
    }

    pub fn update(
        &mut self,
        id: i32,
        action: TouchAction,
        x: f32,
        y: f32,
        now: u64,
    ) -> Vec<Gesture> {
        let mut events = Vec::new();
        if !matches!(action, TouchAction::Cancel) && (!x.is_finite() || !y.is_finite()) {
            return events;
        }
        match action {
            TouchAction::Down => {
                if self.pointers.iter().any(|p| p.0 == id) {
                    return events;
                }
                if self.pointers.is_empty() {
                    *self = Self {
                        origin: (x, y),
                        started: now,
                        ..Self::default()
                    };
                } else {
                    if self.dragging {
                        let (_, px, py) = self.pointers[0];
                        events.push(Gesture::DragEnd(px, py));
                        self.dragging = false;
                    }
                    self.chord = true;
                }
                self.pointers.push((id, x, y));
            }
            TouchAction::Move => {
                let count = self.pointers.len();
                let Some(p) = self.pointers.iter_mut().find(|p| p.0 == id) else {
                    return events;
                };
                let dy = p.2 - y;
                *p = (id, x, y);
                if self.chord {
                    if count == 2 && dy != 0.0 {
                        self.scroll_pending += dy / 2.0;
                        if self.scrolled || self.scroll_pending.abs() >= DRAG_PIXELS {
                            self.scrolled = true;
                            events.push(Gesture::Scroll(self.scroll_pending / SCROLL_PIXELS));
                            self.scroll_pending = 0.0;
                        }
                    }
                } else if (x - self.origin.0).hypot(y - self.origin.1) >= DRAG_PIXELS {
                    if !self.dragging {
                        self.dragging = true;
                        events.push(Gesture::DragStart(self.origin.0, self.origin.1));
                    }
                    events.push(Gesture::DragMove(x, y));
                } else if self.dragging {
                    events.push(Gesture::DragMove(x, y));
                }
                events.extend(self.advance(now));
            }
            TouchAction::Up => {
                if !self.pointers.iter().any(|p| p.0 == id) {
                    return events;
                }
                events.extend(self.advance(now));
                self.pointers.retain(|p| p.0 != id);
                let dragged = self.dragging;
                if dragged {
                    events.push(Gesture::DragEnd(x, y));
                    self.dragging = false;
                }
                if self.pointers.is_empty() {
                    if self.chord && !self.scrolled {
                        events.push(Gesture::RightClick(self.origin.0, self.origin.1));
                    } else if !dragged
                        && !self.chord
                        && !self.held
                        && (x - self.origin.0).hypot(y - self.origin.1) < DRAG_PIXELS
                    {
                        events.push(Gesture::Tap(x, y));
                    }
                    *self = Self::default();
                }
            }
            TouchAction::Cancel => {
                if self.dragging {
                    let (_, px, py) = self.pointers[0];
                    events.push(Gesture::DragEnd(px, py));
                }
                *self = Self::default();
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TouchAction::*;
    // spec: AND-APP-14
    #[test]
    fn tap_drag_and_long_press_thresholds() {
        let mut g = Gestures::default();
        assert!(g.update(1, Down, 0., 0., 100).is_empty());
        assert_eq!(g.advance(599), None);
        assert_eq!(g.advance(600), Some(Gesture::LongPress(0., 0.)));
        assert_eq!(g.advance(601), None);
        assert!(g.update(1, Up, 0., 0., 602).is_empty());
        g.update(1, Down, 0., 0., 700);
        assert!(g.update(1, Move, 7.99, 0., 701).is_empty());
        assert_eq!(
            g.update(1, Move, 8., 0., 702),
            vec![Gesture::DragStart(0., 0.), Gesture::DragMove(8., 0.)]
        );
        assert_eq!(g.advance(2000), None);
        assert_eq!(
            g.update(1, Cancel, 0., 0., 2001),
            vec![Gesture::DragEnd(8., 0.)]
        );
        g.update(1, Down, 2., 3., 3000);
        assert_eq!(g.update(1, Up, 2., 3., 3499), vec![Gesture::Tap(2., 3.)]);
        g.update(1, Down, 2., 3., 4000);
        assert_eq!(
            g.update(1, Up, 2., 3., 4500),
            vec![Gesture::LongPress(2., 3.)]
        );
    }
    // spec: AND-APP-14
    #[test]
    fn two_finger_click_and_scroll_are_exclusive() {
        let mut g = Gestures::default();
        g.update(1, Down, 0., 0., 0);
        g.update(2, Down, 0., 0., 1);
        assert_eq!(g.advance(1000), None);
        assert!(g.update(1, Up, 0., 0., 1001).is_empty());
        assert_eq!(
            g.update(2, Up, 0., 0., 1002),
            vec![Gesture::RightClick(0., 0.)]
        );
        g.update(1, Down, 0., 0., 2000);
        g.update(2, Down, 0., 0., 2001);
        assert_eq!(
            g.update(1, Move, 0., -48., 2002),
            vec![Gesture::Scroll(0.5)]
        );
        assert_eq!(
            g.update(2, Move, 0., -48., 2003),
            vec![Gesture::Scroll(0.5)]
        );
        assert!(g.update(1, Up, 0., -48., 2004).is_empty());
        assert!(g.update(2, Up, 0., -48., 2005).is_empty());
    }
    // spec: AND-APP-14
    #[test]
    fn cancellation_and_unknown_pointers_do_not_click() {
        let mut g = Gestures::default();
        assert!(g.update(99, Up, 0., 0., 0).is_empty());
        g.update(1, Down, 0., 0., 0);
        assert!(g.update(1, Down, 0., 0., 0).is_empty());
        assert!(g.update(1, Cancel, 0., 0., 0).is_empty());
        assert_eq!(g.advance(1000), None);
    }
    // spec: AND-APP-14, AND-APP-11
    #[test]
    fn gesture_mouse_mapping_and_scroll_threshold() {
        assert_eq!(
            Gesture::Tap(960., 540.).mouse((1920, 1080), (640, 480), crate::Scaling::Fit, (0., 0.)),
            vec![(0x201, 1, 320 | 240 << 16), (0x202, 0, 320 | 240 << 16)]
        );
        assert_eq!(
            Gesture::Scroll(-1.).mouse((1, 1), (1, 1), crate::Scaling::Fit, (0., 0.)),
            vec![(0x20a, 0xff880000, 0)]
        );
        let mut g = Gestures::default();
        g.update(1, Down, 0., 0., 0);
        g.update(2, Down, 0., 0., 0);
        assert!(g.update(1, Move, 0., -15.99, 1).is_empty());
        assert_eq!(
            g.update(1, Move, 0., -16., 2),
            vec![Gesture::Scroll(8. / SCROLL_PIXELS)]
        );
        assert!(g.update(1, Cancel, 0., 0., 3).is_empty());
        g.update(1, Down, 0., 0., 4);
        g.update(1, Move, 8., 0., 5);
        g.update(1, Move, 0., 0., 6);
        assert_eq!(g.update(1, Up, 0., 0., 7), vec![Gesture::DragEnd(0., 0.)]);
    }
}
