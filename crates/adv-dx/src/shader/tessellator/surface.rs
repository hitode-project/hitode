//! Triangle and quad rings and the strips joining their sides.
//! Port of CHWTessellator under the Microsoft MIT notice in tessellator.rs.

use super::{EPSILON, Factor, HALF, Mesh, ONE, Partitioning, SurfaceOutput};

const ONE_THIRD: u32 = 0x5555;
const TWO_THIRDS: u32 = 0xaaaa;

fn factors<const E: usize, const I: usize>(
    edges: [f32; E],
    inside: [f32; I],
    partitioning: Partitioning,
) -> Option<([Factor; E], [Factor; I])> {
    // The ordered comparison also culls NaN.
    if !edges.iter().all(|&edge| edge > 0.0) {
        return None;
    }
    let (lower, upper) = partitioning.bounds();
    let outside = edges.map(|edge| partitioning.factor(edge, lower, upper, false));
    let threshold = 1.0 + EPSILON / 2.0;
    // If any factor rounds above one, a fractional-odd patch needs an interior
    // ring even when an inside factor would otherwise clamp to one.
    let inside_lower = if matches!(partitioning, Partitioning::FractionalOdd)
        && edges
            .iter()
            .chain(&inside)
            .any(|&factor| factor > threshold)
    {
        1.0 + EPSILON
    } else {
        lower
    };
    let inside = inside.map(|factor| partitioning.factor(factor, inside_lower, upper, true));
    Some((outside, inside))
}

pub(super) fn tri(
    edges: [f32; 3],
    inside: f32,
    partitioning: Partitioning,
    output: SurfaceOutput,
) -> Mesh {
    let Some((outside, [inside])) = factors(edges, [inside], partitioning) else {
        return Mesh::default();
    };
    if inside.fixed == ONE && outside.iter().all(|factor| factor.fixed == ONE) {
        let mut surface = Surface::new(3, 3, output);
        for point in [[0, ONE, 0], [0, 0, ONE], [ONE, 0, 0]] {
            surface.mesh.point(point);
        }
        if surface.has_triangles() {
            surface.triangle([0, 1, 2]);
        }
        return surface.finish();
    }

    let segments = outside.map(Factor::segments);
    let boundary = segments.iter().sum::<u32>();
    let inside_segments = inside.segments();
    let rings = inside_segments.div_ceil(2) - 1;
    let interior = if inside.odd {
        3 * rings * rings
    } else {
        3 * rings * (rings + 1) + 1
    };
    let mut surface = Surface::new(boundary + interior, boundary, output);
    let mut previous = ring_sides(0, segments);
    for (edge, factor) in outside.into_iter().enumerate() {
        for p in 0..factor.segments() {
            let location = factor.location(if edge == 1 { p } else { factor.segments() - p });
            let (u, v) = match edge {
                0 => (0, location),
                1 => (location, 0),
                _ => (location, ONE - location),
            };
            surface.mesh.point([u, v, ONE - u - v]);
        }
    }

    // The even center is a ring with zero segments on each of its three sides.
    for ring in 1..=inside_segments / 2 {
        let side_segments = inside_segments - 2 * ring;
        let current = ring_sides(surface.mesh.points.len() as u32, [side_segments; 3]);
        if side_segments == 0 {
            surface
                .mesh
                .point([ONE_THIRD, ONE_THIRD, ONE - 2 * ONE_THIRD]);
        } else {
            // Inset the ring in barycentric space by two thirds of the 1D distance.
            let perpendicular = (inside.location(ring) * TWO_THIRDS + HALF) >> 16;
            for edge in 0..3 {
                for p in ring..inside_segments - ring {
                    let parallel = inside.location(if edge == 1 { p } else { inside_segments - p })
                        - perpendicular.div_ceil(2);
                    let (u, v) = match edge {
                        0 => (perpendicular, parallel),
                        1 => (parallel, perpendicular),
                        _ => (parallel, ONE - parallel - perpendicular),
                    };
                    surface.mesh.point([u, v, ONE - u - v]);
                }
            }
        }
        if surface.has_triangles() {
            surface.stitch_ring(current, previous, [inside; 3], outside, ring == 1);
        }
        previous = current;
    }
    if inside.odd && surface.has_triangles() {
        surface.triangle(previous.map(|side| side.index(0)));
    }
    surface.finish()
}

