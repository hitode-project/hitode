//! Touch coordinates in the presented frame, shared by mobile hosts.
use crate::Scaling;

#[derive(Clone, Copy, Debug)]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
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
/// Client pixels per canvas pixel along each axis of the presented frame.
pub fn magnification(client: (u32, u32), canvas: (u32, u32), scaling: Scaling) -> (f32, f32) {
    let [_, _, width, height] = scaling.rectangle((client.0 as f64, client.1 as f64), canvas);
    (
        (width / f64::from(canvas.0.max(1))) as f32,
        (height / f64::from(canvas.1.max(1))) as f32,
    )
}

/// The client point at the centre of canvas pixel `(x, y)`.
pub fn client_point(
    client: (u32, u32),
    canvas: (u32, u32),
    scaling: Scaling,
    x: f32,
    y: f32,
) -> (f32, f32) {
    let [left, top, ..] = scaling.rectangle((client.0 as f64, client.1 as f64), canvas);
    let (sx, sy) = magnification(client, canvas, scaling);
    (left as f32 + (x + 0.5) * sx, top as f32 + (y + 0.5) * sy)
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
    #[test]
    fn client_points_invert_positions() {
        let (x, y) = client_point((1920, 1080), (640, 480), Scaling::Fit, 320., 240.);
        assert_eq!(
            position((1920, 1080), (640, 480), Scaling::Fit, x, y),
            320 | 240 << 16
        );
        assert_eq!(
            magnification((1920, 1080), (640, 480), Scaling::Fit),
            (2.25, 2.25)
        );
    }
}
