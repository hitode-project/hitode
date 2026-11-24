//! Synthetic weight keeps the original contour verbs and expands their joins
//! horizontally: the glyph widens by `strength` to the right, its height and
//! left edge stay.
use super::*;

pub(super) fn embolden(commands: &mut [Command], strength: f32) {
    let mut contours = Vec::<Vec<Vector>>::new();
    for command in commands.iter() {
        match *command {
            Command::MoveTo(p) => contours.push(vec![p]),
            Command::LineTo(p) => contours.last_mut().unwrap().push(p),
            Command::QuadTo(a, p) => contours.last_mut().unwrap().extend([a, p]),
            Command::CurveTo(a, b, p) => contours.last_mut().unwrap().extend([a, b, p]),
            Command::Close => {}
        }
    }
    let area = |points: &[Vector]| {
        points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
            .map(|(a, b)| a.x * b.y - b.x * a.y)
            .sum::<f32>()
    };
    let direction = contours
        .iter()
        .map(|p| area(p))
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(1.0)
        .signum();
    for points in &mut contours {
        let old = points.clone();
        let n = old.len();
        for i in 0..n {
            let normal = |a: Vector, b: Vector| {
                let d = b - a;
                let length = (d.x * d.x + d.y * d.y).sqrt();
                if length == 0.0 {
                    Vector::ZERO
                } else {
                    Vector::new(d.y / length, -d.x / length) * direction
                }
            };
            let a = normal(old[(i + n - 1) % n], old[i]);
            let b = normal(old[i], old[(i + 1) % n]);
            let scale = strength * 0.5 / (1.0 + a.x * b.x + a.y * b.y).max(0.25);
            // A sharp corner's miter would reach past the stroke; clamping it
            // keeps the left edge in place.
            let offset = ((a + b) * scale).x.clamp(-strength * 0.5, strength * 0.5);
            points[i] = old[i] + Vector::new(offset + strength * 0.5, 0.0);
        }
    }
    let mut contour = 0;
    let mut at = 0;
    let mut next = || {
        let p = contours[contour][at];
        at += 1;
        if at == contours[contour].len() {
            contour += 1;
            at = 0;
        }
        p
    };
    for command in commands {
        *command = match *command {
            Command::MoveTo(_) => Command::MoveTo(next()),
            Command::LineTo(_) => Command::LineTo(next()),
            Command::QuadTo(_, _) => Command::QuadTo(next(), next()),
            Command::CurveTo(_, _, _) => Command::CurveTo(next(), next(), next()),
            Command::Close => Command::Close,
        };
    }
}

pub(super) fn transform(command: Command, matrix: Transform) -> Command {
    let point = |p: Vector| {
        let (x, y) = transform_point(matrix, p.x, p.y);
        Vector::new(x, y)
    };
    match command {
        Command::MoveTo(p) => Command::MoveTo(point(p)),
        Command::LineTo(p) => Command::LineTo(point(p)),
        Command::QuadTo(a, p) => Command::QuadTo(point(a), point(p)),
        Command::CurveTo(a, b, p) => Command::CurveTo(point(a), point(b), point(p)),
        Command::Close => Command::Close,
    }
}

/// Rotate an upright glyph +90° CCW into a `@`-font's horizontal baseline cell
/// using its vertical origin `tx` and typographic baseline shift `ty`.
pub(super) fn rotate_vertical(command: Command, tx: f32, ty: f32) -> Command {
    let point = |p: Vector| Vector::new(-p.y + tx, p.x + ty);
    match command {
        Command::MoveTo(p) => Command::MoveTo(point(p)),
        Command::LineTo(p) => Command::LineTo(point(p)),
        Command::QuadTo(a, p) => Command::QuadTo(point(a), point(p)),
        Command::CurveTo(a, b, p) => Command::CurveTo(point(a), point(b), point(p)),
        Command::Close => Command::Close,
    }
}

/// Fit an upright glyph outline horizontally into `[0, cell_width]` for a 1:2
/// CJK substitute: condense wider outlines proportionally and center narrower
/// ones without stretching, keeping `floor(min_x) >= 0` and `ceil(max_x) <= cell_width`.
pub(super) fn fit_horizontal(commands: &mut [Command], raw_advance: f32, cell_width: f32) {
    if commands.is_empty() || cell_width <= 0.0 {
        return;
    }
    let bounds = zeno::bounds(&*commands, zeno::Fill::NonZero, None);
    let min_x = bounds.min.x;
    let max_x = bounds.max.x;
    let ink_w = (max_x - min_x).max(0.0);
    if ink_w == 0.0 {
        return;
    }
    // Leave a tiny margin inside the integer cell edges so floating-point
    // rounding cannot push floor(min_x) below 0 or ceil(max_x) past cell_width.
    const EPS: f32 = 1e-3;
    let safe_w = (cell_width - EPS).max(0.0);
    let mut scale_x = if raw_advance > cell_width {
        cell_width / raw_advance
    } else {
        1.0
    };
    if ink_w * scale_x > safe_w && safe_w > 0.0 {
        scale_x = safe_w / ink_w;
    }
    let mut tx = if raw_advance > 0.0 {
        ((cell_width - raw_advance * scale_x) * 0.5).round()
    } else {
        ((cell_width - ink_w * scale_x) * 0.5 - min_x * scale_x).round()
    };
    let mut new_min = min_x * scale_x + tx;
    let mut new_max = max_x * scale_x + tx;
    if new_min < EPS * 0.5 {
        let shift = EPS * 0.5 - new_min;
        tx += shift;
        new_min += shift;
        new_max += shift;
    }
    if new_max > cell_width - EPS * 0.5 {
        let shift = new_max - (cell_width - EPS * 0.5);
        tx -= shift;
        new_min -= shift;
    }
    if new_min < 0.0 {
        tx -= new_min;
    }
    if (scale_x - 1.0).abs() < 1e-6 && tx.abs() < 1e-6 {
        return;
    }
    let map = |p: Vector| Vector::new(p.x * scale_x + tx, p.y);
    for command in commands {
        *command = match *command {
            Command::MoveTo(p) => Command::MoveTo(map(p)),
            Command::LineTo(p) => Command::LineTo(map(p)),
            Command::QuadTo(a, p) => Command::QuadTo(map(a), map(p)),
            Command::CurveTo(a, b, p) => Command::CurveTo(map(a), map(b), map(p)),
            Command::Close => Command::Close,
        };
    }
}

/// Apply GDI synthetic italic shear horizontally after upright cell fitting.
pub(super) fn shear_italic(commands: &mut [Command], xscale: f32) {
    let slope = 0.21256 * xscale;
    let map = |p: Vector| Vector::new(p.x + p.y * slope, p.y);
    for command in commands {
        *command = match *command {
            Command::MoveTo(p) => Command::MoveTo(map(p)),
            Command::LineTo(p) => Command::LineTo(map(p)),
            Command::QuadTo(a, p) => Command::QuadTo(map(a), map(p)),
            Command::CurveTo(a, b, p) => Command::CurveTo(map(a), map(b), map(p)),
            Command::Close => Command::Close,
        };
    }
}
