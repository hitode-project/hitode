//! Assemble shader state into a neutral GPU command. Guest memory is validated
//! once; raw stream bytes are retained by the command across deferred submits.
use super::*;
use crate::shader::{SamplerDimension, Shader, Translation};
use adv_device::gpu::{
    Attribute, AttributeFormat, ConstantBlock, Program, ProgramTexture, ProgrammableDraw,
    VertexStream,
};
mod source;
pub(super) mod warmup;
pub(super) use warmup::warm;
#[cfg(test)]
mod tests;
// Generational shader identities cover immutable declarations and sampler slots.
// Only input registers and formats affect the assembled entry points.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ProgramKey {
    vertex: Option<Id>,
    pixel: Option<Id>,
    inputs: Vec<(u32, AttributeFormat)>,
}
#[derive(Default)]
pub(super) struct ProgramCache {
    entries: std::collections::HashMap<ProgramKey, Program>,
    bytes: usize,
    warming: std::collections::HashMap<ProgramKey, adv_device::gpu::compile::Compile<Program>>,
    warming_bytes: usize,
    pub counters: adv_device::gpu::compile::CompileCounters,
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(set_clip_plane_ABI);
    registry.register(get_clip_plane_ABI);
}
#[win32(d3d9,name="IDirect3DDevice9::SetClipPlane",level=state)]
fn set_clip_plane(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, plane: GuestPtr<f32>) -> u32 {
    if index >= 6 || device(env, this.addr()).is_none() {
        return INVALID;
    }
    let Ok(values) = GuestPtr::<[f32; 4]>::new(plane.addr()).read(env.cpu) else {
        return INVALID;
    };
    if values.iter().any(|v| !v.is_finite()) {
        return INVALID;
    }
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return INVALID;
    };
    if let Some(record) = &mut d.recording {
        record.fixed.clip_planes[index as usize] = values;
    } else {
        d.fixed.clip_planes[index as usize] = values;
    }
    shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        shaders::stateblocks::FixedKey::ClipPlane(index as usize),
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetClipPlane",level=implement)]
fn get_clip_plane(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, plane: GuestPtr<f32>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(values) = d.fixed.clip_planes.get(index as usize).copied() else {
        return INVALID;
    };
    GuestPtr::<[f32; 4]>::new(plane.addr())
        .write(env.cpu, values)
        .map_or(INVALID, |()| 0)
}

