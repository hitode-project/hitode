//! Cardinal splines become cubic segments without discarding neighbor tangents.
use super::*;
impl Path {
    pub(super) fn curve(
        &mut self,
        points: &[[f32; 2]],
        offset: i32,
        segments: i32,
        tension: f32,
        closed: bool,
    ) -> Result<(), i32> {
        if points.len() < if closed { 3 } else { 2 }
            || offset < 0
            || segments <= 0
            || !tension.is_finite()
            || tension < 0.
            || points.iter().flatten().any(|v| !v.is_finite())
            || !closed
                && (offset as usize >= points.len()
                    || segments as usize >= points.len() - offset as usize)
            || closed && (offset != 0 || segments as usize != points.len())
        {
            return Err(2);
        }
        let at = |i: isize| -> Point {
            let i = if closed {
                i.rem_euclid(points.len() as isize) as usize
            } else {
                i.clamp(0, points.len() as isize - 1) as usize
            };
            Point::new(points[i][0], points[i][1])
        };
        let scale = tension / 3.;
        if closed {
            self.closed = true;
        }
        self.connect(at(offset as isize));
        for i in offset as isize..offset as isize + segments as isize {
            let (before, start, end, after) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            self.commands.push(Command::CurveTo(
                Point::new(
                    start.x + (end.x - before.x) * scale,
                    start.y + (end.y - before.y) * scale,
                ),
                Point::new(
                    end.x - (after.x - start.x) * scale,
                    end.y - (after.y - start.y) * scale,
                ),
                end,
            ));
            self.current = Some(end);
            self.count += 3;
        }
        if closed {
            self.close();
        }
        Ok(())
    }
}
