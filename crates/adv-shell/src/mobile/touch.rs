//! Immediate touch-to-mouse policy, shared by mobile hosts.
use crate::Scaling;

#[derive(Clone, Copy)]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}
#[derive(Default)]
pub struct Touch {
    pointers: Vec<i32>,
    left: Option<i32>,
    position: u32,
}
pub fn position(client: (u32, u32), canvas: (u32, u32), scaling: Scaling, x: f32, y: f32) -> u32 {
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
    pub fn update(
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

#[cfg(test)]
mod tests {
    use super::*;
    // spec: AND-APP-11
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
    // spec: AND-APP-11
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
}
