//! D3D7 FVF and primitive admission.
use crate::fixed::VertexLayout;
use adv_device::gpu::Topology;

use super::super::INVALID;

pub(in crate::ddraw::d3d) fn layout(fvf: u32) -> Result<VertexLayout, u32> {
    const INVALID_FVF: u32 = 0x88760800;
    let position = fvf & 0xe;
    let textures = (fvf >> 8 & 0xf) as usize;
    if !matches!(position, 2 | 4 | 6 | 8 | 10 | 12 | 14)
        || textures > 8
        || fvf & 0xf001 != 0
        || (position == 4 && fvf & 0x10 != 0)
    {
        return Err(INVALID_FVF);
    }
    let mut size = match position {
        2 => 12,
        4 => 16,
        position => 12 + (position - 4) / 2 * 4,
    };
    let mut field = |present: bool, bytes: u32| {
        present.then(|| {
            let at = size;
            size += bytes;
            at
        })
    };
    let normal = field(fvf & 0x10 != 0, 12).map(|offset| (0, offset));
    field(fvf & 0x20 != 0, 4);
    let color = field(fvf & 0x40 != 0, 4);
    let specular = field(fvf & 0x80 != 0, 4).map(|offset| (0, offset));
    let texcoords = std::array::from_fn(|n| {
        let dimensions = [2, 3, 4, 1][(fvf >> (16 + n * 2) & 3) as usize];
        field(n < textures, dimensions as u32 * 4).map(|offset| (0, offset, dimensions))
    });
    Ok(VertexLayout {
        transformed: position == 4,
        position: 0,
        color,
        normal,
        specular,
        texcoords,
        streams: [0; 2],
        size,
    })
}

pub(in crate::ddraw::d3d) fn primitive(kind: u32, count: usize) -> Result<Topology, u32> {
    Ok(match kind {
        1 => Topology::Points,
        2 if count.is_multiple_of(2) => Topology::Lines,
        3 if count >= 2 => Topology::LineStrip,
        4 if count.is_multiple_of(3) => Topology::Triangles,
        5 if count >= 3 => Topology::TriangleStrip,
        6 if count >= 3 => Topology::Triangles,
        _ => return Err(INVALID),
    })
}

pub(in crate::ddraw::d3d) fn expand_fan(indices: impl Fn(u32) -> u32, count: u32) -> Vec<u32> {
    (1..count - 1)
        .flat_map(|i| [indices(0), indices(i), indices(i + 1)])
        .collect()
}

pub(in crate::ddraw::d3d) fn legacy_fvf(kind: u32) -> Option<u32> {
    match kind {
        1 => Some(0x112), // D3DVERTEX
        2 => Some(0x1e2), // D3DLVERTEX
        3 => Some(0x1c4), // D3DTLVERTEX
        _ => None,
    }
}
