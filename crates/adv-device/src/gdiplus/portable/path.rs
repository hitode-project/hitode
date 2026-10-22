//! Owned path geometry shared by CPU coverage, bounds and gradient brushes.
use zeno::{Command, Point};

#[derive(Clone)]
pub(in crate::gdiplus) struct Path {
    pub commands: Vec<Command>,
    pub winding: bool,
    pub count: i32,
    current: Option<Point>,
    start: Option<Point>,
    closed: bool,
}
impl Path {
    pub fn transformed(&self, matrix: super::matrix::Matrix) -> Self {
        let [a, b, c, d, x, y] = matrix.0;
        let transform = zeno::Transform::new(a, b, c, d, x, y);
        let mut path = self.clone();
        for command in &mut path.commands {
            *command = command.transform(&transform);
        }
        path.current = path.current.map(|point| transform.transform_point(point));
        path.start = path.start.map(|point| transform.transform_point(point));
        path
    }
    pub fn new(mode: i32) -> Result<Self, i32> {
        if !(0..=1).contains(&mode) {
            return Err(2);
        }
        Ok(Self {
            commands: Vec::new(),
            winding: mode == 1,
            count: 0,
            current: None,
            start: None,
            closed: false,
        })
    }
    fn connect(&mut self, point: Point) {
        if self.current.is_none() || self.closed {
            self.commands.push(Command::MoveTo(point));
            self.start = Some(point);
            self.count += 1;
        } else if self.current != Some(point) {
            self.commands.push(Command::LineTo(point));
            self.count += 1;
        }
        self.current = Some(point);
        self.closed = false;
    }
    pub fn line(&mut self, [x1, y1, x2, y2]: [f32; 4]) -> Result<(), i32> {
        if [x1, y1, x2, y2].iter().any(|x| !x.is_finite()) {
            return Err(2);
        }
        self.connect(Point::new(x1, y1));
        self.commands.push(Command::LineTo(Point::new(x2, y2)));
        self.current = Some(Point::new(x2, y2));
        self.count += 1;
        Ok(())
    }
    pub fn arc(&mut self, rect: [f32; 4], start: f32, sweep: f32) -> Result<(), i32> {
        let [x, y, w, h] = rect;
        if rect.iter().chain([&start, &sweep]).any(|x| !x.is_finite()) || w <= 0. || h <= 0. {
            return Err(2);
        }
        let sweep = sweep.clamp(-360., 360.);
        if sweep == 0. {
            return Ok(());
        }
        let n = (sweep.abs() / 90.).ceil() as usize;
        let step = sweep.to_radians() / n as f32;
        let mut angle = start.rem_euclid(360.).to_radians();
        let point = |a: f32| Point::new(x + w * (1. + a.cos()) / 2., y + h * (1. + a.sin()) / 2.);
        self.connect(point(angle));
        for _ in 0..n {
            let end = angle + step;
            let k = 4. / 3. * (step / 4.).tan();
            let a = point(angle);
            let b = point(end);
            self.commands.push(Command::CurveTo(
                Point::new(
                    a.x - k * w * angle.sin() / 2.,
                    a.y + k * h * angle.cos() / 2.,
                ),
                Point::new(b.x + k * w * end.sin() / 2., b.y - k * h * end.cos() / 2.),
                b,
            ));
            self.count += 3;
            self.current = Some(b);
            angle = end;
        }
        Ok(())
    }
    pub fn close(&mut self) {
        if self.current.is_some() && !self.closed {
            self.commands.push(Command::Close);
            self.current = self.start;
            self.closed = true;
        }
    }
    pub fn fill(&self) -> zeno::Fill {
        if self.winding {
            zeno::Fill::NonZero
        } else {
            zeno::Fill::EvenOdd
        }
    }
    pub fn bounds(&self) -> [f32; 4] {
        if self.count == 0 {
            return [0.; 4];
        }
        let bounds = zeno::bounds(&self.commands, self.fill(), None);
        [
            bounds.min.x,
            bounds.min.y,
            bounds.max.x - bounds.min.x,
            bounds.max.y - bounds.min.y,
        ]
    }
    /// Aliased coverage tests pixel centers, rather than thresholding area coverage.
    pub fn coverage(&self, width: u32, height: u32, offset: f32) -> Vec<u8> {
        let mut result = vec![0; width as usize * height as usize];
        for y in 0..height {
            let hits = self.crossings(f64::from(y) + f64::from(offset));
            let mut winding: i32 = hits.iter().map(|hit| hit.1).sum();
            let mut at = 0;
            for x in 0..width {
                let position = f64::from(x) + f64::from(offset);
                while at < hits.len() && hits[at].0 <= position {
                    winding -= hits[at].1;
                    at += 1;
                }
                if if self.winding {
                    winding != 0
                } else {
                    winding % 2 != 0
                } {
                    result[y as usize * width as usize + x as usize] = 255;
                }
            }
        }
        result
    }
    fn crossings(&self, y: f64) -> Vec<(f64, i32)> {
        let mut hits = Vec::new();
        let mut current = None;
        let mut start = None;
        for command in &self.commands {
            match *command {
                Command::MoveTo(point) => {
                    if let (Some(a), Some(b)) = (current, start) {
                        line_cross(a, b, y, &mut hits);
                    }
                    current = Some(point);
                    start = Some(point);
                }
                Command::LineTo(point) => {
                    if let Some(a) = current {
                        line_cross(a, point, y, &mut hits);
                    }
                    current = Some(point);
                }
                Command::CurveTo(b, c, d) => {
                    if let Some(a) = current {
                        curve_cross([a, b, c, d], y, &mut hits);
                    }
                    current = Some(d);
                }
                Command::QuadTo(b, c) => {
                    if let Some(a) = current {
                        curve_cross(
                            [
                                a,
                                Point::new(
                                    a.x + (b.x - a.x) * 2. / 3.,
                                    a.y + (b.y - a.y) * 2. / 3.,
                                ),
                                Point::new(
                                    c.x + (b.x - c.x) * 2. / 3.,
                                    c.y + (b.y - c.y) * 2. / 3.,
                                ),
                                c,
                            ],
                            y,
                            &mut hits,
                        );
                    }
                    current = Some(c);
                }
                Command::Close => {
                    if let (Some(a), Some(b)) = (current, start) {
                        line_cross(a, b, y, &mut hits);
                    }
                    current = start;
                }
            }
        }
        if let (Some(a), Some(b)) = (current, start) {
            line_cross(a, b, y, &mut hits);
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        hits
    }
}

fn line_cross(a: Point, b: Point, y: f64, hits: &mut Vec<(f64, i32)>) {
    let (ay, by) = (f64::from(a.y), f64::from(b.y));
    if ay <= y && by > y || by <= y && ay > y {
        let t = (y - ay) / (by - ay);
        hits.push((
            f64::from(a.x) + (f64::from(b.x) - f64::from(a.x)) * t,
            if by > ay { 1 } else { -1 },
        ));
    }
}
fn curve_cross(points: [Point; 4], y: f64, hits: &mut Vec<(f64, i32)>) {
    let xs = points.map(|p| f64::from(p.x));
    let ys = points.map(|p| f64::from(p.y));
    let a = -ys[0] + 3. * ys[1] - 3. * ys[2] + ys[3];
    let b = 3. * ys[0] - 6. * ys[1] + 3. * ys[2];
    let c = -3. * ys[0] + 3. * ys[1];
    let mut cuts = vec![0., 1.];
    if a.abs() > f64::EPSILON {
        let disc = 4. * b * b - 12. * a * c;
        if disc >= 0. {
            for root in [
                (-2. * b - disc.sqrt()) / (6. * a),
                (-2. * b + disc.sqrt()) / (6. * a),
            ] {
                if root > 0. && root < 1. {
                    cuts.push(root);
                }
            }
        }
    } else if b.abs() > f64::EPSILON {
        let root = -c / (2. * b);
        if root > 0. && root < 1. {
            cuts.push(root);
        }
    }
    cuts.sort_by(f64::total_cmp);
    for pair in cuts.windows(2) {
        let (mut lo, mut hi) = (pair[0], pair[1]);
        let (low, high) = (bezier(ys, lo), bezier(ys, hi));
        if !(low <= y && high > y || high <= y && low > y) {
            continue;
        }
        let ascending = high > low;
        for _ in 0..48 {
            let mid = (lo + hi) / 2.;
            if (bezier(ys, mid) < y) == ascending {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hits.push((bezier(xs, (lo + hi) / 2.), if ascending { 1 } else { -1 }));
    }
}
fn bezier(p: [f64; 4], t: f64) -> f64 {
    let u = 1. - t;
    p[0] * u * u * u + 3. * p[1] * u * u * t + 3. * p[2] * u * t * t + p[3] * t * t * t
}
