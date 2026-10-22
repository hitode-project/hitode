//! Flat path points preserve cubic controls, figure boundaries and marker flags.
use super::*;
use crate::gdiplus::PathEdit;

impl Path {
    pub fn data(&self) -> (Vec<[f32; 2]>, Vec<u8>) {
        let mut points = Vec::new();
        let mut types = Vec::new();
        fn push(points: &mut Vec<[f32; 2]>, types: &mut Vec<u8>, point: Point, kind: u8) {
            points.push([point.x, point.y]);
            types.push(kind);
        }
        for command in &self.commands {
            match *command {
                Command::MoveTo(p) => push(&mut points, &mut types, p, 0),
                Command::LineTo(p) => push(&mut points, &mut types, p, 1),
                Command::CurveTo(a, b, c) => {
                    push(&mut points, &mut types, a, 3);
                    push(&mut points, &mut types, b, 3);
                    push(&mut points, &mut types, c, 3);
                }
                Command::Close => {
                    if let Some(last) = types.last_mut() {
                        *last |= 0x80;
                    }
                }
                Command::QuadTo(..) => unreachable!("GDI+ paths store cubic segments"),
            }
        }
        for &index in &self.markers {
            if let Some(kind) = types.get_mut(index) {
                *kind |= 0x20;
            }
        }
        (points, types)
    }
    pub fn from_points(points: &[[f32; 2]], types: &[u8], mode: i32) -> Result<Self, i32> {
        if points.is_empty()
            || points.len() != types.len()
            || points.iter().flatten().any(|n| !n.is_finite())
        {
            return Err(2);
        }
        let mut result = Self::new(mode)?;
        let mut index = 0;
        while index < points.len() {
            let point = Point::new(points[index][0], points[index][1]);
            match types[index] & 7 {
                0 => {
                    result.commands.push(Command::MoveTo(point));
                    result.start = Some(point);
                    result.current = Some(point);
                    result.closed = false;
                    result.count += 1;
                }
                1 if result.current.is_some() => {
                    result.commands.push(Command::LineTo(point));
                    result.current = Some(point);
                    result.count += 1;
                }
                3 if result.current.is_some()
                    && index + 2 < points.len()
                    && types[index..index + 3].iter().all(|kind| kind & 7 == 3) =>
                {
                    let p = |i: usize| Point::new(points[i][0], points[i][1]);
                    result
                        .commands
                        .push(Command::CurveTo(p(index), p(index + 1), p(index + 2)));
                    result.current = Some(p(index + 2));
                    result.count += 3;
                    for (i, &kind) in types.iter().enumerate().skip(index).take(2) {
                        if kind & 0x80 != 0 {
                            return Err(2);
                        }
                        if kind & 0x20 != 0 {
                            result.markers.insert(i);
                        }
                    }
                    index += 2;
                }
                _ => return Err(2),
            }
            if types[index] & 0x20 != 0 {
                result.markers.insert(index);
            }
            if types[index] & 0x80 != 0 {
                result.close();
            }
            index += 1;
        }
        Ok(result)
    }
    fn lines(&mut self, points: &[[f32; 2]], polygon: bool) -> Result<(), i32> {
        if points.len() < if polygon { 3 } else { 2 }
            || points.iter().flatten().any(|n| !n.is_finite())
        {
            return Err(2);
        }
        if polygon {
            self.closed = true;
        }
        self.connect(Point::new(points[0][0], points[0][1]));
        for p in &points[1..] {
            let p = Point::new(p[0], p[1]);
            self.commands.push(Command::LineTo(p));
            self.current = Some(p);
            self.count += 1;
        }
        if polygon {
            self.close();
        }
        Ok(())
    }
    pub fn edit(&mut self, edit: PathEdit<'_>) -> Result<(), i32> {
        match edit {
            PathEdit::Lines(points, polygon) => self.lines(points, polygon)?,
            PathEdit::Beziers(points) => {
                if points.len() < 4
                    || points.len() % 3 != 1
                    || points.iter().flatten().any(|n| !n.is_finite())
                {
                    return Err(2);
                }
                self.connect(Point::new(points[0][0], points[0][1]));
                for points in points[1..].as_chunks::<3>().0 {
                    let p = |i: usize| Point::new(points[i][0], points[i][1]);
                    self.commands.push(Command::CurveTo(p(0), p(1), p(2)));
                    self.current = Some(p(2));
                    self.count += 3;
                }
            }
            PathEdit::Rectangles(rectangles) => {
                if rectangles.is_empty() || rectangles.iter().flatten().any(|n| !n.is_finite()) {
                    return Err(2);
                }
                for &[x, y, w, h] in rectangles {
                    if w == 0. || h == 0. {
                        continue;
                    }
                    self.lines(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]], true)?;
                }
            }
            PathEdit::Ellipse([x, y, w, h]) => {
                if [x, y, w, h].iter().any(|n| !n.is_finite()) {
                    return Err(2);
                }
                let (cx, cy, rx, ry) = (x + w / 2., y + h / 2., w / 2., h / 2.);
                let k = 0.552_284_8;
                self.closed = true;
                self.connect(Point::new(cx + rx, cy));
                for p in [
                    [
                        [cx + rx, cy + k * ry],
                        [cx + k * rx, cy + ry],
                        [cx, cy + ry],
                    ],
                    [
                        [cx - k * rx, cy + ry],
                        [cx - rx, cy + k * ry],
                        [cx - rx, cy],
                    ],
                    [
                        [cx - rx, cy - k * ry],
                        [cx - k * rx, cy - ry],
                        [cx, cy - ry],
                    ],
                    [
                        [cx + k * rx, cy - ry],
                        [cx + rx, cy - k * ry],
                        [cx + rx, cy],
                    ],
                ] {
                    self.commands.push(Command::CurveTo(
                        Point::new(p[0][0], p[0][1]),
                        Point::new(p[1][0], p[1][1]),
                        Point::new(p[2][0], p[2][1]),
                    ));
                    self.current = Some(Point::new(p[2][0], p[2][1]));
                    self.count += 3;
                }
                self.close();
            }
            PathEdit::Pie(rect, start, sweep) => {
                // Native pie geometry is a center-to-arc closed figure.
                let mut next = self.clone();
                next.closed = true;
                next.connect(Point::new(rect[0] + rect[2] / 2., rect[1] + rect[3] / 2.));
                next.arc(rect, start, sweep)?;
                next.close();
                *self = next;
            }
            PathEdit::Start => self.closed = true,
            PathEdit::CloseAll => {
                let mut commands = Vec::new();
                let mut open = false;
                for command in &self.commands {
                    if matches!(command, Command::MoveTo(_)) && open {
                        commands.push(Command::Close);
                    }
                    commands.push(*command);
                    open = !matches!(command, Command::Close);
                }
                if open {
                    commands.push(Command::Close);
                }
                self.commands = commands;
                self.closed = true;
                self.current = self.start;
            }
            PathEdit::Marker(true) => {
                if self.count > 0 {
                    self.markers.insert(self.count as usize - 1);
                }
            }
            PathEdit::Marker(false) => self.markers.clear(),
            PathEdit::Transform(matrix) => *self = self.transformed(matrix),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: DEV-IMAGE-13
    fn degenerate_rectangles_and_ellipses_keep_native_path_structure() {
        for (w, h) in [(0., 0.), (0., 4.), (3., 0.)] {
            let mut path = Path::new(0).unwrap();
            path.edit(PathEdit::Rectangles(&[[1., 2., w, h]])).unwrap();
            assert_eq!(path.count, 0);
            path.edit(PathEdit::Ellipse([1., 2., w, h])).unwrap();
            assert_eq!(path.count, 13);
            let (points, types) = path.data();
            assert_eq!(points.len(), 13);
            assert_eq!(types, [0, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 0x83]);
            assert_eq!(points.first(), points.last());
        }
    }
}
