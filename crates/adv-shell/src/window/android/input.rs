use crate::Scaling;

#[derive(Clone, Copy)]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}
#[derive(Default)]
pub(crate) struct Touch {
    pointers: Vec<i32>,
    left: Option<i32>,
    position: u32,
}
pub(crate) fn position(
    client: (u32, u32),
    canvas: (u32, u32),
    scaling: Scaling,
    x: f32,
    y: f32,
) -> u32 {
    let [left, top, width, height] = scaling.rectangle((client.0 as f64, client.1 as f64), canvas);
    let x = ((f64::from(x) - left) * f64::from(canvas.0) / width.max(1.0))
        .floor()
        .clamp(0.0, f64::from(canvas.0.saturating_sub(1))) as u16;
    let y = ((f64::from(y) - top) * f64::from(canvas.1) / height.max(1.0))
        .floor()
        .clamp(0.0, f64::from(canvas.1.saturating_sub(1))) as u16;
    u32::from(x) | u32::from(y) << 16
}
impl Touch {
    pub(crate) fn update(
        &mut self,
        pointer: i32,
        action: TouchAction,
        position: u32,
    ) -> Vec<(u32, u32, u32)> {
        let mut events = Vec::new();
        let mut release = |this: &mut Self| {
            if this.left.take().is_some() {
                events.push((0x202, 0, this.position));
            }
        };
        match action {
            TouchAction::Down => {
                self.pointers.push(pointer);
                if self.pointers.len() == 1 {
                    self.position = position;
                    self.left = Some(pointer);
                    events.push((0x201, 1, position));
                } else if self.pointers.len() == 2 {
                    release(self);
                    events.extend([(0x204, 2, self.position), (0x205, 0, self.position)]);
                }
            }
            TouchAction::Move if self.left == Some(pointer) => {
                self.position = position;
                events.push((0x200, 1, position));
            }
            TouchAction::Up => {
                if self.left == Some(pointer) {
                    self.position = position;
                    release(self);
                }
                self.pointers.retain(|id| *id != pointer);
            }
            TouchAction::Cancel => {
                release(self);
                self.pointers.clear();
            }
            _ => {}
        }
        events
    }
}
/// Android physical key codes to Win32 virtual keys and set-1 scan codes.
pub(crate) fn key(code: i32) -> Option<(u32, u32)> {
    const LETTERS: [u32; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    Some(match code {
        29..=54 => (0x41 + (code - 29) as u32, LETTERS[(code - 29) as usize]),
        7 => (0x30, 0x0b),
        8..=16 => (0x31 + (code - 8) as u32, (code - 6) as u32),
        131..=142 => (
            0x70 + (code - 131) as u32,
            if code <= 140 {
                (code - 72) as u32
            } else {
                (code - 54) as u32
            },
        ),
        19 => (0x26, 0x148),
        20 => (0x28, 0x150),
        21 => (0x25, 0x14b),
        22 => (0x27, 0x14d),
        57 => (0x12, 0x38),
        58 => (0x12, 0x138),
        59 => (0x10, 0x2a),
        60 => (0x10, 0x36),
        61 => (9, 0x0f),
        62 => (0x20, 0x39),
        66 => (0x0d, 0x1c),
        67 => (8, 0x0e),
        111 => (0x1b, 1),
        112 => (0x2e, 0x153),
        113 => (0x11, 0x1d),
        114 => (0x11, 0x11d),
        115 => (0x14, 0x3a),
        116 => (0x91, 0x46),
        120 => (0x2c, 0x137),
        121 => (0x13, 0x45),
        92 => (0x21, 0x149),
        93 => (0x22, 0x151),
        122 => (0x24, 0x147),
        123 => (0x23, 0x14f),
        124 => (0x2d, 0x152),
        55 => (0xbc, 0x33),
        56 => (0xbe, 0x34),
        68 => (0xc0, 0x29),
        69 => (0xbd, 0x0c),
        70 => (0xbb, 0x0d),
        71 => (0xdb, 0x1a),
        72 => (0xdd, 0x1b),
        73 => (0xdc, 0x2b),
        74 => (0xba, 0x27),
        75 => (0xde, 0x28),
        76 => (0xbf, 0x35),
        _ => return None,
    })
}
pub(crate) fn message(key: u32, scan: u32, down: bool, repeat: bool, alt: bool) -> (u32, u32, u32) {
    (
        if down { 0x100 } else { 0x101 } + if alt || key == 0x79 { 4 } else { 0 },
        key,
        1 | scan << 16
            | u32::from(alt) << 29
            | u32::from(repeat || !down) << 30
            | u32::from(!down) << 31,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    // spec: HOST-AND-11
    #[test]
    fn touch_uses_the_presented_rectangle_and_clamps_letterbox() {
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Fit, 240., 0.),
            0
        );
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Fit, 1680., 1080.),
            639 | 479 << 16
        );
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Integer, 320., 60.),
            0
        );
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Integer, 960., 540.),
            320 | 240 << 16
        );
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Stretch, 960., 540.),
            320 | 240 << 16
        );
    }
    // spec: HOST-AND-11
    #[test]
    fn second_finger_cancels_drag_and_clicks_right_once() {
        let mut touch = Touch::default();
        assert_eq!(touch.update(4, TouchAction::Down, 10), vec![(0x201, 1, 10)]);
        assert_eq!(touch.update(4, TouchAction::Move, 20), vec![(0x200, 1, 20)]);
        assert_eq!(
            touch.update(9, TouchAction::Down, 30),
            vec![(0x202, 0, 20), (0x204, 2, 20), (0x205, 0, 20)]
        );
        assert!(touch.update(12, TouchAction::Down, 40).is_empty());
        assert!(touch.update(4, TouchAction::Up, 40).is_empty());
        assert!(touch.update(9, TouchAction::Move, 40).is_empty());
        assert!(touch.update(9, TouchAction::Cancel, 40).is_empty());
        assert_eq!(touch.update(9, TouchAction::Down, 40), vec![(0x201, 1, 40)]);
        assert_eq!(
            touch.update(9, TouchAction::Cancel, 0),
            vec![(0x202, 0, 40)]
        );
    }
    // spec: HOST-AND-11
    #[test]
    fn physical_keys_preserve_scan_codes_and_leave_back_to_the_host() {
        assert_eq!(key(4), None);
        assert_eq!(key(29), Some((0x41, 0x1e)));
        assert_eq!(key(114), Some((0x11, 0x11d)));
        assert_eq!(
            message(0x11, 0x11d, false, false, false),
            (0x101, 0x11, 0xc11d0001)
        );
        assert_eq!(
            message(0x41, 0x1e, true, true, true),
            (0x104, 0x41, 0x601e0001)
        );
    }
}