pub(super) fn quad(
    edges: [f32; 4],
    inside: [f32; 2],
    partitioning: Partitioning,
    output: SurfaceOutput,
) -> Mesh {
    let Some((outside, inside)) = factors(edges, inside, partitioning) else {
        return Mesh::default();
    };
    if inside
        .into_iter()
        .chain(outside)
        .all(|factor| factor.fixed == ONE)
    {
        let mut surface = Surface::new(4, 4, output);
        for point in [[0, 0, 0], [ONE, 0, 0], [ONE, ONE, 0], [0, ONE, 0]] {
            surface.mesh.point(point);
        }
        if surface.has_triangles() {
            surface.triangle([0, 1, 3]);
            surface.triangle([1, 2, 3]);
        }
        return surface.finish();
    }

    let segments = outside.map(Factor::segments);
    let [u, v] = inside.map(Factor::segments);
    let boundary = segments.iter().sum::<u32>();
    let mut surface = Surface::new(boundary + (u - 1) * (v - 1), boundary, output);
    let mut previous = ring_sides(0, segments);
    for (edge, factor) in outside.into_iter().enumerate() {
        for p in 0..factor.segments() {
            let location = factor.location(if edge == 1 || edge == 2 {
                p
            } else {
                factor.segments() - p
            });
            surface.mesh.point(match edge {
                0 => [0, location, 0],
                1 => [location, 0, 0],
                2 => [ONE, location, 0],
                _ => [location, ONE, 0],
            });
        }
    }

    let inside_sides = [inside[1], inside[0], inside[1], inside[0]];
    for ring in 1..=u.min(v) / 2 {
        let base = surface.mesh.points.len() as u32;
        let width = u - 2 * ring;
        let height = v - 2 * ring;
        // A collapsed inner ring shares a single row between two opposite sides.
        let current = if height == 0 {
            for p in ring..=u - ring {
                surface.mesh.point([inside[0].location(p), HALF, 0]);
            }
            let row = Side::straight(base, width);
            [
                Side::straight(base, 0),
                row,
                Side::straight(base + width, 0),
                row.reverse(),
            ]
        } else if width == 0 {
            for p in (ring..=v - ring).rev() {
                surface.mesh.point([HALF, inside[1].location(p), 0]);
            }
            let row = Side::straight(base, height);
            [
                row,
                Side::straight(base + height, 0),
                row.reverse(),
                Side::straight(base, 0),
            ]
        } else {
            for (edge, factor) in inside_sides.into_iter().enumerate() {
                let other = inside[edge % 2];
                let perpendicular = other.location(if edge < 2 {
                    ring
                } else {
                    other.segments() - ring
                });
                for p in ring..factor.segments() - ring {
                    let parallel = factor.location(if edge == 1 || edge == 2 {
                        p
                    } else {
                        factor.segments() - p
                    });
                    surface.mesh.point(if edge & 1 == 0 {
                        [perpendicular, parallel, 0]
                    } else {
                        [parallel, perpendicular, 0]
                    });
                }
            }
            ring_sides(base, [height, width, height, width])
        };
        if surface.has_triangles() {
            surface.stitch_ring(current, previous, inside_sides, outside, ring == 1);
        }
        previous = current;
    }

    if surface.has_triangles() {
        // An odd innermost ring leaves a strip of quads. The middle diagonal
        // reverses on a V-aligned strip when both axes are odd.
        if u > v && inside[1].odd {
            surface.regular(
                previous[3].reverse(),
                previous[1],
                false,
                Diagonals::Forward,
            );
        } else if v >= u && inside[0].odd {
            let diagonals = if inside[1].odd {
                Diagonals::ReverseMiddle
            } else {
                Diagonals::Forward
            };
            surface.regular(previous[2].reverse(), previous[0], false, diagonals);
        }
    }
    surface.finish()
}

/// A view of one side of a ring. The endpoint can wrap to its first vertex.
#[derive(Clone, Copy)]
struct Side {
    start: u32,
    end: u32,
    segments: u32,
    reversed: bool,
}

impl Side {
    fn straight(start: u32, segments: u32) -> Self {
        Self {
            start,
            end: start + segments,
            segments,
            reversed: false,
        }
    }

    fn reverse(self) -> Self {
        Self {
            reversed: !self.reversed,
            ..self
        }
    }

    fn index(self, point: u32) -> u32 {
        let point = if self.reversed {
            self.segments - point
        } else {
            point
        };
        if point == self.segments {
            self.end
        } else {
            self.start + point
        }
    }
}

fn ring_sides<const N: usize>(base: u32, segments: [u32; N]) -> [Side; N] {
    let mut start = base;
    let mut sides = segments.map(|segments| {
        let side = Side::straight(start, segments);
        start = side.end;
        side
    });
    sides[N - 1].end = base;
    sides
}

enum Diagonals {
    Forward,
    ReverseMiddle,
    Mirrored,
}

struct Surface {
    mesh: Mesh,
    output: SurfaceOutput,
}

impl Surface {
    fn new(points: u32, boundary: u32, output: SurfaceOutput) -> Self {
        let indices = match output {
            SurfaceOutput::Point => points,
            // A triangulated disk has 2V - B - 2 triangles for B boundary vertices.
            _ => 3 * (2 * points - boundary - 2),
        };
        Self {
            mesh: Mesh::with_capacity(points, indices),
            output,
        }
    }

