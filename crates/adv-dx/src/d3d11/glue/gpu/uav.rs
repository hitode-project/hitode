//! Clear a UAV's raw channel fields and preserve bytes outside the view.

use super::*;
use adv_device::gpu::{DispatchArguments, ModernShader, SlotBindings, StorageBinding};
use adv_gpu::resource::{BufferDesc, Memory};
use std::{ops::Range, sync::Arc};

pub(super) fn clear(
    execution: &mut Execution,
    key: ResourceKey,
    desc: Resource,
    view: &crate::d3d11::desc::View,
    backing: Backing,
    raw: [u32; 4],
    integer: bool,
) -> Result<(), Error> {
    let untyped = matches!(view.format(), 0 | 39);
    let layout = crate::dxgi::formats::format_layout(view.format());
    let unit = if untyped {
        let Resource::Buffer(buffer) = desc else {
            return Err(Error::InvalidArgument);
        };
        if !integer {
            return Err(Error::InvalidArgument);
        }
        buffer.stride().max(4)
    } else {
        u32::from(layout.unwrap().aligned_unit_bytes)
    };
    let mut raw = raw;
    if view.format() == 65 {
        raw[0] = raw[3];
    }
    let pattern = Pattern {
        unit,
        format: view.format(),
        raw,
        integer,
    };
    match (backing, desc) {
        (Backing::Buffer(buffer), Resource::Buffer(_)) => {
            let elements = view.elements();
            let range = elements.start * unit..elements.end * unit;
            if untyped {
                return execution.append(
                    Command::Transfer(ModernTransfer::FillBuffer {
                        destination: BufferRange {
                            buffer,
                            range: u64::from(range.start)..u64::from(range.end),
                        },
                        value: raw[0],
                    }),
                    extent_rect([range.len() as u32, 1, 1]),
                    None,
                    key,
                );
            }
            pattern.fill(execution, key, buffer, range, 0)
        }
        (Backing::Image(image), Resource::Texture(texture)) => {
            let range = view.subresources();
            let level = range.mips.start;
            let [width, height, _] = texture.extent(level);
            let pitch = (width * unit).next_multiple_of(256);
            let length = pitch * height;
            let buffer = execution.gpu.device().create_buffer(&BufferDesc {
                size: u64::from(length),
                usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
                memory: Memory::Device,
                label: "UAV clear slice",
            })?;
            pattern.fill(execution, key, buffer.clone(), 0..length, pitch)?;
            let source = TransferBuffer {
                buffer,
                offset: 0,
                row_pitch: pitch,
                rows: height,
            };
            let rect = extent_rect([width, height, 1]);
            for layer in range.layers.clone() {
                for slice in view.elements() {
                    let destination = pixel_region(
                        &image,
                        texture,
                        layer * texture.levels() + level,
                        slice,
                        rect,
                    );
                    execution.append(
                        Command::Transfer(ModernTransfer::Upload {
                            source: source.clone(),
                            destination,
                        }),
                        rect,
                        None,
                        key,
                    )?;
                }
            }
            Ok(())
        }
        _ => unreachable!(),
    }
}

struct Pattern {
    unit: u32,
    format: u32,
    raw: [u32; 4],
    integer: bool,
}

impl Pattern {
    fn fill(
        &self,
        execution: &mut Execution,
        key: ResourceKey,
        buffer: adv_gpu::Buffer,
        range: Range<u32>,
        pitch: u32,
    ) -> Result<(), Error> {
        let mut source = crate::shader::wgsl::clear_pattern(
            resource::storage_format(self.format)?,
            self.integer,
        )
        .map_err(|_| Error::InvalidArgument)?;
        source.push_str(
            r#"
struct Clear {
    value: vec4<u32>,
    bounds: vec4<u32>,
}
@group(0) @binding(0) var<uniform> clear: Clear;
@group(3) @binding(0) var<storage, read_write> data: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>,
        @builtin(num_workgroups) groups: vec3<u32>) {
    let first = clear.bounds.x;
    let end = clear.bounds.y;
    let word = first / 4u + id.x + id.y * groups.x * 64u;
    if word >= (end - 1u) / 4u + 1u { return; }
    let pattern = clear_pattern(clear.value);
    let pitch = clear.bounds.z;
    let unit = clear.bounds.w;
    let address = word * 4u;
    var value = 0u;
    var mask = 0u;
    for (var i = 0u; i < 4u; i++) {
        let byte = address + i;
        if byte >= first && byte < end {
            let offset = byte - first;
            let in_row = select(offset, offset % max(pitch, 1u), pitch != 0u);
            let lane = in_row % unit;
            let bits = (pattern[lane / 4u] >> (8u * (lane % 4u))) & 255u;
            value |= bits << (8u * i);
            mask |= 255u << (8u * i);
        }
    }
    if mask != 0xffffffffu { value |= data[word] & ~mask; }
    data[word] = value;
}
"#,
        );
        let shader = Arc::new(ModernShader::wgsl(
            xxhash_rust::xxh3::xxh3_64(source.as_bytes()),
            &source,
            "main",
            adv_gpu::shader::Stage::Compute,
        )?);
        let pipeline = Arc::new(execution.gpu.prepare_dispatch(shader)?);
        let params = [self.raw, [range.start, range.end, pitch, self.unit]];
        let uniform = resource::upload(
            &execution.gpu,
            bytemuck::bytes_of(&params),
            BufferUsage::UNIFORM,
            32,
        )?;
        let size = buffer.desc().size;
        let bindings = SlotBindings {
            uniforms: [BufferRange {
                buffer: uniform,
                range: 0..32,
            }
            .into()]
            .into(),
            storage: [StorageBinding::Buffer(BufferRange {
                buffer,
                range: 0..size,
            })]
            .into(),
            ..Default::default()
        };
        let groups = (range.end.div_ceil(4) - range.start / 4).div_ceil(64);
        let width = groups.min(65535);
        let extent = extent_rect([range.len() as u32, 1, 1]);
        execution.append(
            Command::Dispatch(Box::new(crate::d3d11::recording::Compute {
                pipeline,
                bindings,
                arguments: DispatchArguments::Direct([width, groups.div_ceil(width), 1]),
            })),
            extent,
            None,
            key,
        )
    }
}
