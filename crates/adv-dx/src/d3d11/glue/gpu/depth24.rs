//! Pack both depth/stencil aspects into their guest-visible D24S8 layout.

use super::*;
use adv_device::gpu::{DispatchArguments, ImageRegion, ModernShader, SlotBindings, StorageBinding};
use adv_gpu::resource::ViewDesc;
use std::sync::Arc;

pub(super) fn pack(
    gpu: &mut Gpu,
    source: ImageRegion,
    rect: Rect,
    pitch: u32,
    buffer: Buffer,
) -> Result<(), Error> {
    const PACK: &str = r#"
struct Region {
    rect: vec4<u32>,
    stride: vec4<u32>,
}
@group(0) @binding(0) var<uniform> region: Region;
@group(1) @binding(0) var depth: texture_depth_2d;
@group(1) @binding(1) var stencil: texture_2d<u32>;
@group(3) @binding(0) var<storage, read_write> packed: array<u32>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= region.rect.zw) { return; }
    let at = vec2<i32>(id.xy + region.rect.xy);
    let d = u32(round(textureLoad(depth, at, 0) * 16777215.0));
    let s = textureLoad(stencil, at, 0).g;
    packed[id.y * region.stride.x + id.x] = d | (s << 24u);
}
"#;
    let shader = Arc::new(ModernShader::wgsl(
        xxhash_rust::xxh3::xxh3_64(PACK.as_bytes()),
        PACK,
        "main",
        adv_gpu::shader::Stage::Compute,
    )?);
    let pipeline = gpu.prepare_dispatch(shader)?;
    let view = |plane| {
        source.image.create_view(&ViewDesc {
            plane,
            dimension: Some(adv_gpu::resource::Dimension::D2),
            mips: source.mip..source.mip + 1,
            layers: source.layer..source.layer + 1,
            format: None,
        })
    };
    let params = [[rect.x, rect.y, rect.w, rect.h], [pitch / 4, 0, 0, 0]];
    let uniform = resource::upload(gpu, bytemuck::bytes_of(&params), BufferUsage::UNIFORM, 32)?;
    let size = buffer.desc().size;
    gpu.dispatch(
        &pipeline,
        &SlotBindings {
            uniforms: vec![Some(BufferRange {
                buffer: uniform,
                range: 0..32,
            })],
            images: vec![Some(view(2)?), Some(view(1)?)],
            storage: vec![Some(StorageBinding::Buffer(BufferRange {
                buffer,
                range: 0..size,
            }))],
            ..Default::default()
        },
        DispatchArguments::Direct([rect.w.div_ceil(8), rect.h.div_ceil(8), 1]),
    )?;
    Ok(())
}
