use super::{Content, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mapping {
    /// Source texel coordinate = destination pixel coordinate * scale + offset.
    pub(crate) scale: [f64; 2],
    pub(crate) offset: [f64; 2],
    pub(crate) radius: f64,
    pub(crate) source: Rect,
    pub(crate) repeat: [bool; 2],
}
impl Mapping {
    #[cfg(test)]
    pub(crate) fn bits(self) -> ([u64; 2], [u64; 2], u64, Rect, [bool; 2]) {
        (
            self.scale.map(f64::to_bits),
            self.offset.map(f64::to_bits),
            self.radius.to_bits(),
            self.source,
            self.repeat,
        )
    }

    pub(crate) fn new(
        scale: [f64; 2],
        offset: [f64; 2],
        radius: f64,
        source: Rect,
    ) -> Option<Self> {
        (scale.iter().all(|v| v.is_finite() && *v != 0.) && offset.iter().all(|v| v.is_finite()))
            .then_some(Self {
                scale,
                offset,
                radius,
                source,
                repeat: [false; 2],
            })
    }
    /// The same pixel of a `source` extent, as depth and stencil tests read.
    pub(crate) fn identity(source: Rect) -> Self {
        Self {
            scale: [1.; 2],
            offset: [0.; 2],
            radius: 0.,
            source,
            repeat: [false; 2],
        }
    }
    /// Bounds the source texels that destination pixels in `pixels` read.
    pub(crate) fn unmap(self, pixels: Rect) -> Rect {
        if pixels.empty() || self.source.empty() {
            return Rect::default();
        }
        let lo = [f64::from(pixels.x), f64::from(pixels.y)];
        let hi = [
            f64::from(pixels.x + pixels.w),
            f64::from(pixels.y + pixels.h),
        ];
        let first = [self.source.x, self.source.y];
        let last = [
            self.source.x + self.source.w - 1,
            self.source.y + self.source.h - 1,
        ];
        let mut start = [0; 2];
        let mut end = [0; 2];
        for n in 0..2 {
            let a = lo[n] * self.scale[n] + self.offset[n];
            let b = hi[n] * self.scale[n] + self.offset[n];
            let lo = (a.min(b) - self.radius).floor();
            let hi = (a.max(b) + self.radius).ceil() - 1.;
            let first = f64::from(first[n]);
            let last = f64::from(last[n]);
            // The two sides of a wrapped read share one bounding interval.
            let wraps = self.repeat[n] && (lo < first || hi > last);
            start[n] = if wraps { first } else { lo.clamp(first, last) } as u32;
            end[n] = if wraps { last } else { hi.clamp(first, last) } as u32 + 1;
        }
        Rect {
            x: start[0],
            y: start[1],
            w: end[0] - start[0],
            h: end[1] - start[1],
        }
    }
    pub(crate) fn map(self, damage: Rect, bounds: Rect) -> Rect {
        let damage = damage.clipped(self.source);
        if damage.empty() {
            return Rect::default();
        }
        // Clamped addressing repeats edge texels without limit, so damage on
        // a source edge reaches every coordinate beyond it.
        let edge = |axis: usize, at: u32, limit: u32, value: f64, beyond: f64| {
            if !self.repeat[axis] && at == limit {
                beyond
            } else {
                value
            }
        };
        let source = self.source;
        let lo = [
            edge(
                0,
                damage.x,
                source.x,
                f64::from(damage.x) - self.radius,
                f64::NEG_INFINITY,
            ),
            edge(
                1,
                damage.y,
                source.y,
                f64::from(damage.y) - self.radius,
                f64::NEG_INFINITY,
            ),
        ];
        let hi = [
            edge(
                0,
                damage.x + damage.w,
                source.x + source.w,
                f64::from(damage.x + damage.w) + self.radius,
                f64::INFINITY,
            ),
            edge(
                1,
                damage.y + damage.h,
                source.y + source.h,
                f64::from(damage.y + damage.h) + self.radius,
                f64::INFINITY,
            ),
        ];
        let mut start = [0; 2];
        let mut end = [0; 2];
        let min = [bounds.x, bounds.y];
        let max = [bounds.x + bounds.w, bounds.y + bounds.h];
        let extent = [source.w, source.h];
        for n in 0..2 {
            start[n] = max[n];
            end[n] = min[n];
            // The proven coordinate range and filter reach at most the two
            // adjacent periods (one-texel axes are equivalent to clamping).
            let periods = i32::from(self.repeat[n]);
            for period in -periods..=periods {
                let shift = f64::from(period) * f64::from(extent[n]);
                let a = (lo[n] + shift - self.offset[n]) / self.scale[n];
                let b = (hi[n] + shift - self.offset[n]) / self.scale[n];
                let lo = (a.min(b).floor() as u32).max(min[n]);
                let hi = (a.max(b).ceil() as u32).min(max[n]);
                if lo < hi {
                    start[n] = start[n].min(lo);
                    end[n] = end[n].max(hi);
                }
            }
        }
        Rect {
            x: start[0],
            y: start[1],
            w: end[0].saturating_sub(start[0]),
            h: end[1].saturating_sub(start[1]),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Read {
    pub source: Content,
    pub mapping: Mapping,
}
