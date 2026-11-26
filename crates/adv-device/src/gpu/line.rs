//! Inclusive aliased endpoints over half-open GPU lines, preserving draw order.
//! Integer 2D vertices match the authored D3D9 reference-rasterizer oracle.
use super::*;

// Each inclusive list segment encodes one line and at most one point command.
// Keep command growth bounded independently of the shared vertex-upload limit.
const MAX_INCLUSIVE_LIST_SEGMENTS: usize = 4096;
const INTEGER_TOLERANCE: f32 = 1. / 4096.;

pub enum Inclusive {
    List,
    Strip { terminal: bool },
}

impl Inclusive {
    /// Native line ranges interleaved with their terminal points. A range of
    /// length one uses point topology and the original vertex input.
    pub fn ranges(
        topology: Topology,
        count: u32,
        same_pixel: impl Fn(u32, u32) -> bool,
    ) -> Vec<std::ops::Range<u32>> {
        let inclusive = match topology {
            Topology::Lines => Self::List,
            Topology::LineStrip => Self::Strip {
                terminal: !same_pixel(count - 2, count - 1),
            },
            _ => return Vec::new(),
        };
        let mut ranges = Vec::new();
        match inclusive {
            Self::List => {
                for end in (1..count).step_by(2) {
                    ranges.push(end - 1..end + 1);
                    if !same_pixel(end - 1, end) {
                        ranges.push(end..end + 1);
                    }
                }
            }
            Self::Strip { terminal } => {
                ranges.push(0..count);
                if terminal {
                    ranges.push(count - 1..count);
                }
            }
        }
        ranges
    }
}

pub(super) fn prepare(
    vertices: &[Vertex],
    image: Option<&Image>,
    state: &DrawState,
) -> Result<Option<Inclusive>, Error> {
    if !state.last_pixel || !matches!(state.topology, Topology::Lines | Topology::LineStrip) {
        return Ok(None);
    }
    if image.is_some_and(|image| image.mip_levels() > 1) && state.sampler.mip.is_some() {
        return Err(Error::Unavailable(
            "inclusive line endpoints do not support automatic mip-level selection".into(),
        ));
    }
    let viewport = state.viewport;
    for vertex in vertices {
        if vertex.position[3] != 1. || !(0. ..=1.).contains(&vertex.position[2]) {
            return Err(Error::Unavailable(
                "inclusive lines require 2D vertices with W=1 and depth in 0..1".into(),
            ));
        }
        let x = viewport.x + (vertex.position[0] + 1.) * viewport.width * 0.5 - 0.5;
        let y = viewport.y + (1. - vertex.position[1]) * viewport.height * 0.5 - 0.5;
        if !x.is_finite()
            || !y.is_finite()
            || (x - x.round()).abs() > INTEGER_TOLERANCE
            || (y - y.round()).abs() > INTEGER_TOLERANCE
        {
            return Err(Error::Unavailable(
                "inclusive lines require integer pixel coordinates (subpixel endpoints are unavailable)".into(),
            ));
        }
    }
    match state.topology {
        Topology::Lines => {
            if vertices.len() / 2 > MAX_INCLUSIVE_LIST_SEGMENTS {
                return Err(Error::Unavailable(
                    "inclusive line lists exceed the 4096-segment command limit".into(),
                ));
            }
            Ok(Some(Inclusive::List))
        }
        Topology::LineStrip => {
            let last = vertices.len() - 1;
            // REF suppresses the terminal when the final segment is degenerate,
            // even if an earlier nondegenerate segment ends at the same pixel.
            Ok(Some(Inclusive::Strip {
                terminal: !degenerate(&vertices[last - 1], &vertices[last], viewport),
            }))
        }
        _ => unreachable!(),
    }
}

pub(super) fn degenerate(start: &Vertex, end: &Vertex, viewport: Viewport) -> bool {
    let pixel = |vertex: &Vertex| {
        [
            (viewport.x + (vertex.position[0] + 1.) * viewport.width * 0.5 - 0.5).round(),
            (viewport.y + (1. - vertex.position[1]) * viewport.height * 0.5 - 0.5).round(),
        ]
    };
    pixel(start) == pixel(end)
}
