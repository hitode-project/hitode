//! Damage as a cell bitmap (#116). Every layer of the display chain carries
//! changed pixels in this form, so no layer collapses them into bounds.

use crate::Rect;

/// Side of a damage cell in pixels.
pub const CELL: u32 = 8;

/// Changed pixels of an image, one bit per [`CELL`]-square cell. It covers
/// every changed pixel; cells on the right and bottom edges may be partial.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Region {
    size: (u32, u32),
    /// Words per cell row.
    stride: usize,
    bits: Vec<u64>,
}

impl Region {
    /// Nothing changed in an image of `size` pixels.
    pub fn empty(size: (u32, u32)) -> Self {
        let (columns, rows) = cells(size);
        let stride = columns.div_ceil(64) as usize;
        Self {
            size,
            stride,
            bits: vec![0; stride * rows as usize],
        }
    }

    /// Everything changed.
    pub fn whole(size: (u32, u32)) -> Self {
        let mut region = Self::empty(size);
        region.add(Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        });
        region
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&w| w == 0)
    }

    pub fn is_whole(&self) -> bool {
        let (columns, rows) = cells(self.size);
        let set: u64 = self.bits.iter().map(|w| u64::from(w.count_ones())).sum();
        set == u64::from(columns) * u64::from(rows)
    }

    /// Marks the cells `rect` touches, clipped to the image.
    pub fn add(&mut self, rect: Rect) {
        if let Some((x0, x1, y0, y1)) = self.span(rect) {
            for y in y0..y1 {
                let row = &mut self.bits[y as usize * self.stride..][..self.stride];
                let mut x = x0 as usize;
                while x < x1 as usize {
                    let word = x / 64;
                    let end = (x1 as usize).min(word * 64 + 64);
                    row[word] |= mask(x % 64, end - word * 64);
                    x = end;
                }
            }
        }
    }

    /// Adds `other`. A region of another size changed an image this region
    /// cannot describe, so everything changed.
    pub fn union(&mut self, other: &Self) {
        if other.size != self.size {
            *self = Self::whole(self.size);
            return;
        }
        for (a, b) in self.bits.iter_mut().zip(&other.bits) {
            *a |= b;
        }
    }

    /// Keeps only the cells `other` marks too. A region of another size
    /// cannot narrow this one.
    pub fn intersect(&mut self, other: &Self) {
        if other.size == self.size {
            for (a, b) in self.bits.iter_mut().zip(&other.bits) {
                *a &= b;
            }
        }
    }

    /// Whether any cell `rect` touches is marked.
    pub fn intersects(&self, rect: Rect) -> bool {
        let Some((x0, x1, y0, y1)) = self.span(rect) else {
            return false;
        };
        (y0..y1).any(|y| {
            let row = self.row(y);
            let mut x = x0 as usize;
            while x < x1 as usize {
                let word = x / 64;
                let end = (x1 as usize).min(word * 64 + 64);
                if row[word] & mask(x % 64, end - word * 64) != 0 {
                    return true;
                }
                x = end;
            }
            false
        })
    }

    /// The smallest rectangle covering every marked cell, clipped to the image.
    pub fn bound(&self) -> Option<Rect> {
        let (columns, rows) = cells(self.size);
        let (mut lo, mut hi, mut top, mut bottom) = (columns, 0, None, 0);
        for y in 0..rows {
            for (a, b) in runs(self.row(y), columns) {
                lo = lo.min(a);
                hi = hi.max(b);
                top.get_or_insert(y);
                bottom = y + 1;
            }
        }
        Some(self.pixels(lo, hi, top?, bottom))
    }

    /// Disjoint rectangles covering exactly the marked cells, clipped to the
    /// image: runs of cells in each row, merged with identical runs below.
    pub fn rects(&self) -> Vec<Rect> {
        let (columns, rows) = cells(self.size);
        let mut out = Vec::new();
        // Open runs as (first column, end column, first row), sorted by column.
        let mut open: Vec<(u32, u32, u32)> = Vec::new();
        let mut next = Vec::new();
        for y in 0..rows {
            let mut i = 0;
            for (a, b) in runs(self.row(y), columns) {
                let mut extended = None;
                while i < open.len() && open[i].0 <= a {
                    let (oa, ob, top) = open[i];
                    if (oa, ob) == (a, b) {
                        extended = Some(top);
                    } else {
                        out.push(self.pixels(oa, ob, top, y));
                    }
                    i += 1;
                }
                next.push((a, b, extended.unwrap_or(y)));
            }
            for &(a, b, top) in &open[i..] {
                out.push(self.pixels(a, b, top, y));
            }
            std::mem::swap(&mut open, &mut next);
            next.clear();
        }
        for (a, b, top) in open {
            out.push(self.pixels(a, b, top, rows));
        }
        out
    }

    /// At most `max` (at least one) disjoint rectangles covering every marked
    /// cell: the [`rects`](Self::rects) of the finest grid of `CELL << k`
    /// squares that needs no more of them.
    pub fn parts(&self, max: usize) -> Vec<Rect> {
        let mut out = self.rects();
        let (columns, rows) = cells(self.size);
        let mut k = 0;
        while out.len() > max.max(1) {
            k += 1;
            let mut coarse = Self::empty((
                columns.div_ceil(1 << k) * CELL,
                rows.div_ceil(1 << k) * CELL,
            ));
            for y in 0..rows {
                for (a, b) in runs(self.row(y), columns) {
                    let (a, b) = (a >> k, (b - 1) >> k);
                    coarse.add(Rect {
                        x: a * CELL,
                        y: (y >> k) * CELL,
                        w: (b - a + 1) * CELL,
                        h: CELL,
                    });
                }
            }
            out = coarse
                .rects()
                .into_iter()
                .map(|r| {
                    let (x, y) = (r.x << k, r.y << k);
                    Rect {
                        x,
                        y,
                        w: ((r.x + r.w) << k).min(self.size.0) - x,
                        h: ((r.y + r.h) << k).min(self.size.1) - y,
                    }
                })
                .collect();
        }
        out
    }

    /// Cell range of `rect` clipped to the image, if any.
    fn span(&self, rect: Rect) -> Option<(u32, u32, u32, u32)> {
        let x1 = rect.x.saturating_add(rect.w).min(self.size.0);
        let y1 = rect.y.saturating_add(rect.h).min(self.size.1);
        (rect.x < x1 && rect.y < y1).then(|| {
            (
                rect.x / CELL,
                x1.div_ceil(CELL),
                rect.y / CELL,
                y1.div_ceil(CELL),
            )
        })
    }

    fn row(&self, y: u32) -> &[u64] {
        &self.bits[y as usize * self.stride..][..self.stride]
    }

    fn pixels(&self, x0: u32, x1: u32, y0: u32, y1: u32) -> Rect {
        let (x, y) = (x0 * CELL, y0 * CELL);
        Rect {
            x,
            y,
            w: (x1 * CELL).min(self.size.0) - x,
            h: (y1 * CELL).min(self.size.1) - y,
        }
    }
}

