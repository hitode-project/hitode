//! Assemble shader state into a neutral GPU command. Guest memory is validated
//! once; raw stream bytes are retained by the command across deferred submits.
use super::*;
use crate::shader::{SamplerDimension, Shader, Translation};
use adv_device::gpu::{
    Attribute, AttributeFormat, IndexBytes, Program, ProgramTexture, ProgrammableDraw, VertexBytes,
    VertexStream,
};
mod source;
mod streams;
pub(in crate::d3d9) mod transformed;
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
    /// Whether the fixed pixel stages combine a second texture stage.
    second: bool,
    features: (u32, bool),
}
#[derive(Default)]
pub(super) struct ProgramCache {
    entries: std::collections::HashMap<ProgramKey, Program>,
    bytes: usize,
    order: std::collections::VecDeque<ProgramKey>,
    warming: std::collections::HashMap<ProgramKey, adv_device::gpu::compile::Compile<Program>>,
    warming_bytes: usize,
    warming_order: std::collections::VecDeque<(ProgramKey, usize)>,
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
    let fixed = if let Some(record) = &mut d.recording {
        &mut record.fixed
    } else {
        &mut *d.fixed
    };
    let changed = fixed.clip_planes[index as usize].map(f32::to_bits) != values.map(f32::to_bits);
    fixed.clip_planes[index as usize] = values;
    shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        shaders::stateblocks::FixedKey::ClipPlane(index as usize),
        changed,
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

/// Whether a draw takes this path: a shader is bound, or the fixed pixel
/// stages combine a second stage, which only the assembled program draws.
pub(super) fn active(env: &Env<'_>, pointer: u32) -> bool {
    device(env, pointer).is_some_and(|d| {
        d.shaders.vertex.is_some()
            || d.shaders.pixel.is_some()
            || !matches!(fixed::second_stage(&d.fixed), Ok(None))
    })
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
    let instanced =
        indices.is_some() && up.is_none() && d.buffers.frequencies[0] & buffers::INDEXED_DATA != 0;
    let instance_count = if instanced {
        buffers::instances(&d.buffers)
    } else {
        1
    };
    let fixed_raw = instanced && vs.is_none();
    let fixed_layout = if fixed_raw {
        Some(fixed::layout(env, pointer).map_err(|e| e.code)?)
    } else {
        None
    };
    if let Some(layout) = fixed_layout {
        if !layout.transformed && d.fixed.render[&137] != 0 {
            return Err(UNAVAILABLE);
        }
        if d.shaders.vertex.is_none()
            && d.shaders.pixel.is_none()
            && (d.fixed.render[&28] != 0 || d.fixed.render[&152] != 0)
        {
            return Err(UNAVAILABLE);
        }
    }
    let mut vertex_count;
    let mut output_indices = indices.map(|v| v.iter().copied().collect::<IndexBytes>());
    let (mut draw, mut streams, input_spec) = if let Some(vs) = &vs {
        let inputs = inputs(env, pointer, &vs.parsed)?;
        let first = indices.and_then(|i| i.iter().min().copied()).unwrap_or(0);
        let last = output_indices
            .as_ref()
            .and_then(IndexBytes::maximum)
            .unwrap_or(length - 1);
        vertex_count = last
            .checked_sub(first)
            .and_then(|v| v.checked_add(1))
            .ok_or(INVALID)?;
        if let Some(i) = &mut output_indices {
            i.rebase(first);
        }
        let streams = streams::capture(env, pointer, &inputs, up, start, first, last, instanced)?;
        let (target, image, state) =
            fixed::prepare_state(env, pointer, kind).map_err(|e| e.code)?;
        (
            fixed::Draw {
                target,
                image,
                state,
                vertices: Arc::default(),
                texcoords: None,
                programmable: None,
                recording_key: recording::draw_key(env, pointer),
                program_key: 0,
            },
            streams,
            inputs,
        )
    } else if fixed_raw || pretransformed(env, pointer)? {
        let layout = fixed::layout(env, pointer).map_err(|e| e.code)?;
        let inputs = transformed::inputs(layout);
        let first = indices.and_then(|v| v.iter().min().copied()).unwrap_or(0);
        let last = output_indices
            .as_ref()
            .and_then(IndexBytes::maximum)
            .unwrap_or(length - 1);
        vertex_count = last
            .checked_sub(first)
            .and_then(|n| n.checked_add(1))
            .ok_or(INVALID)?;
        if let Some(indices) = &mut output_indices {
            indices.rebase(first);
        }
        let streams = streams::capture(env, pointer, &inputs, up, start, first, last, instanced)?;
        let (target, image, state) =
            fixed::prepare_state(env, pointer, kind).map_err(|e| e.code)?;
        if layout.transformed {
            transformed::validate(
                &streams,
                state.viewport,
                output_indices.as_ref().map(|v| v.as_slice()),
                vertex_count,
            )?;
        }
        (
            fixed::Draw {
                target,
                image,
                state,
                vertices: Arc::default(),
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
        let count = draw.texcoords.as_ref().map(|(count, _)| *count);
        let mut stream = VertexStream {
            instance: false,
            bytes: VertexBytes::fixed(
                draw.vertices.clone(),
                draw.texcoords.clone(),
                Some(draw.recording_key),
            ),
            stride: count.map_or(40, |n| 32 + n as u32 * 16),
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
            ],
        };
        let mut inputs = Vec::new();
        if let Some(count) = count {
            for n in 0..count {
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
        } else {
            stream.attributes.push(Attribute {
                location: 2,
                offset: 32,
                format: AttributeFormat::Float2,
            });
        }
        (draw, vec![stream], inputs)
    };
    let transformed = vs.is_none() && input_spec.iter().any(|i| i.register == 0);
    if (vs.is_some() || transformed) && kind == 6 {
        let original = output_indices
            .take()
            .unwrap_or_else(|| (0..length).collect::<IndexBytes>());
        let mut fan = Vec::with_capacity(count as usize * 3);
        for n in 1..original.len() - 1 {
            fan.extend_from_slice(&[original[0], original[n], original[n + 1]]);
        }
        output_indices = Some(fan.into());
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
            stream.bytes = std::mem::take(&mut stream.bytes).points(
                vertex_count,
                stream.stride as usize,
                width,
            );
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
    let second = if ps.is_none() {
        slots.push((false, 0));
        bindings.push(ProgramTexture {
            image: draw.image.clone(),
            sampler: draw.state.sampler,
        });
        let second = fixed::second_stage(&d.fixed).map_err(|e| e.code)?;
        if second.is_some() {
            slots.push((false, 1));
            bindings.push(texture(env, pointer, 1, &draw.state)?);
        }
        second
    } else {
        None
    };

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
    parameters[13] = second.unwrap_or_default();
    parameters[15][0] = u32::from(transformed);
    if fixed_layout.is_some_and(|layout| !layout.transformed) {
        parameters[15][0] = 2;
        let mut matrix = d.fixed.transforms[0];
        for next in &d.fixed.transforms[1..] {
            matrix = std::array::from_fn(|i| {
                (0..4)
                    .map(|k| matrix[(i / 4) * 4 + k] * next[k * 4 + i % 4])
                    .sum()
            });
        }
        for n in 0..4 {
            parameters[36 + n] = std::array::from_fn(|k| matrix[n * 4 + k].to_bits());
        }
    }
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
    let mut extra = Vec::new();
    for (n, stage) in d.fixed.stage_slots.iter().enumerate() {
        if usage[1].registers[4] & (1 << (32 + n)) != 0 {
            extra.push((288 + n, [7, 8, 9, 10].map(|k| stage[&k])));
        }
        if usage[1].registers[4] & (1 << (40 + n)) != 0 {
            extra.push((296 + n, [stage[&22], stage[&23], 0, 0]));
        }
    }
    let pixel_constants = d.shaders.pixel_constants.snapshot(usage[1], &extra);
    let vertex_constants = d.shaders.vertex_constants.snapshot(usage[0], &[]);
    let key = ProgramKey {
        vertex: vs.as_ref().map(|s| s.id),
        pixel: ps.as_ref().map(|s| s.id),
        inputs: input_spec.iter().map(|i| (i.register, i.format)).collect(),
        second: second.is_some(),
        features: (d.fixed.render[&152], d.fixed.render[&28] != 0),
    };
    draw.program_key = recording::hash(&key);
    let cache = &mut env.ext.get_mut::<State>().programs;
    let program = if let Some(program) = cache.entries.get(&key) {
        program.clone()
    } else {
        let prepared = cache.warming.remove(&key).and_then(|job| {
            let at = cache
                .warming_order
                .iter()
                .position(|(k, _)| k == &key)
                .unwrap();
            cache.warming_bytes -= cache.warming_order.remove(at).unwrap().1;
            job.wait().as_ref().ok().cloned()
        });
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
                (second.is_some(), key.features.0, key.features.1),
            )?;
            let mut program = Program::new(source.into(), slots.iter().map(|s| s.0).collect());
            program.constants = usage;
            program
        };
        const MAX_BYTES: usize = 8 * 1024 * 1024;
        if program.source().len() <= MAX_BYTES {
            while cache.entries.len() == 256 || cache.bytes + program.source().len() > MAX_BYTES {
                let oldest = cache.order.pop_front().unwrap();
                cache.bytes -= cache.entries.remove(&oldest).unwrap().source().len();
            }
            cache.order.push_back(key.clone());
            cache.bytes += program.source().len();
            cache.entries.insert(key, program.clone());
        }
        program
    };
    draw.programmable = Some(Arc::new(ProgrammableDraw {
        instance_count,
        program,
        streams,
        indices: output_indices,
        vertex_count,
        vertex_constants,
        pixel_constants,
        textures: bindings,
        parameters,
    }));
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
    if resources::sampling_blocked(env, r) {
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
