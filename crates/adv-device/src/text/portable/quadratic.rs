//! Uniform midpoint quadratics for cubic outlines requested as GGO_NATIVE.
use super::{Curve, Point, Vector};

const TOLERANCE: f64 = 1.0 / 16.0;
const MAX_PIECES: usize = 16;
type Position = (f64, f64);

fn at(c: &[Position; 4], t: f64) -> Position {
    let u = 1.0 - t;
    let (a, b, d, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * c[0].0 + b * c[1].0 + d * c[2].0 + e * c[3].0,
        a * c[0].1 + b * c[1].1 + d * c[2].1 + e * c[3].1,
    )
}
fn derivative(c: &[Position; 4], t: f64) -> Position {
    let u = 1.0 - t;
    let (a, b, d) = (3.0 * u * u, 6.0 * u * t, 3.0 * t * t);
    (
        a * (c[1].0 - c[0].0) + b * (c[2].0 - c[1].0) + d * (c[3].0 - c[2].0),
        a * (c[1].1 - c[0].1) + b * (c[2].1 - c[1].1) + d * (c[3].1 - c[2].1),
    )
}
fn pieces(c: &[Position; 4]) -> usize {
    let third = (
        c[3].0 - 3.0 * c[2].0 + 3.0 * c[1].0 - c[0].0,
        c[3].1 - 3.0 * c[2].1 + 3.0 * c[1].1 - c[0].1,
    );
    let error = third.0.hypot(third.1) * 3f64.sqrt() / 36.0;
    ((error / TOLERANCE).cbrt().ceil() as usize).clamp(1, MAX_PIECES)
}
fn control(c: &[Position; 4], t0: f64, t1: f64) -> Position {
    let (start, end) = (at(c, t0), at(c, t1));
    let (d0, d1) = (derivative(c, t0), derivative(c, t1));
    let h = (t1 - t0) / 3.0;
    let p1 = (start.0 + h * d0.0, start.1 + h * d0.1);
    let p2 = (end.0 - h * d1.0, end.1 - h * d1.1);
    (
        (3.0 * (p1.0 + p2.0) - start.0 - end.0) / 4.0,
        (3.0 * (p1.1 + p2.1) - start.1 - end.1) / 4.0,
    )
}
fn point(p: Position) -> Point {
    Point {
        x: (p.0 * 65536.0).round() as i32,
        y: (p.1 * 65536.0).round() as i32,
    }
}
pub(super) fn append(cubic: [Vector; 4], curves: &mut Vec<Curve>) {
    let c = cubic.map(|p| (f64::from(p.x), f64::from(p.y)));
    let count = pieces(&c);
    for piece in 0..count {
        let t0 = piece as f64 / count as f64;
        let t1 = (piece + 1) as f64 / count as f64;
        curves.push(Curve::Quadratic(vec![
            point(control(&c, t0, t1)),
            point(at(&c, t1)),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: DEV-FONT-1
    #[test]
    fn text_cubic_midpoint_quadratics_preserve_endpoints_and_error_bound() {
        for c in [
            [(32.0, 0.0), (32.0, 25.0), (0.0, 25.0), (0.0, 0.0)],
            [(0.0, 0.0), (20.0, 150.0), (-70.0, -100.0), (10.0, 10.0)],
            [(0.0, 0.0), (1.0, 2.0), (2.0, 2.0), (3.0, 0.0)],
        ] {
            let mut curves = Vec::new();
            append(c.map(|(x, y)| Vector::new(x as f32, y as f32)), &mut curves);
            assert!(!curves.is_empty() && curves.len() <= MAX_PIECES);
            let mut start = c[0];
            for (piece, curve) in curves.iter().enumerate() {
                let Curve::Quadratic(p) = curve else {
                    panic!("quadratic required")
                };
                let q = (f64::from(p[0].x) / 65536.0, f64::from(p[0].y) / 65536.0);
                let end = (f64::from(p[1].x) / 65536.0, f64::from(p[1].y) / 65536.0);
                for sample in 0..=100 {
                    let t = sample as f64 / 100.0;
                    let u = 1.0 - t;
                    let approximate = (
                        u * u * start.0 + 2.0 * u * t * q.0 + t * t * end.0,
                        u * u * start.1 + 2.0 * u * t * q.1 + t * t * end.1,
                    );
                    let exact = at(&c, (piece as f64 + t) / curves.len() as f64);
                    assert!(
                        (approximate.0 - exact.0).hypot(approximate.1 - exact.1)
                            <= TOLERANCE + 1.0 / 65536.0
                    );
                }
                start = end;
            }
            assert_eq!(start, c[3]);
        }
    }

    // spec: DEV-FONT-1
    #[test]
    fn text_quadratic_split_count_is_bounded() {
        assert_eq!(pieces(&[(0.0, 0.0); 4]), 1);
        assert_eq!(
            pieces(&[(0.0, 0.0), (1e9, 0.0), (0.0, 1e9), (0.0, 0.0)]),
            MAX_PIECES
        );
    }
}
