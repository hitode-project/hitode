//! Bounded application clipping owned by the logical DC, including SaveDC.
#[derive(Clone, Debug, Default)]
pub struct DcClip {
    excluded: Vec<[i32; 4]>,
    /// Intersection limit in device units; origin changes never relocate it.
    intersection: Option<[i32; 4]>,
    /// USER's current paint update region, separate from the application clip.
    paint_region: Option<Vec<[i32; 4]>>,
}
impl DcClip {
    pub const MAX_RECTANGLES: usize = 64;
    pub fn exclude(&mut self, rect: [i32; 4]) -> bool {
        let [l, t, r, b] = rect;
        let rect = [l.min(r), t.min(b), l.max(r), t.max(b)];
        if rect[0] == rect[2] || rect[1] == rect[3] || self.excluded.contains(&rect) {
            return true;
        }
        if self.excluded.len() == Self::MAX_RECTANGLES {
            return false;
        }
        self.excluded.push(rect);
        true
    }
    pub fn is_default(&self) -> bool {
        self.excluded.is_empty() && self.intersection.is_none() && self.paint_region.is_none()
    }
    /// USER validates the bounded non-overlapping rectangle union before lending
    /// it to this DC. Returning the previous layer supports nested BeginPaint.
    pub fn replace_paint_region(&mut self, next: Option<Vec<[i32; 4]>>) -> Option<Vec<[i32; 4]>> {
        std::mem::replace(&mut self.paint_region, next)
    }
    /// SaveDC captures the application's clipping state. The active USER paint
    /// layer follows its lease and cannot be removed by restoring an older DC.
    pub fn application_state(&self) -> Self {
        Self {
            excluded: self.excluded.clone(),
            intersection: self.intersection,
            paint_region: None,
        }
    }
    pub fn restore_application(&mut self, saved: Self) {
        self.excluded = saved.excluded;
        self.intersection = saved.intersection;
    }
    pub fn intersect(&mut self, rect: [i32; 4]) {
        let [l, t, r, b] = rect;
        let mut rect = [l.min(r), t.min(b), l.max(r), t.max(b)];
        if let Some([l, t, r, b]) = self.intersection {
            rect = [
                rect[0].max(l),
                rect[1].max(t),
                rect[2].min(r),
                rect[3].min(b),
            ];
        }
        if rect[0] >= rect[2] || rect[1] >= rect[3] {
            rect = [0; 4];
        }
        self.intersection = Some(rect);
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.intersection
            .is_none_or(|[l, t, r, b]| x >= l && x < r && y >= t && y < b)
            && self.paint_region.as_ref().is_none_or(|rects| {
                rects
                    .iter()
                    .any(|&[l, t, r, b]| x >= l && x < r && y >= t && y < b)
            })
            && !self
                .excluded
                .iter()
                .any(|&[l, t, r, b]| x >= l && x < r && y >= t && y < b)
    }
    /// Visible, disjoint spans in one surface row. Right/bottom are exclusive.
    pub fn spans(&self, y: i32, width: u32) -> Vec<(u32, u32)> {
        let (l, r) = if let Some([l, t, r, b]) = self.intersection {
            if y < t || y >= b {
                return Vec::new();
            }
            ((l.max(0) as u32).min(width), (r.max(0) as u32).min(width))
        } else {
            (0, width)
        };
        if l >= r {
            return Vec::new();
        }
        let mut spans = if let Some(rects) = &self.paint_region {
            let mut spans = rects
                .iter()
                .filter_map(|&[a, t, z, b]| {
                    if y < t || y >= b {
                        return None;
                    }
                    let a = (a.max(0) as u32).max(l);
                    let z = (z.max(0) as u32).min(r);
                    (a < z).then_some((a, z))
                })
                .collect::<Vec<_>>();
            spans.sort_unstable();
            let mut joined: Vec<(u32, u32)> = Vec::new();
            for (a, z) in spans {
                if let Some(last) = joined.last_mut()
                    && a <= last.1
                {
                    last.1 = last.1.max(z);
                } else {
                    joined.push((a, z));
                }
            }
            joined
        } else {
            vec![(l, r)]
        };
        for &[l, t, r, b] in &self.excluded {
            if y < t || y >= b {
                continue;
            }
            let l = l.max(0) as u32;
            let r = (r.max(0) as u32).min(width);
            if l >= r {
                continue;
            }
            let mut next = Vec::with_capacity(spans.len() + 1);
            for (a, z) in spans {
                if z <= l || a >= r {
                    next.push((a, z));
                } else {
                    if a < l {
                        next.push((a, l));
                    }
                    if z > r {
                        next.push((r, z));
                    }
                }
            }
            spans = next;
        }
        spans
    }
    pub fn visible(&self, width: u32, height: u32) -> bool {
        // Region topology changes only at rectangle boundaries, not every row.
        if width == 0 || height == 0 {
            return false;
        }
        if !self.spans(0, width).is_empty() {
            return true;
        }
        self.edges()
            .filter(|&y| y >= 0 && (y as u32) < height)
            .any(|y| !self.spans(y, width).is_empty())
    }

