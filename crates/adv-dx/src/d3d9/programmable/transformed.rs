//! POSITIONT keeps captured SDK bytes and indices until GPU execution.
use super::*;
use adv_device::gpu::Viewport;

pub(super) fn inputs(layout: fixed::Layout) -> Vec<Input> {
    let mut inputs = vec![Input {
        stream: layout.streams[0] as usize,
        offset: layout.position,
        format: if layout.transformed {
            AttributeFormat::Float4
        } else {
            AttributeFormat::Float3
        },
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