fn cells((w, h): (u32, u32)) -> (u32, u32) {
    (w.div_ceil(CELL), h.div_ceil(CELL))
}

/// Bits `lo..hi` of a word, `hi <= 64`.
fn mask(lo: usize, hi: usize) -> u64 {
    let ones = if hi - lo == 64 {
        !0
    } else {
        (1u64 << (hi - lo)) - 1
    };
    ones << lo
}

/// Runs of marked cells in one row as (first, end) columns.
fn runs(row: &[u64], columns: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
    let columns = columns as usize;
    let find = move |from: usize, set: bool| {
        let mut x = from;
        while x < columns {
            let word = if set { row[x / 64] } else { !row[x / 64] } >> (x % 64);
            if word != 0 {
                return (x + word.trailing_zeros() as usize).min(columns);
            }
            x = (x / 64 + 1) * 64;
        }
        columns
    };
    let mut x = 0;
    std::iter::from_fn(move || {
        let a = find(x, true);
        if a >= columns {
            return None;
        }
        let b = find(a, false);
        x = b;
        Some((a as u32, b as u32))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Every pixel of the image is covered by `region.rects()` exactly when
    /// its cell is marked, and the rectangles are disjoint.
    fn check(region: &Region, marked: impl Fn(u32, u32) -> bool) {
        let (w, h) = region.size();
        let mut cover = vec![0u8; (w * h) as usize];
        for r in region.rects() {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    cover[(y * w + x) as usize] += 1;
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let expected = u8::from(marked(x / CELL, y / CELL));
                assert_eq!(cover[(y * w + x) as usize], expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn distant_changes_stay_separate() {
        let mut region = Region::empty((1280, 720));
        region.add(rect(3, 5, 2, 2));
        region.add(rect(1270, 710, 10, 10));
        assert_eq!(region.rects(), [rect(0, 0, 8, 8), rect(1264, 704, 16, 16)]);
        assert_eq!(region.bound(), Some(rect(0, 0, 1280, 720)));
        assert!(region.intersects(rect(7, 7, 1, 1)));
        assert!(!region.intersects(rect(8, 0, 1256, 704)));
    }

    #[test]
    fn rects_cover_irregular_shapes_exactly() {
        // Partial edge cells and a word boundary (columns 63..65).
        let size = (8 * 70 + 3, 8 * 9 + 5);
        let mut region = Region::empty(size);
        let shape = |x: u32, y: u32| (x * 7 + y * 3) % 5 < 2 || (62..66).contains(&x) || y == 9;
        for y in 0..10 {
            for x in 0..71 {
                if shape(x, y) {
                    region.add(rect(x * CELL + 1, y * CELL + 1, 1, 1));
                }
            }
        }
        check(&region, shape);
    }

    #[test]
    fn parts_stay_disjoint_and_cover_every_cell_within_the_limit() {
        let size = (8 * 70 + 3, 8 * 9 + 5);
        let mut region = Region::empty(size);
        let shape = |x: u32, y: u32| (x * 7 + y * 3) % 5 < 2;
        for y in 0..10 {
            for x in 0..71 {
                if shape(x, y) {
                    region.add(rect(x * CELL, y * CELL, 1, 1));
                }
            }
        }
        assert_eq!(region.parts(usize::MAX), region.rects());
        for max in [1, 3, 8, 40] {
            let parts = region.parts(max);
            assert!(parts.len() <= max, "{max}: {}", parts.len());
            let (w, h) = size;
            let mut cover = vec![0u8; (w * h) as usize];
            for r in &parts {
                assert!(r.x + r.w <= w && r.y + r.h <= h, "{r:?}");
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        cover[(y * w + x) as usize] += 1;
                    }
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let at = cover[(y * w + x) as usize];
                    assert!(at <= 1, "{max}: pixel {x},{y} in two parts");
                    assert!(at == 1 || !shape(x / CELL, y / CELL), "{max}: {x},{y}");
                }
            }
        }
        // Two distant changes stay two parts.
        let mut two = Region::empty((1280, 720));
        two.add(rect(3, 5, 2, 2));
        two.add(rect(1270, 710, 10, 10));
        assert_eq!(two.parts(8), [rect(0, 0, 8, 8), rect(1264, 704, 16, 16)]);
        assert_eq!(Region::empty((9, 9)).parts(8), []);
    }

    #[test]
    fn whole_and_union_by_size() {
        let size = (100, 50);
        let whole = Region::whole(size);
        assert!(whole.is_whole());
        assert_eq!(whole.rects(), [rect(0, 0, 100, 50)]);
        let mut a = Region::empty(size);
        assert!(a.is_empty() && a.bound().is_none() && a.rects().is_empty());
        let mut b = Region::empty(size);
        b.add(rect(40, 40, 100, 100));
        a.union(&b);
        assert_eq!(a.rects(), [rect(40, 40, 60, 10)]);
        a.union(&Region::empty((101, 50)));
        assert!(a.is_whole());
    }
}
