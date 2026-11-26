//! Direct3D 11 fixed-function tessellation of a single patch.
//!
//! Port of Microsoft's May 7, 2012 hardware reference (CHWTessellator), as
//! shipped in Mesa 25.3.0 under src/gallium/auxiliary/tessellator/.
//! https://gitlab.freedesktop.org/mesa/mesa/-/tree/mesa-25.3.0/src/gallium/auxiliary/tessellator
//! Factors and domain coordinates use the reference's 16-bit fixed-point
//! fraction. Connectivity follows its concentric rings and ruler-function
//! stitching at the transition from the exterior to the first interior ring.

/*
Copyright (c) Microsoft Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense,
and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/

mod surface;
#[cfg(test)]
mod tests;

/// Factors and output topology for the declared hull shader domain.
#[derive(Clone, Copy, Debug)]
pub enum Domain {
    Tri {
        /// Edges U=0, V=0, W=0.
        edges: [f32; 3],
        inside: f32,
        output: SurfaceOutput,
    },
    Quad {
        /// Edges U=0, V=0, U=1, V=1.
        edges: [f32; 4],
        /// Inside factors in U, V order.
        inside: [f32; 2],
        output: SurfaceOutput,
    },
    Isoline {
        /// First edge factor, the count of constant-V lines.
        density: f32,
        /// Second edge factor, the subdivision along U.
        detail: f32,
        output: IsolineOutput,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Partitioning {
    Integer,
    /// The hull shader supplies any power-of-two rounding. The fixed-function
    /// stage rounds the resulting factors as in `Integer`.
    Pow2,
    FractionalOdd,
    FractionalEven,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceOutput {
    Point,
    TriangleCw,
    TriangleCcw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsolineOutput {
    Point,
    Line,
}

#[derive(Debug, Default)]
pub struct Mesh {
    /// Barycentric U, V, W for triangles, or U, V, 0 for quads and isolines.
    pub points: Vec<[f32; 3]>,
    /// Point, line, or triangle list indices in groups of one, two, or three.
    pub indices: Vec<u32>,
}

/// Tessellate the factors written by a hull shader. A culled patch returns
/// empty vectors.
pub fn tessellate(domain: Domain, partitioning: Partitioning) -> Mesh {
    match domain {
        Domain::Tri {
            edges,
            inside,
            output,
        } => surface::tri(edges, inside, partitioning, output),
        Domain::Quad {
            edges,
            inside,
            output,
        } => surface::quad(edges, inside, partitioning, output),
        Domain::Isoline {
            density,
            detail,
            output,
        } => isoline(density, detail, partitioning, output),
    }
}

const ONE: u32 = 1 << 16;
const HALF: u32 = ONE / 2;
const EPSILON: f32 = 1.0 / ONE as f32;

impl Mesh {
    fn with_capacity(points: u32, indices: u32) -> Self {
        Self {
            points: Vec::with_capacity(points as usize),
            indices: Vec::with_capacity(indices as usize),
        }
    }

    fn point(&mut self, location: [u32; 3]) {
        self.points.push(location.map(|x| x as f32 * EPSILON));
    }
}

impl Partitioning {
    fn bounds(self) -> (f32, f32) {
        match self {
            Self::Integer | Self::Pow2 => (1.0, 64.0),
            Self::FractionalOdd => (1.0, 63.0),
            Self::FractionalEven => (2.0, 64.0),
        }
    }

    fn factor(self, value: f32, lower: f32, upper: f32, inside: bool) -> Factor {
        // D3D max maps a NaN inside factor to the lower bound.
        let clamped = value.max(lower).min(upper);
        let (value, odd) = match self {
            Self::Integer | Self::Pow2 => {
                let rounded = clamped.ceil() as u32;
                // A factor of one on the inside uses the even midpoint.
                (rounded * ONE, rounded & 1 != 0 && !(inside && rounded == 1))
            }
            Self::FractionalOdd => (to_fixed(clamped), true),
            Self::FractionalEven => (to_fixed(clamped), false),
        };
        Factor::new(value, odd)
    }
}

fn to_fixed(value: f32) -> u32 {
    // Scaling a clamped factor by a power of two is exact. Round the scaled
    // mantissa to nearest even, as in the reference floatToIDotF conversion.
    (value * ONE as f32).round_ties_even() as u32
}

const RECIPROCAL: [u32; 65] = [
    0xffff_ffff,
    0x10000,
    0x8000,
    0x5555,
    0x4000,
    0x3333,
    0x2aab,
    0x2492,
    0x2000,
    0x1c72,
    0x199a,
    0x1746,
    0x1555,
    0x13b1,
    0x1249,
    0x1111,
    0x1000,
    0xf0f,
    0xe39,
    0xd79,
    0xccd,
    0xc31,
    0xba3,
    0xb21,
    0xaab,
    0xa3d,
    0x9d9,
    0x97b,
    0x925,
    0x8d4,
    0x889,
    0x842,
    0x800,
    0x7c2,
    0x788,
    0x750,
    0x71c,
    0x6eb,
    0x6bd,
    0x690,
    0x666,
    0x63e,
    0x618,
    0x5f4,
    0x5d1,
    0x5b0,
    0x591,
    0x572,
    0x555,
    0x539,
    0x51f,
    0x505,
    0x4ec,
    0x4d5,
    0x4be,
    0x4a8,
    0x492,
    0x47e,
    0x46a,
    0x457,
    0x444,
    0x432,
    0x421,
    0x410,
    0x400,
];

#[derive(Clone, Copy)]
struct Factor {
    fixed: u32,
    odd: bool,
    half_points: u32,
    fraction: u32,
    split: u32,
    floor_reciprocal: u32,
    ceil_reciprocal: u32,
}

impl Factor {
    fn new(fixed: u32, odd: bool) -> Self {
        let mut half = fixed.div_ceil(2);
        if odd || half == HALF {
            half += HALF;
        }
        let floor = half / ONE;
        let ceil = half.div_ceil(ONE);
        let split = if ceil == floor {
            ceil + 1
        } else if odd && floor == 1 {
            0
        } else {
            let n = floor - u32::from(odd);
            let top_bit = 1 << (31 - n.leading_zeros());
            2 * (n ^ top_bit) + 1
        };
        Self {
            fixed,
            odd,
            half_points: ceil,
            fraction: half % ONE,
            split,
            floor_reciprocal: RECIPROCAL[(2 * floor - u32::from(odd)) as usize],
            ceil_reciprocal: RECIPROCAL[(2 * ceil - u32::from(odd)) as usize],
        }
    }

    fn segments(self) -> u32 {
        2 * self.half_points - u32::from(self.odd)
    }

    fn location(self, index: u32) -> u32 {
        let flip = index >= self.half_points;
        let point = if flip { self.segments() - index } else { index };
        if point == self.half_points {
            return HALF;
        }
        let on_floor = (point - u32::from(point > self.split)) * self.floor_reciprocal;
        let on_ceil = point * self.ceil_reciprocal;
        let location = (on_floor * (ONE - self.fraction) + on_ceil * self.fraction + HALF) >> 16;
        if flip { ONE - location } else { location }
    }
}

fn isoline(density: f32, detail: f32, partitioning: Partitioning, output: IsolineOutput) -> Mesh {
    if !(density > 0.0 && detail > 0.0) {
        return Mesh::default();
    }
    let density = Partitioning::Integer.factor(density, 1.0, 64.0, false);
    let (lower, upper) = partitioning.bounds();
    let detail = partitioning.factor(detail, lower, upper, false);
    let lines = density.segments();
    let segments = detail.segments();
    let point_count = lines * (segments + 1);
    let index_count = match output {
        IsolineOutput::Point => point_count,
        IsolineOutput::Line => lines * segments * 2,
    };
    let mut mesh = Mesh::with_capacity(point_count, index_count);
    for line in 0..lines {
        let v = density.location(line);
        for point in 0..=segments {
            mesh.point([detail.location(point), v, 0]);
        }
    }
    match output {
        IsolineOutput::Point => mesh.indices.extend(0..point_count),
        IsolineOutput::Line => {
            for line in 0..lines {
                let start = line * (segments + 1);
                for point in start..start + segments {
                    mesh.indices.extend_from_slice(&[point, point + 1]);
                }
            }
        }
    }
    mesh
}