pub(super) fn active(env: &Env<'_>, pointer: u32) -> bool {
    device(env, pointer).is_some_and(|d| d.shaders.vertex.is_some() || d.shaders.pixel.is_some())
}
fn format(kind: u8) -> Result<AttributeFormat, u32> {
    use AttributeFormat::*;
    Ok(match kind {
        0 => Float1,
        1 => Float2,
        2 => Float3,
        3 => Float4,
        4 => Bgra8Unorm,
        5 => Uint8x4,
        6 => Sint16x2,
        7 => Sint16x4,
        8 => Unorm8x4,
        9 => Snorm16x2,
        10 => Snorm16x4,
        11 => Unorm16x2,
        12 => Unorm16x4,
        13 => Uint10x3,
        14 => Snorm10x3,
        15 => Float16x2,
        16 => Float16x4,
        _ => return Err(INVALID),
    })
}
fn constants(c: &shaders::Constants, usage: adv_device::constants::ConstantUsage) -> ConstantBlock {
    let mut block = ConstantBlock::default();
    for n in usage.ranges().flat_map(|r| (r.start / 16)..(r.end / 16)) {
        match n {
            0..256 => block.floats[n] = c.floats[n],
            256..272 => block.integers[n - 256] = c.integers[n - 256],
            272..288 => block.booleans[n - 272][0] = c.booleans[n - 272],
            _ => {}
        }
    }
    block
}
#[derive(Clone)]
struct Input {
    stream: usize,
    offset: u32,
    format: AttributeFormat,
    register: u32,
}
fn inputs(env: &Env<'_>, pointer: u32, vs: &Shader) -> Result<Vec<Input>, u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    let elements = if let Some(id) = d.buffers.declaration {
        let Some(Payload::Declaration(decl)) = com(env).get_identity(&env.sys.objects, id) else {
            return Err(INVALID);
        };
        decl.elements.clone()
    } else {
        let layout = fixed::Layout::from_fvf(d.fixed.fvf).ok_or(UNAVAILABLE)?;
        if layout.transformed {
            return Err(INVALID);
        }
        let mut v = vec![[
            0,
            0,
            layout.position as u8,
            (layout.position >> 8) as u8,
            2,
            0,
            0,
            0,
        ]];
        if let Some(o) = layout.color {
            v.push([0, 0, o as u8, (o >> 8) as u8, 4, 0, 10, 0]);
        }
        for (n, coordinate) in layout.texcoords.iter().enumerate() {
            if let Some((_, o, dimensions)) = *coordinate {
                v.push([0, 0, o as u8, (o >> 8) as u8, dimensions - 1, 0, 5, n as u8]);
            }
        }
        v.push([255, 0, 0, 0, 17, 0, 0, 0]);
        v
    };
    declaration_inputs(&elements, vs)
}
fn declaration_inputs(elements: &[[u8; 8]], vs: &Shader) -> Result<Vec<Input>, u32> {
    let mut result = Vec::new();
    for e in elements.iter().take_while(|e| e[0] != 255) {
        if e[5] != 0 {
            return Err(UNAVAILABLE);
        }
        if e[6] == 9 {
            return Err(INVALID);
        }
        let register = vs
            .inputs
            .iter()
            .find(|i| {
                i.register.kind == crate::shader::RegisterKind::Input
                    && i.usage == e[6]
                    && i.index == e[7]
            })
            .map(|i| u32::from(i.register.index));
        if let Some(register) = register {
            if register >= 16 || result.iter().any(|i: &Input| i.register == register) {
                return Err(INVALID);
            }
            result.push(Input {
                stream: u16::from_le_bytes([e[0], e[1]]) as usize,
                offset: u32::from(u16::from_le_bytes([e[2], e[3]])),
                format: format(e[4])?,
                register,
            });
        }
    }
    if vs
        .inputs
        .iter()
        .filter(|d| d.register.kind == crate::shader::RegisterKind::Input)
        .any(|decl| {
            !result
                .iter()
                .any(|i| i.register == u32::from(decl.register.index))
        })
    {
        return Err(INVALID);
    }
    Ok(result)
}
struct StageProgram {
    id: Id,
    parsed: std::sync::Arc<Shader>,
    translation: std::sync::Arc<Translation>,
}
fn stage(env: &Env<'_>, id: Option<Id>) -> Result<Option<StageProgram>, u32> {
    id.map(|id| {
        let s = shaders::bound(env, id).ok_or(INVALID)?;
        Ok(StageProgram {
            id,
            parsed: s.parsed.clone(),
            translation: s.translation.as_ref().map_err(|_| UNAVAILABLE)?.clone(),
        })
    })
    .transpose()
}
pub(super) fn sampler_slot(sampler: u32) -> Option<usize> {
    match sampler {
        0..=15 => Some(sampler as usize),
        257..=260 => Some(sampler as usize - 241),
        _ => None,
    }
}
fn pretransformed(env: &Env<'_>, pointer: u32) -> Result<bool, u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    if let Some(id) = d.buffers.declaration {
        let Some(Payload::Declaration(declaration)) = com(env).get_identity(&env.sys.objects, id)
        else {
            return Err(INVALID);
        };
        Ok(declaration
            .elements
            .iter()
            .take_while(|e| e[0] != 255)
            .any(|e| e[6] == 9 && e[7] == 0))
    } else {
        Ok(d.fixed.fvf & 0x0e == 4)
    }
}
pub(super) fn prepare(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    up: Option<(GuestPtr<u8>, u32)>,
    indices: Option<&[u32]>,
    start: u32,
) -> Result<fixed::Draw, u32> {
    let result = prepare_checked(env, pointer, kind, count, up, indices, start);
    if let Err(code) = result
        && code == UNAVAILABLE
    {
        let detail = device(env, pointer)
            .map(|d| {
                [d.shaders.vertex, d.shaders.pixel]
                    .into_iter()
                    .flatten()
                    .filter_map(|id| shaders::bound(env, id))
                    .filter_map(|s| s.translation.as_ref().err().cloned())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::DrawPrimitive",
            &format!("programmable shader/declaration unavailable: {detail}"),
        );
    }
    result
}
fn prepare_checked(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    up: Option<(GuestPtr<u8>, u32)>,
    indices: Option<&[u32]>,
    start: u32,
) -> Result<fixed::Draw, u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    if !d.scene || d.needs_reset {
        return Err(INVALID);
    }
    // POSITIONT/FVF XYZRHW bypasses vertex processing even while a vertex
    // shader remains bound. Getter/state-block identity remains unchanged.
    let vs = stage(
        env,
        if pretransformed(env, pointer)? {
            None
        } else {
            d.shaders.vertex
        },
    )?;
    let ps = stage(env, d.shaders.pixel)?;
    let vt = vs.as_ref().map(|s| s.translation.as_ref());
    let pt = ps.as_ref().map(|s| s.translation.as_ref());
    let length = fixed::primitive_length(kind, count)?;
    if indices.is_some_and(|i| i.len() != length as usize) {
        return Err(INVALID);
    }
    let mut vertex_count;
    let mut output_indices = indices.map(<[u32]>::to_vec);
    let (mut draw, mut streams, input_spec) = if let Some(vs) = &vs {
        let inputs = inputs(env, pointer, &vs.parsed)?;
        let first = indices.and_then(|i| i.iter().min().copied()).unwrap_or(0);
        let last = indices
            .and_then(|i| i.iter().max().copied())
            .unwrap_or(length - 1);
        vertex_count = last
            .checked_sub(first)
            .and_then(|v| v.checked_add(1))
            .ok_or(INVALID)?;
        if let Some(i) = &mut output_indices {
            for i in i {
                *i -= first;
            }
        }
        let mut streams = Vec::new();
        let mut total = 0u32;
        for n in 0..16 {
            let selected: Vec<_> = inputs.iter().filter(|i| i.stream == n).collect();
            if selected.is_empty() {
                continue;
            }
            let minimum = selected
                .iter()
                .map(|i| i.offset + i.format.size())
                .max()
                .unwrap();
            let (data, stride, available) = if let Some((data, stride)) = up {
                if n != 0 {
                    return Err(INVALID);
                }
                (data.addr(), stride, u32::MAX)
            } else {
                let stream = d.buffers.streams[n];
                let Some(Payload::Buffer(buffer)) = stream
                    .buffer
                    .and_then(|id| com(env).get_identity(&env.sys.objects, id))
                else {
                    return Err(INVALID);
                };
                if buffer.locks != 0 || stream.offset > buffer.len {
                    return Err(INVALID);
                }
                (
                    buffer.storage.checked_add(stream.offset).ok_or(INVALID)?,
                    stream.stride,
                    buffer.len - stream.offset,
                )
            };
            if stride > 2048 || !stride.is_multiple_of(4) || (stride != 0 && stride < minimum) {
                return Err(INVALID);
            }
            let offset = start
                .checked_add(first)
                .and_then(|v| v.checked_mul(stride))
                .ok_or(INVALID)?;
            let bytes = (last - first)
                .checked_mul(stride)
                .and_then(|v| v.checked_add(minimum))
                .ok_or(INVALID)?;
            if offset.checked_add(bytes).is_none_or(|end| end > available) {
                return Err(INVALID);
            }
            total = total
                .checked_add(bytes)
                .filter(|&v| v <= 16 * 1024 * 1024)
                .ok_or(INVALID)?;
            let at = data.checked_add(offset).ok_or(INVALID)?;
            adv_abi::guest::check_range(env.cpu, at, bytes, Prot::READ).map_err(|_| INVALID)?;
            let mut raw = vec![0; bytes as usize];
            env.cpu.read(at, &mut raw).map_err(|_| INVALID)?;
            streams.push(VertexStream {
                bytes: raw,
                stride,
                attributes: selected
                    .iter()
                    .map(|i| Attribute {
                        location: i.register,
                        offset: i.offset,
                        format: i.format,
                    })
                    .collect(),
            });
        }
        let (target, image, state) =
            fixed::prepare_state(env, pointer, kind).map_err(|e| e.code)?;
        (
            fixed::Draw {
                target,
                image,
                state,
                vertices: Vec::new(),
                texcoords: None,
                programmable: None,
                recording_key: recording::draw_key(env, pointer),
                program_key: 0,
            },
            streams,
            inputs,
        )
    } else {
        // The shared fixed decoder validates all declaration streams and expands
        // indices/fans before assembling the neutral programmable stream.
        let draw = if let Some((data, stride)) = up {
            fixed::prepare(env, pointer, kind, count, data, stride, indices)?
        } else {
            fixed::prepare_bound(env, pointer, kind, count, start, indices)?
        };
        vertex_count = draw.vertices.len() as u32;
        output_indices = None;
        let mut stream = VertexStream {
            bytes: bytemuck::cast_slice(&draw.vertices).to_vec(),
            stride: 40,
            attributes: vec![
                Attribute {
                    location: 0,
                    offset: 0,
                    format: AttributeFormat::Float4,
                },
                Attribute {
                    location: 1,
                    offset: 16,
                    format: AttributeFormat::Float4,
                },
                Attribute {
                    location: 2,
                    offset: 32,
                    format: AttributeFormat::Float2,
                },
            ],
        };
        let mut inputs = Vec::new();
        if let Some((count, values)) = &draw.texcoords {
            stream.stride = 32 + *count as u32 * 16;
            stream.bytes.clear();
            stream
                .bytes
                .reserve(draw.vertices.len() * stream.stride as usize);
            stream.attributes.truncate(2);
            for (vertex, coordinates) in draw.vertices.iter().zip(values.chunks_exact(*count)) {
                stream
                    .bytes
                    .extend_from_slice(bytemuck::cast_slice(&vertex.position));
                stream
                    .bytes
                    .extend_from_slice(bytemuck::cast_slice(&vertex.color));
                stream
                    .bytes
                    .extend_from_slice(bytemuck::cast_slice(coordinates));
            }
            for n in 0..*count {
                let location = 2 + n as u32;
                let offset = 32 + n as u32 * 16;
                stream.attributes.push(Attribute {
                    location,
                    offset,
                    format: AttributeFormat::Float4,
                });
                inputs.push(Input {
                    stream: 0,
                    offset,
                    format: AttributeFormat::Float4,
                    register: location,
                });
            }
        }
        (draw, vec![stream], inputs)
    };
    if vs.is_some() && kind == 6 {
        let original = output_indices
            .take()
            .unwrap_or_else(|| (0..length).collect());
        let mut fan = Vec::with_capacity(count as usize * 3);
        for n in 1..original.len() - 1 {
            fan.extend_from_slice(&[original[0], original[n], original[n + 1]]);
        }
        output_indices = Some(fan);
    }
    if kind == 1 {
        let mut budget = 0usize;
        for stream in &mut streams {
            let width = stream
                .attributes
                .iter()
                .map(|a| a.offset + a.format.size())
                .max()
                .unwrap_or(0) as usize;
            let stride = if stream.stride == 0 {
                width
            } else {
                stream.stride as usize
            };
            let size = (vertex_count as usize)
                .checked_mul(6)
                .and_then(|v| v.checked_mul(stride))
                .ok_or(INVALID)?;
            budget = budget
                .checked_add(size)
                .filter(|v| *v <= 16 * 1024 * 1024)
                .ok_or(INVALID)?;
            let mut expanded = vec![0; size];
            for v in 0..vertex_count as usize {
                let at = v * stream.stride as usize;
                for corner in 0..6 {
                    let to = (v * 6 + corner) * stride;
                    expanded[to..to + width].copy_from_slice(&stream.bytes[at..at + width]);
                }
            }
            stream.bytes = expanded;
            stream.stride = stride as u32;
        }
        vertex_count = vertex_count.checked_mul(6).ok_or(INVALID)?;
        output_indices = None;
        draw.state.topology = adv_device::gpu::Topology::Triangles;
        draw.state.cull = adv_device::gpu::Cull::None;
    }
    let d = device(env, pointer).ok_or(INVALID)?;
    let mut bindings = Vec::new();
    let mut slots = Vec::new();
    for (vertex, t) in [(true, vt), (false, pt)] {
        if let Some(t) = t {
            for sampler in &t.samplers {
                if sampler.dimension != SamplerDimension::D2 {
                    return Err(UNAVAILABLE);
                }
                let slot = if vertex {
                    16 + usize::from(sampler.index)
                } else {
                    usize::from(sampler.index)
                };
                slots.push((vertex, sampler.index));
                bindings.push(texture(env, pointer, slot, &draw.state)?);
            }
        }
    }
    if ps.is_none() {
        slots.push((false, 0));
        bindings.push(ProgramTexture {
            image: draw.image.clone(),
            sampler: draw.state.sampler,
        });
    }

    let v = draw.state.viewport;
    let mut parameters = [[0; 4]; 40];
    parameters[0] = [
        v.width.to_bits(),
        v.height.to_bits(),
        v.x.to_bits(),
        v.y.to_bits(),
    ];
    parameters[1] = [
        op(draw.state.texture_op),
        op(draw.state.alpha_texture_op),
        compare(draw.state.alpha_test),
        draw.state.alpha_reference.to_bits(),
    ];
    parameters[2][0] = u32::from(
        draw.image
            .as_ref()
            .is_some_and(|i| i.alpha_mode() == adv_device::gpu::AlphaMode::Opaque),
    );
    parameters[2][3] = u32::from(kind == 1);
    parameters[3] = [
        d.fixed.render[&152],
        d.fixed.render[&28],
        d.fixed.render[&35],
        0,
    ];
    for n in 0..6 {
        parameters[4 + n] = d.fixed.clip_planes[n].map(f32::to_bits);
    }
    let color = d.fixed.render[&34];
    parameters[10] = [
        ((color >> 16) & 255) as f32 / 255.,
        ((color >> 8) & 255) as f32 / 255.,
        (color & 255) as f32 / 255.,
        1.,
    ]
    .map(f32::to_bits);
    parameters[11] = [
        d.fixed.render[&36],
        d.fixed.render[&37],
        d.fixed.render[&38],
        d.fixed.render[&154],
    ];
    parameters[12] = [
        d.fixed.render[&155],
        d.fixed.render[&166],
        d.fixed.render[&156],
        0,
    ];
    for (ordinal, binding) in bindings.iter().enumerate() {
        parameters[16 + ordinal] = adv_device::gpu::sampler_parameters(binding.sampler);
        if binding
            .image
            .as_ref()
            .is_some_and(|i| i.alpha_mode() == adv_device::gpu::AlphaMode::Opaque)
        {
            parameters[14][0] |= 1 << ordinal;
        }
    }
    let usage = [vt, pt].map(|t| t.map_or(Default::default(), |t| t.constants));
    let mut pixel_constants = constants(&d.shaders.pixel_constants, usage[1]);
    for (n, stage) in d.fixed.stage_slots.iter().enumerate() {
        if usage[1].registers[4] & (1 << (32 + n)) != 0 {
            pixel_constants.bump_matrix[n] = [7, 8, 9, 10].map(|k| f32::from_bits(stage[&k]));
        }
        if usage[1].registers[4] & (1 << (40 + n)) != 0 {
            pixel_constants.bump_luminance[n] = [
                f32::from_bits(stage[&22]),
                f32::from_bits(stage[&23]),
                0.,
                0.,
            ];
        }
    }
    let vertex_constants = constants(&d.shaders.vertex_constants, usage[0]);
    let key = ProgramKey {
        vertex: vs.as_ref().map(|s| s.id),
        pixel: ps.as_ref().map(|s| s.id),
        inputs: input_spec.iter().map(|i| (i.register, i.format)).collect(),
    };
    draw.program_key = recording::hash(&key);
    let cache = &mut env.ext.get_mut::<State>().programs;
    let program = if let Some(program) = cache.entries.get(&key) {
        program.clone()
    } else {
        let prepared = cache
            .warming
            .remove(&key)
            .and_then(|job| job.wait().as_ref().ok().cloned());
        let program = if let Some(program) = prepared {
            program
        } else {
            let source = source::build(
                vs.as_ref().map(|s| s.parsed.as_ref()),
                ps.as_ref().map(|s| s.parsed.as_ref()),
                vt,
                pt,
                &input_spec,
                &slots,
            )?;
            let mut program = Program::new(source.into(), slots.iter().map(|s| s.0).collect());
            program.constants = usage;
            program
        };
        const MAX_BYTES: usize = 8 * 1024 * 1024;
        if program.source().len() <= MAX_BYTES {
            if cache.entries.len() == 256 || cache.bytes + program.source().len() > MAX_BYTES {
                cache.entries.clear();
                cache.bytes = 0;
            }
            cache.bytes += program.source().len();
            cache.entries.insert(key, program.clone());
        }
        program
    };
    draw.programmable = Some(ProgrammableDraw {
        program,
        streams,
        indices: output_indices,
        vertex_count,
        vertex_constants,
        pixel_constants,
        textures: bindings,
        parameters,
    });
    Ok(draw)
}
fn texture(
    env: &Env<'_>,
    pointer: u32,
    slot: usize,
    state: &adv_device::gpu::DrawState,
) -> Result<ProgramTexture, u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    // Extended slots share the same resource ownership rules as stage zero.
    let id = if slot == 0 {
        d.texture
    } else {
        *d.shaders.textures.get(slot).ok_or(INVALID)?
    };
    let Some(id) = id else {
        return Ok(ProgramTexture {
            image: None,
            sampler: state.sampler,
        });
    };
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return Err(INVALID);
    };
    if resources::locked(env, r) {
        return Err(INVALID);
    }
    let image = (env.ext.get::<State>().provider.target)(env.ext, r.surface.ok_or(INVALID)?)
        .ok_or(INVALID)?
        .image;
    let mut sampler = d.fixed.program_sampler(slot)?;
    sampler.first_level = if sampler.mip.is_none() {
        r.lod
    } else {
        r.lod.max(sampler.first_level)
    }
    .min(image.mip_levels() - 1);
    Ok(ProgramTexture {
        image: Some(image),
        sampler,
    })
}
fn op(o: adv_device::gpu::TextureOp) -> u32 {
    use adv_device::gpu::TextureOp::*;
    match o {
        Color => 0,
        Texture => 1,
        Multiply => 2,
        Add => 3,
        Subtract => 4,
    }
}
fn compare(o: adv_device::gpu::Compare) -> u32 {
    use adv_device::gpu::Compare::*;
    match o {
        Never => 0,
        Less => 1,
        Equal => 2,
        LessEqual => 3,
        Greater => 4,
        NotEqual => 5,
        GreaterEqual => 6,
        Always => 7,
    }
}