    /// Tight device bounds and GDI complexity of the final surface-visible mask.
    /// Use constant-topology row bands, including USER paint boundaries, rather
    /// than scanning pixels or constructing a second region representation.
    pub fn bounding_box(&self, width: u32, height: u32) -> Option<(u32, [i32; 4])> {
        let width_i32 = i32::try_from(width).ok()?;
        let height_i32 = i32::try_from(height).ok()?;
        let mut edges = vec![0, height_i32];
        edges.extend(self.edges().filter(|&y| y > 0 && y < height_i32));
        edges.sort_unstable();
        edges.dedup();
        let mut bounds = [width_i32, height_i32, 0, 0];
        let mut area = 0_u64;
        for band in edges.windows(2) {
            let spans = self.spans(band[0], width);
            if let (Some(&(left, _)), Some(&(_, right))) = (spans.first(), spans.last()) {
                bounds[0] = bounds[0].min(left as i32);
                bounds[1] = bounds[1].min(band[0]);
                bounds[2] = bounds[2].max(right as i32);
                bounds[3] = bounds[3].max(band[1]);
                area += spans.iter().map(|&(l, r)| u64::from(r - l)).sum::<u64>()
                    * (band[1] - band[0]) as u64;
            }
        }
        if area == 0 {
            return Some((1, [0; 4]));
        }
        let rectangle_area = (bounds[2] - bounds[0]) as u64 * (bounds[3] - bounds[1]) as u64;
        Some((if area == rectangle_area { 2 } else { 3 }, bounds))
    }

    /// RectVisible's MM_TEXT query over the selected surface's visible bounds.
    /// Native normalizes reversed coordinates and gives zero-length axes one
    /// logical unit, including a point rectangle. Use i64 for the i32::MAX edge.
    pub fn intersects(&self, rect: [i32; 4], width: u32, height: u32) -> bool {
        self.intersects_device(rect.map(i64::from), width, height)
    }

    /// Input has already been translated into the fixed device clipping space.
    pub fn intersects_device(&self, rect: [i64; 4], width: u32, height: u32) -> bool {
        let [l, t, r, b] = rect;
        let (l, t, r, b) = (l.min(r), t.min(b), l.max(r), t.max(b));
        let r = if r == l { r + 1 } else { r };
        let b = if b == t { b + 1 } else { b };
        let (l, t, r, b) = (
            l.max(0),
            t.max(0),
            r.min(i64::from(width)),
            b.min(i64::from(height)),
        );
        if l >= r || t >= b {
            return false;
        }
        let visible = |y| {
            self.spans(y, width)
                .into_iter()
                .any(|(a, z)| i64::from(a) < r && i64::from(z) > l)
        };
        if visible(t as i32) {
            return true;
        }
        // The row topology is constant between exclusion edges: no pixel-height
        // loop and at most two event rows per bounded exclusion rectangle.
        self.edges()
            .filter(|&y| i64::from(y) >= t && i64::from(y) < b)
            .any(visible)
    }

    fn edges(&self) -> impl Iterator<Item = i32> + '_ {
        self.excluded
            .iter()
            .flat_map(|r| [r[1], r[3]])
            .chain(self.intersection.into_iter().flat_map(|r| [r[1], r[3]]))
            .chain(
                self.paint_region
                    .iter()
                    .flatten()
                    .flat_map(|r| [r[1], r[3]]),
            )
    }
}