    fn has_triangles(&self) -> bool {
        self.output != SurfaceOutput::Point
    }

    fn triangle(&mut self, [a, b, c]: [u32; 3]) {
        let indices = if self.output == SurfaceOutput::TriangleCcw {
            [a, c, b]
        } else {
            [a, b, c]
        };
        self.mesh.indices.extend_from_slice(&indices);
    }

    fn finish(mut self) -> Mesh {
        if self.output == SurfaceOutput::Point {
            self.mesh.indices.extend(0..self.mesh.points.len() as u32);
        }
        self.mesh
    }

    fn stitch_ring<const N: usize>(
        &mut self,
        inside: [Side; N],
        outside: [Side; N],
        inside_factors: [Factor; N],
        outside_factors: [Factor; N],
        transition: bool,
    ) {
        for edge in 0..N {
            if transition {
                self.transition(
                    inside[edge],
                    outside[edge],
                    inside_factors[edge],
                    outside_factors[edge],
                );
            } else {
                self.regular(inside[edge], outside[edge], true, Diagonals::Mirrored);
            }
        }
    }

    fn regular(&mut self, inside: Side, outside: Side, trapezoid: bool, diagonals: Diagonals) {
        let offset = u32::from(trapezoid);
        if trapezoid {
            self.triangle([outside.index(0), outside.index(1), inside.index(0)]);
        }
        for p in 0..inside.segments {
            let i0 = inside.index(p);
            let i1 = inside.index(p + 1);
            let o0 = outside.index(p + offset);
            let o1 = outside.index(p + offset + 1);
            let reverse = match diagonals {
                Diagonals::Forward => false,
                Diagonals::ReverseMiddle => p == inside.segments / 2,
                Diagonals::Mirrored => p < inside.segments.div_ceil(2),
            };
            if reverse {
                self.triangle([o0, i1, i0]);
                self.triangle([o0, o1, i1]);
            } else {
                self.triangle([i0, o0, o1]);
                self.triangle([i0, o1, i1]);
            }
        }
        if trapezoid {
            let end = outside.segments;
            self.triangle([
                outside.index(end - 1),
                outside.index(end),
                inside.index(inside.segments),
            ]);
        }
    }

    fn transition(&mut self, inside: Side, outside: Side, inner: Factor, outer: Factor) {
        // Positions in the maximally split half-edge, in ruler-function order.
        const POSITIONS: [u32; 33] = [
            0, 32, 16, 8, 17, 4, 18, 9, 19, 2, 20, 10, 21, 5, 22, 11, 23, 1, 24, 12, 25, 6, 26, 13,
            27, 3, 28, 14, 29, 7, 30, 15, 31,
        ];
        // First and last relevant positions after the special case for 0.
        const START: [usize; 33] = [
            1, 1, 17, 9, 9, 5, 5, 5, 5, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2,
        ];
        const END: [usize; 33] = [
            0, 0, 17, 17, 25, 25, 25, 25, 29, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31, 31,
            31, 31, 31, 31, 31, 31, 31, 31, 31, 31, 32,
        ];
        let inner_half = inner.half_points - u32::from(inner.odd);
        let outer_half = outer.half_points - u32::from(outer.odd);
        let start = START[inner_half as usize].min(START[outer_half as usize]);
        let end = END[inner_half as usize].max(END[outer_half as usize]);
        let (mut i, mut o) = (0, 0);
        if outer_half != 0 {
            self.triangle([outside.index(o), outside.index(o + 1), inside.index(i)]);
            o += 1;
        }
        let positions = &POSITIONS[start..end + 1];
        for &position in positions {
            if position < inner_half {
                self.triangle([inside.index(i), outside.index(o), inside.index(i + 1)]);
                i += 1;
            }
            if position < outer_half {
                self.triangle([outside.index(o), outside.index(o + 1), inside.index(i)]);
                o += 1;
            }
        }

        // Match parities at the midpoint, with a quad when both sides are odd.
        if inner.odd {
            self.triangle([inside.index(i), outside.index(o), inside.index(i + 1)]);
            i += 1;
        }
        if outer.odd {
            self.triangle([inside.index(i), outside.index(o), outside.index(o + 1)]);
            o += 1;
        }

        for &position in positions.iter().rev() {
            if position < outer_half {
                self.triangle([outside.index(o), outside.index(o + 1), inside.index(i)]);
                o += 1;
            }
            if position < inner_half {
                self.triangle([inside.index(i), outside.index(o), inside.index(i + 1)]);
                i += 1;
            }
        }
        if outer_half != 0 {
            self.triangle([outside.index(o), outside.index(o + 1), inside.index(i)]);
        }
    }
}
