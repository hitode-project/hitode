//! Synthetic weight keeps the original contour verbs and expands their joins.
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
            points[i] = old[i] + (a + b) * scale + Vector::new(strength * 0.5, strength * 0.5);
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
