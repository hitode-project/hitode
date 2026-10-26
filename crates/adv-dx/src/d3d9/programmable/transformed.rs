//! POSITIONT keeps captured SDK bytes and indices until GPU execution.
use super::*;
use adv_device::gpu::Viewport;

pub(super) fn inputs(layout: fixed::Layout) -> Vec<Input> {
    let mut inputs = vec![Input {
        stream: layout.streams[0] as usize,
        offset: layout.position,
        format: AttributeFormat::Float4,
        register: 0,
    }];
    if let Some(offset) = layout.color {
        inputs.push(Input {
            stream: layout.streams[1] as usize,
            offset,
            format: AttributeFormat::Bgra8Unorm,
            register: 1,
        });
    }
    for (n, coordinate) in layout.texcoords.iter().enumerate() {
        if let Some((stream, offset, dimensions)) = *coordinate {
            inputs.push(Input {
                stream: stream as usize,
                offset,
                format: match dimensions {
                    1 => AttributeFormat::Float1,
                    2 => AttributeFormat::Float2,
                    3 => AttributeFormat::Float3,
                    _ => AttributeFormat::Float4,
                },
                register: 2 + n as u32,
            });
        }
    }
    inputs
}
pub(in crate::d3d9) fn position(raw: [f32; 4], viewport: Viewport) -> Option<[f32; 4]> {
    if raw[3] == 0. || !raw[3].is_finite() {
        return None;
    }
    let w = 1. / raw[3];
    let position = [
        (((raw[0] + 0.5 - viewport.x) / viewport.width) * 2. - 1.) * w,
        (1. - ((raw[1] + 0.5 - viewport.y) / viewport.height) * 2.) * w,
        raw[2] * w,
        w,
    ];
    position.iter().all(|v| v.is_finite()).then_some(position)
}
pub(super) fn validate(
    streams: &[VertexStream],
    viewport: Viewport,
    indices: Option<&[u32]>,
    count: u32,
) -> Result<(), u32> {
    for ordinal in 0..indices.map_or(count as usize, <[u32]>::len) {
        let n = indices.map_or(ordinal as u32, |v| v[ordinal]);
        let mut position_value = None;
        for stream in streams {
            for attribute in &stream.attributes {
                if attribute.format == AttributeFormat::Bgra8Unorm {
                    continue;
                }
                let mut bytes = [0u8; 16];
                let bytes = &mut bytes[..attribute.format.size() as usize];
                stream
                    .bytes
                    .read_vertex(
                        n as usize,
                        stream.stride as usize,
                        attribute.offset as usize,
                        bytes,
                    )
                    .ok_or(INVALID)?;
                if bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|v| !f32::from_le_bytes(*v).is_finite())
                {
                    return Err(INVALID);
                }
                if attribute.location == 0 {
                    position_value = Some(std::array::from_fn(|i| {
                        f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                    }));
                }
            }
        }
        position(position_value.ok_or(INVALID)?, viewport).ok_or(INVALID)?;
    }
    Ok(())
}
