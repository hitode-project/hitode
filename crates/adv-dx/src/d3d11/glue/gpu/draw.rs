//! Lower immutable immediate-context bindings to the existing modern executor.
use super::*;
use crate::{
    d3d11::{
        desc::{Aspects, Bind, InputRate, Misc},
        state::{STREAMS, Snapshot, Stage, ViewSlot},
    },
    shader::{
        analysis::Reflection,
        expand,
        ir::{BindingKind, Register, ResourceDimension, SamplerMode, SystemValue},
        variant::{DrawFacts, Variant},
        wgsl::{
            self, BindingLocation, BorderChannel, BorderSampler, BorderTexture, LodSampler,
            Options, TextureLod, Translation,
            pulled::{
                self, DRAW_ARGUMENTS_SLOT, DrawStart, InputStep, VERTEX_STREAM_BASE,
                VertexComponent, VertexFetch, VertexFormat,
            },
        },
    },
};
use adv_abi::types::{
    D3D11_BLEND_DESC1, D3D11_DEPTH_STENCIL_DESC, D3D11_DEPTH_STENCILOP_DESC,
    D3D11_RASTERIZER_DESC1, D3D11_SAMPLER_DESC,
};
use adv_device::gpu::{
    DispatchArguments, DrawArguments, ModernColor, ModernDepth, ModernDraw, ModernShader,
    SlotBindings, StorageBinding,
};
use adv_gpu::{
    encode::{IndexFormat, Load, Store},
    pipeline as p, resource as r,
};
use std::{ops::Range, sync::Arc};

#[derive(Clone)]
enum DrawRange {
    Direct {
        vertices: Range<u32>,
        instances: Range<u32>,
        base_vertex: i32,
    },
    Indirect(BufferRange, ResourceKey),
}

impl DrawRange {
    fn direct(
        vertices: u32,
        first: u32,
        instances: u32,
        base_instance: u32,
        base_vertex: i32,
    ) -> Result<Self, Error> {
        Ok(Self::Direct {
            vertices: first..first.checked_add(vertices).ok_or(Error::InvalidArgument)?,
            instances: base_instance
                ..base_instance
                    .checked_add(instances)
                    .ok_or(Error::InvalidArgument)?,
            base_vertex,
        })
    }

    fn arguments(&self) -> DrawArguments {
        match self {
            Self::Direct {
                vertices,
                instances,
                base_vertex,
            } => DrawArguments::Direct {
                vertices: vertices.clone(),
                instances: instances.clone(),
                base_vertex: *base_vertex,
            },
            Self::Indirect(buffer, _) => DrawArguments::Indirect(buffer.clone()),
        }
    }

    fn start(&self, indexed: bool) -> DrawStart {
        match self {
            Self::Direct {
                vertices,
                instances,
                base_vertex,
            } => DrawStart::Direct {
                vertex: if indexed {
                    *base_vertex as u32
                } else {
                    vertices.start
                },
                instance: instances.start,
            },
            Self::Indirect(args, _) => DrawStart::Indirect {
                first_word: (args.range.start / 4) as u32,
                indexed,
            },
        }
    }
}

pub(in crate::d3d11::glue) fn draw(env: &mut Env<'_>) -> Result<(), Error> {
    render(
        env,
        false,
        DrawRange::direct(env.arg(1), env.arg(2), 1, 0, 0)?,
    )
}
pub(in crate::d3d11::glue) fn draw_indexed(env: &mut Env<'_>) -> Result<(), Error> {
    render(
        env,
        true,
        DrawRange::direct(env.arg(1), env.arg(2), 1, 0, env.arg(3) as i32)?,
    )
}
pub(in crate::d3d11::glue) fn draw_instanced(env: &mut Env<'_>) -> Result<(), Error> {
    render(
        env,
        false,
        DrawRange::direct(env.arg(1), env.arg(3), env.arg(2), env.arg(4), 0)?,
    )
}
pub(in crate::d3d11::glue) fn draw_indexed_instanced(env: &mut Env<'_>) -> Result<(), Error> {
    render(
        env,
        true,
        DrawRange::direct(
            env.arg(1),
            env.arg(3),
            env.arg(2),
            env.arg(5),
            env.arg(4) as i32,
        )?,
    )
}

pub(in crate::d3d11::glue) fn draw_indirect(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (args, resource) = indirect(env, 16)?;
    render(env, false, DrawRange::Indirect(args, resource))
}
pub(in crate::d3d11::glue) fn draw_indexed_indirect(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (args, resource) = indirect(env, 20)?;
    render(env, true, DrawRange::Indirect(args, resource))
}

pub(in crate::d3d11::glue) fn draw_auto(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let source = context::groups(env)?.ia.vertices[0].buffer;
    if !context::owned(env, source)
        .and_then(Object::resource)
        .is_some_and(|resource| resource.bind().contains(Bind::STREAM_OUTPUT))
    {
        return Ok(());
    }
    let cursor = stream_cursor(env, source)?;
    let key = key(env, source, 0);
    let exec = execution(env.ext)?;
    let (pipeline, slots, arguments) = expand::draw_auto(&exec.gpu, cursor)?;
    let extent = extent_rect([1, 1, 1]);
    exec.recording.append(
        &mut exec.gpu,
        Command::Dispatch {
            pipeline: Arc::new(pipeline),
            bindings: slots,
            arguments: DispatchArguments::Direct([1, 1, 1]),
        },
        Effects {
            extent,
            bounds: extent,
            key: 0,
            reads: &[key],
            writes: &[(key, false)],
            retained_bytes: 0,
        },
    )?;
    render(env, false, DrawRange::Indirect(arguments, key))
}

fn indirect(env: &mut Env<'_>, length: u32) -> Result<(BufferRange, ResourceKey), Error> {
    let (pointer, offset) = (env.arg(1), env.arg(2));
    let desc = description(env, pointer)?;
    let Resource::Buffer(buffer) = desc else {
        return Err(Error::InvalidArgument);
    };
    if offset % 4 != 0
        || offset
            .checked_add(length)
            .is_none_or(|end| end > buffer.size())
        || !desc.misc().contains(Misc::INDIRECT)
    {
        return Err(Error::InvalidArgument);
    }
    let Backing::Buffer(buffer) = backing(env, pointer, desc)? else {
        unreachable!()
    };
    Ok((
        BufferRange {
            buffer,
            range: u64::from(offset)..u64::from(offset + length),
        },
        key(env, pointer, 0),
    ))
}

fn bound_shader<'a>(
    env: &'a Env<'_>,
    snapshot: &'a Snapshot,
    stage: Stage,
) -> Option<&'a crate::d3d11::desc::Shader> {
    let group = &snapshot.stages[stage as usize];
    group
        .specialized
        .as_deref()
        .or_else(|| context::owned(env, group.shader).and_then(Object::shader))
}

fn pipeline<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a PipelineState> {
    table(env).get(&env.sys.objects, pointer)?.pipeline()
}

#[derive(Default)]
struct IaCapture {
    hash: xxhash_rust::xxh3::Xxh3,
    bytes: usize,
}

impl IaCapture {
    fn record(&mut self, pointer: u32, range: &Range<u32>, bytes: &[u8]) {
        for word in [pointer, range.start, range.end] {
            self.hash.update(&word.to_le_bytes());
        }
        self.hash.update(bytes);
        self.bytes += bytes.len();
    }
}

struct CapturedIndex {
    binding: (BufferRange, IndexFormat),
    vertices: Option<Range<u32>>,
}

fn ensure_index(
    env: &mut Env<'_>,
    state: &Snapshot,
    args: &mut DrawRange,
    strip: bool,
    capture: &mut IaCapture,
) -> Result<CapturedIndex, Error> {
    let binding = state.ia.indices;
    let (format, stride) = match binding.format {
        42 => (IndexFormat::U32, 4),
        57 => (IndexFormat::U16, 2),
        _ => return Err(Error::InvalidArgument),
    };
    let desc = description(env, binding.buffer)?;
    let Resource::Buffer(buffer) = desc else {
        return Err(Error::InvalidArgument);
    };
    if !binding.offset.is_multiple_of(stride)
        || binding.offset >= buffer.size()
        || matches!(args, DrawRange::Direct { vertices, .. }
            if u64::from(vertices.end) * u64::from(stride) + u64::from(binding.offset) > u64::from(buffer.size()))
    {
        return Err(Error::InvalidArgument);
    }
    if desc.usage() == Usage::Dynamic
        && let DrawRange::Direct {
            vertices,
            base_vertex,
            ..
        } = args
    {
        let range =
            binding.offset + vertices.start * stride..binding.offset + vertices.end * stride;
        let bytes = shadow_bytes(env, binding.buffer, range.clone())?;
        capture.record(binding.buffer, &range, &bytes);
        let restart = if stride == 2 { 0xffff } else { u32::MAX };
        let mut first = u32::MAX;
        let mut last = 0;
        for bytes in bytes.chunks_exact(stride as usize) {
            let mut word = [0; 4];
            word[..bytes.len()].copy_from_slice(bytes);
            let index = u32::from_le_bytes(word);
            if strip && index == restart {
                continue;
            }
            let vertex = i64::from(index) + i64::from(*base_vertex);
            if let Ok(vertex) = u32::try_from(vertex) {
                first = first.min(vertex);
                last = last.max(vertex);
            }
        }
        *vertices = 0..vertices.len() as u32;
        let length = bytes.len();
        let backing = resource::upload(
            &execution(env.ext)?.gpu,
            &bytes,
            BufferUsage::INDEX | BufferUsage::STORAGE,
            (length as u64).next_multiple_of(4),
        )?;
        let range = 0..length as u64;
        let fetched = if first <= last {
            first..last.saturating_add(1)
        } else {
            0..0
        };
        return Ok(CapturedIndex {
            binding: (
                BufferRange {
                    buffer: backing,
                    range,
                },
                format,
            ),
            vertices: Some(fetched),
        });
    }
    let backing = capture_buffer(
        env,
        binding.buffer,
        BufferUsage::INDEX | BufferUsage::STORAGE,
        binding.offset..buffer.size(),
        capture,
    )?;
    let native = if desc.usage() == Usage::Dynamic {
        0..u64::from(buffer.size() - binding.offset)
    } else {
        u64::from(binding.offset)..u64::from(buffer.size())
    };
    Ok(CapturedIndex {
        binding: (
            BufferRange {
                buffer: backing,
                range: native,
            },
            format,
        ),
        vertices: None,
    })
}

fn shadow_bytes(env: &mut Env<'_>, pointer: u32, range: Range<u32>) -> Result<Vec<u8>, Error> {
    let table = table(env);
    let object = table.get_mut(&mut env.sys.objects, pointer).unwrap();
    let base = object.mapping().unwrap().pointer();
    array(env, base + range.start, range.end - range.start)
}

fn capture_buffer(
    env: &mut Env<'_>,
    pointer: u32,
    usage: BufferUsage,
    range: Range<u32>,
    capture: &mut IaCapture,
) -> Result<adv_gpu::Buffer, Error> {
    let desc = description(env, pointer)?;
    let Resource::Buffer(_) = desc else {
        return Err(Error::InvalidArgument);
    };
    if desc.usage() == Usage::Dynamic {
        let bytes = shadow_bytes(env, pointer, range.clone())?;
        capture.record(pointer, &range, &bytes);
        let size = bytes.len();
        let buffer = resource::upload(
            &execution(env.ext)?.gpu,
            &bytes,
            usage,
            (size as u64).max(1).next_multiple_of(4),
        )?;
        return Ok(buffer);
    }
    let Backing::Buffer(buffer) = backing(env, pointer, desc)? else {
        unreachable!()
    };
    Ok(buffer)
}

fn vertex_capture(
    fetch: VertexFetch,
    size: u32,
    args: &DrawRange,
    vertices: Option<&Range<u32>>,
) -> Range<u32> {
    let elements = match (fetch.step, args) {
        (InputStep::Vertex, _) => vertices.cloned(),
        (InputStep::Instance(rate), DrawRange::Direct { instances, .. }) => {
            let last = (instances.len() as u32 - 1).checked_div(rate).unwrap_or(0);
            Some(instances.start..instances.start + last + 1)
        }
        _ => None,
    };
    let address = |index: u32| {
        (u64::from(index) * u64::from(fetch.stride) + u64::from(fetch.offset)).min(u64::from(size))
            as u32
    };
    match elements {
        Some(elements) if elements.is_empty() => 0..0,
        Some(elements) => {
            address(elements.start)
                ..address(elements.end - 1)
                    .saturating_add(fetch.format.bytes)
                    .min(size)
        }
        None => fetch.offset.min(size)..size,
    }
}

fn capture_texture(
    env: &mut Env<'_>,
    pointer: u32,
    desc: Resource,
    texture: Texture,
) -> Result<Backing, Error> {
    let table = table(env);
    let storage = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .mapping()
        .unwrap();
    let base = storage.pointer();
    let layout = storage.layout(0);
    let [width, height, depth] = texture.extent(0);
    let format = texture.format();
    let rows = height.div_ceil(u32::from(format.alignment[1]));
    let row = width.div_ceil(u32::from(format.alignment[0])) * u32::from(format.aligned_unit_bytes);
    let mut bytes = Vec::with_capacity((row * rows * depth) as usize);
    for slice in 0..depth {
        for y in 0..rows {
            bytes.extend(array::<u8>(
                env,
                layout.slice(base, slice) + y * layout.row_pitch(),
                row,
            )?);
        }
    }
    let execution = execution(env.ext)?;
    let backing = Backing::new(&execution.gpu, desc)?;
    let Backing::Image(image) = &backing else {
        unreachable!()
    };
    let destination = resource::region(image, texture, 0);
    let source = resource::texture_transfer(&execution.gpu, texture, &destination, &bytes)?;
    execution.gpu.transfer(ModernTransfer::Upload {
        source,
        destination,
    })?;
    Ok(backing)
}

fn slots_for(
    env: &mut Env<'_>,
    snapshot: &Snapshot,
    stage: Stage,
    (reflection, locations, counters): (&Reflection, &[BindingLocation], &[(u32, u32)]),
    reads: &mut Vec<ResourceKey>,
) -> Result<SlotBindings, Error> {
    let state = &snapshot.stages[stage as usize];
    let mut slots = SlotBindings::default();
    use Register::*;
    for location in locations {
        let index = location.slot as usize;
        let binding = location.binding as usize;
        match location.register {
            ConstantBuffer => {
                let constant = state.constants[index];
                let length = reflection
                    .bindings
                    .iter()
                    .find_map(|binding| match binding.kind {
                        BindingKind::ConstantBuffer { bytes, .. }
                            if binding.slot == location.slot =>
                        {
                            Some(bytes)
                        }
                        _ => None,
                    })
                    .unwrap()
                    .max(16);
                let selected = length.min(constant.count.saturating_mul(16));
                let offset = u64::from(constant.first) * 16;
                let gpu_buffer = if constant.buffer == 0 {
                    resource::upload(
                        &execution(env.ext)?.gpu,
                        &[],
                        BufferUsage::UNIFORM,
                        u64::from(length),
                    )?
                } else {
                    reads.push(key(env, constant.buffer, 0));
                    let desc = description(env, constant.buffer)?;
                    let super::Resource::Buffer(buf) = desc else {
                        unreachable!()
                    };
                    let available = u64::from(buf.size())
                        .saturating_sub(offset)
                        .min(u64::from(selected));
                    if desc.usage() == Usage::Dynamic
                        || available < u64::from(length)
                        || offset != 0
                    {
                        let source = if desc.usage() == Usage::Dynamic {
                            let table = table(env);
                            let pointer = table
                                .get_mut(&mut env.sys.objects, constant.buffer)
                                .unwrap()
                                .mapping()
                                .unwrap()
                                .pointer();
                            Some(array::<u8>(
                                env,
                                pointer
                                    .checked_add(offset as u32)
                                    .ok_or(Error::InvalidArgument)?,
                                available as u32,
                            )?)
                        } else {
                            None
                        };
                        let gpu = &execution(env.ext)?.gpu;
                        let buffer = resource::upload(
                            gpu,
                            source.as_deref().unwrap_or_default(),
                            BufferUsage::UNIFORM | BufferUsage::COPY_DST,
                            u64::from(length),
                        )?;
                        if source.is_none() && available != 0 {
                            let Backing::Buffer(native) = backing(env, constant.buffer, desc)?
                            else {
                                unreachable!()
                            };
                            let transfer = ModernTransfer::CopyBuffer {
                                source: BufferRange {
                                    buffer: native,
                                    range: offset..offset + available,
                                },
                                destination: buffer.clone(),
                                offset: 0,
                            };
                            let read_key = key(env, constant.buffer, 0);
                            execution(env.ext)?.append(
                                Command::Transfer(transfer),
                                extent_rect([length, 1, 1]),
                                Some(read_key),
                                read_key,
                            )?;
                        }
                        buffer
                    } else {
                        let Backing::Buffer(buffer) = backing(env, constant.buffer, desc)? else {
                            unreachable!()
                        };
                        buffer
                    }
                };
                set_slot(
                    &mut slots.uniforms,
                    binding,
                    BufferRange {
                        buffer: gpu_buffer,
                        range: 0..u64::from(length),
                    },
                );
            }
            Sampler => {
                let mut desc = sampler_description(env, state.samplers[index]);
                let shader_bias = round_point_mip(desc);
                if let Some(minimum) = location.sampler_min_lod_bits.map(f32::from_bits) {
                    desc.MinLOD = desc.MinLOD.max(minimum);
                    desc.MaxLOD = desc.MaxLOD.max(desc.MinLOD);
                }
                let mode = reflection
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.register == Register::Sampler && binding.slot == location.slot
                    })
                    .unwrap()
                    .kind;
                let sampler = execution(env.ext)?.gpu.device().sampler(&sampler_desc(
                    desc,
                    mode,
                    shader_bias,
                ))?;
                set_slot(&mut slots.samplers, binding, sampler);
            }
            Resource | UnorderedAccessView => {
                let view_slot = match (location.register, stage) {
                    (Resource, stage) => ViewSlot::srv(stage, index),
                    (UnorderedAccessView, Stage::Compute) => ViewSlot::cs_uav(index),
                    (UnorderedAccessView, _) => ViewSlot::om_uav(index),
                    _ => unreachable!(),
                }
                .ok_or(Error::InvalidArgument)?;
                let view = snapshot.view(view_slot).ok_or(Error::InvalidArgument)?;
                let desc = description(env, view.resource)?;
                reads.push(key(env, view.resource, view.desc.subresources().mips.start));
                let backing = match desc {
                    super::Resource::Texture(texture) if desc.usage() == Usage::Dynamic => {
                        capture_texture(env, view.resource, desc, texture)?
                    }
                    _ => backing(env, view.resource, desc)?,
                };
                match (backing, desc) {
                    (Backing::Image(image), _) => {
                        let view = Backing::Image(image).view(&view.desc)?;
                        if location.group == 1 {
                            set_slot(&mut slots.images, binding, view);
                        } else {
                            set_slot(&mut slots.storage, binding, StorageBinding::Image(view));
                        }
                    }
                    (Backing::Buffer(buffer), super::Resource::Buffer(desc)) => {
                        let unit = (view.desc.format() != 0)
                            .then(|| crate::dxgi::formats::format_layout(view.desc.format()))
                            .flatten()
                            .map_or(desc.stride().max(4), |layout| {
                                u32::from(layout.aligned_unit_bytes).max(1)
                            });
                        let elements = view.desc.elements();
                        let range = u64::from(elements.start) * u64::from(unit)
                            ..u64::from(elements.end) * u64::from(unit);
                        set_slot(
                            &mut slots.storage,
                            binding,
                            StorageBinding::Buffer(BufferRange { buffer, range }),
                        );
                    }
                    _ => unreachable!(),
                }
            }
            _ => return Err(Error::InvalidArgument),
        }
    }
    for &(slot, binding) in counters {
        let view = match stage {
            Stage::Compute => ViewSlot::cs_uav(slot as usize),
            _ => ViewSlot::om_uav(slot as usize),
        }
        .and_then(|slot| snapshot.view(slot))
        .ok_or(Error::InvalidArgument)?;
        let counter = counter_buffer(env, view.pointer)?;
        set_slot(
            &mut slots.storage,
            binding as usize,
            StorageBinding::Buffer(BufferRange {
                buffer: counter,
                range: 0..4,
            }),
        );
    }
    Ok(slots)
}

fn bindings(translation: &Translation) -> (&Reflection, &[BindingLocation], &[(u32, u32)]) {
    (
        &translation.reflection,
        &translation.locations,
        &translation.counters,
    )
}

fn set_slot<T>(slots: &mut Vec<Option<T>>, slot: usize, value: T) {
    if slots.len() <= slot {
        slots.resize_with(slot + 1, || None);
    }
    slots[slot] = Some(value);
}

fn sampler_description(env: &Env<'_>, pointer: u32) -> D3D11_SAMPLER_DESC {
    match pipeline(env, pointer) {
        Some(PipelineState::Sampler(desc)) => *desc,
        _ => D3D11_SAMPLER_DESC {
            Filter: 0x15,
            AddressU: 3,
            AddressV: 3,
            AddressW: 3,
            MaxLOD: f32::MAX,
            ComparisonFunc: 1,
            ..Default::default()
        },
    }
}

#[derive(Default)]
struct ShaderViews {
    storage_formats: Vec<(u32, naga::StorageFormat)>,
    buffer_formats: Vec<(u32, naga::StorageFormat)>,
    buffer_elements: Vec<(Register, u32, u32)>,
    writable: Vec<(u32, u32)>,
}

impl ShaderViews {
    fn new(snapshot: &Snapshot, stage: Stage, reflection: &Reflection) -> Result<Self, Error> {
        let mut views = Self::default();
        for binding in &reflection.bindings {
            let writable = binding.register == Register::UnorderedAccessView;
            let slot = match binding.register {
                Register::Resource => ViewSlot::srv(stage, binding.slot as usize),
                Register::UnorderedAccessView if stage == Stage::Compute => {
                    ViewSlot::cs_uav(binding.slot as usize)
                }
                Register::UnorderedAccessView => ViewSlot::om_uav(binding.slot as usize),
                _ => None,
            };
            let Some(view) = slot.and_then(|slot| snapshot.view(slot)) else {
                continue;
            };
            if writable {
                views
                    .writable
                    .push((view.resource, view.desc.subresources().mips.start));
            }
            if let BindingKind::Typed { dimension, .. } = binding.kind {
                if writable {
                    views
                        .storage_formats
                        .push((binding.slot, storage_format(view.desc.format())?));
                } else if dimension == ResourceDimension::Buffer {
                    views
                        .buffer_formats
                        .push((binding.slot, storage_format(view.desc.format())?));
                }
                if dimension == ResourceDimension::Buffer {
                    views.buffer_elements.push((
                        binding.register,
                        binding.slot,
                        view.desc.elements().len() as u32,
                    ));
                }
            }
        }
        Ok(views)
    }

    fn writes(&self, env: &Env<'_>, writes: &mut Vec<(ResourceKey, bool)>) {
        writes.extend(
            self.writable
                .iter()
                .map(|&(resource, mip)| (key(env, resource, mip), false)),
        );
    }

    fn options<'a>(&'a self, options: Options<'a>) -> Options<'a> {
        Options {
            storage_formats: &self.storage_formats,
            buffer_formats: &self.buffer_formats,
            buffer_elements: &self.buffer_elements,
            ..options
        }
    }
}

#[derive(Default)]
struct Sampling {
    lods: Vec<TextureLod>,
    samplers: Vec<(u32, LodSampler)>,
    border_samplers: Vec<BorderSampler>,
    border_textures: Vec<BorderTexture>,
    depths: Vec<u32>,
    depths24: Vec<u32>,
    unorm: Vec<(u32, [u8; 4])>,
}

impl Sampling {
    fn new(env: &Env<'_>, snapshot: &Snapshot, stage: Stage, reflection: &Reflection) -> Self {
        use crate::dxgi::formats::Interpretation;
        let mut state = Self::default();
        for binding in &reflection.bindings {
            if binding.register != Register::Sampler {
                continue;
            }
            let slot = binding.slot;
            let sampler =
                sampler_description(env, snapshot.stages[stage as usize].samplers[slot as usize]);
            let lod = LodSampler {
                bias: sampler.MipLODBias,
                min: sampler.MinLOD,
                max: sampler.MaxLOD,
                max_anisotropy: if sampler.Filter & 0x40 == 0 {
                    1
                } else {
                    sampler.MaxAnisotropy as u16
                },
                round_mip: round_point_mip(sampler),
                point: sampler.Filter == 0,
                linear_comparison: sampler.Filter & 0x184 == 0x84,
            };
            if reflection.lod_queries || lod.round_mip || lod.point || lod.linear_comparison {
                state.samplers.push((slot, lod));
            }
            if custom_border(sampler) {
                state.border_samplers.push(BorderSampler {
                    slot,
                    axes: [sampler.AddressU, sampler.AddressV, sampler.AddressW]
                        .map(|address| address == 4),
                    color: sampler.BorderColor,
                    linear: [0x10, 0x4, 0x1].map(|mask| sampler.Filter & mask != 0),
                    lod,
                    comparison: sampler.ComparisonFunc,
                });
            }
        }
        for binding in &reflection.bindings {
            if binding.register != Register::Resource {
                continue;
            }
            let Some(view) =
                ViewSlot::srv(stage, binding.slot as usize).and_then(|slot| snapshot.view(slot))
            else {
                continue;
            };
            let minimum = context::owned(env, view.resource)
                .and_then(Object::min_lod)
                .unwrap_or(0.);
            let mips = &view.desc.subresources().mips;
            let minimum = minimum - mips.start as f32;
            let depth = view.desc.aspects().intersects(Aspects::DEPTH);
            if depth {
                state.depths.push(binding.slot);
            }
            if view.desc.format() == 46 {
                state.depths24.push(binding.slot);
            }
            let format = crate::dxgi::formats::format_layout(view.desc.format());
            if !depth
                && let Some(format) = format
                && format.alignment == [1; 3]
            {
                let widths = std::array::from_fn(|lane| {
                    let bits = format.component_bits[lane];
                    if format.interpretations[lane] == Interpretation::Unorm && bits <= 16 {
                        bits
                    } else {
                        0
                    }
                });
                if widths != [0; 4] {
                    state.unorm.push((binding.slot, widths));
                }
            } else if view.desc.format() == 98 {
                state.unorm.push((binding.slot, [8; 4]));
            }
            if !state.border_samplers.is_empty()
                && let Some(format) = format
            {
                let channels = std::array::from_fn(|lane| {
                    let bits = format.component_bits[lane];
                    match format.interpretations[lane] {
                        _ if bits == 0 => BorderChannel::Missing,
                        Interpretation::Unorm => BorderChannel::Unorm(bits),
                        Interpretation::Snorm => BorderChannel::Snorm(bits),
                        _ => BorderChannel::Float,
                    }
                });
                state.border_textures.push(BorderTexture {
                    slot: binding.slot,
                    channels,
                });
            }
            if minimum > 0. {
                let format = format.unwrap();
                let default_alpha = if format.component_bits[3] != 0 {
                    0
                } else if matches!(
                    format.interpretations[0],
                    Interpretation::Uint | Interpretation::Sint
                ) {
                    1
                } else {
                    1f32.to_bits()
                };
                state.lods.push(TextureLod {
                    slot: binding.slot,
                    minimum,
                    last: mips.len() as u32 - 1,
                    default_alpha,
                });
            }
        }
        state
    }

    fn options(&self) -> Options<'_> {
        Options {
            texture_lods: &self.lods,
            lod_samplers: &self.samplers,
            border_samplers: &self.border_samplers,
            border_textures: &self.border_textures,
            depth_textures: &self.depths,
            depth24_textures: &self.depths24,
            exact_unorm: &self.unorm,
            ..Default::default()
        }
    }
}

fn custom_border(desc: D3D11_SAMPLER_DESC) -> bool {
    [desc.AddressU, desc.AddressV, desc.AddressW].contains(&4)
        && ![[0.; 4], [0., 0., 0., 1.], [1.; 4]].contains(&desc.BorderColor)
}

fn round_point_mip(desc: D3D11_SAMPLER_DESC) -> bool {
    matches!(desc.Filter, 0 | 0x14) && desc.MinLOD.fract() == 0. && desc.MaxLOD.fract() == 0.
}

fn sampler_desc(desc: D3D11_SAMPLER_DESC, mode: BindingKind, shader_bias: bool) -> r::SamplerDesc {
    let linear = |mask| {
        if desc.Filter & mask != 0 {
            r::Filter::Linear
        } else {
            r::Filter::Nearest
        }
    };
    let address = |value| match value {
        1 => r::Address::Repeat,
        2 => r::Address::Mirror,
        3 => r::Address::Clamp,
        4 => r::Address::Border,
        5 => r::Address::MirrorOnce,
        _ => unreachable!(),
    };
    let border = if desc.BorderColor == [1.; 4] {
        r::BorderColor::OpaqueWhite
    } else if desc.BorderColor == [0., 0., 0., 1.] {
        r::BorderColor::OpaqueBlack
    } else {
        r::BorderColor::TransparentBlack
    };
    let get_lod = |value: f32| (value * 256.).round() as u16;
    r::SamplerDesc {
        min: linear(0x10),
        mag: linear(0x4),
        mip: linear(0x1),
        address: [desc.AddressU, desc.AddressV, desc.AddressW].map(address),
        border,
        lod_bias: if shader_bias {
            0
        } else {
            (desc.MipLODBias * 256.).round() as i16
        },
        lod_min: get_lod(desc.MinLOD),
        lod_max: get_lod(desc.MaxLOD),
        anisotropy: if desc.Filter & 0x40 == 0 {
            1
        } else {
            desc.MaxAnisotropy as u8
        },
        compare: (mode == BindingKind::Sampler(SamplerMode::Comparison))
            .then(|| compare(desc.ComparisonFunc)),
    }
}

fn compare(value: i32) -> p::Compare {
    match value {
        1 => p::Compare::Never,
        2 => p::Compare::Less,
        3 => p::Compare::Equal,
        4 => p::Compare::LessEqual,
        5 => p::Compare::Greater,
        6 => p::Compare::NotEqual,
        7 => p::Compare::GreaterEqual,
        8 => p::Compare::Always,
        _ => unreachable!(),
    }
}
fn blend_factor(value: i32) -> p::Factor {
    match value {
        1 => p::Factor::Zero,
        2 => p::Factor::One,
        3 => p::Factor::Src,
        4 => p::Factor::OneMinusSrc,
        5 => p::Factor::SrcAlpha,
        6 => p::Factor::OneMinusSrcAlpha,
        7 => p::Factor::DstAlpha,
        8 => p::Factor::OneMinusDstAlpha,
        9 => p::Factor::Dst,
        10 => p::Factor::OneMinusDst,
        11 => p::Factor::SrcAlphaSaturated,
        14 | 20 => p::Factor::Constant,
        15 | 21 => p::Factor::OneMinusConstant,
        16 => p::Factor::Src1,
        17 => p::Factor::OneMinusSrc1,
        18 => p::Factor::Src1Alpha,
        19 => p::Factor::OneMinusSrc1Alpha,
        _ => unreachable!(),
    }
}
fn blend_component(src: i32, dst: i32, op: i32) -> p::BlendComponent {
    let op = match op {
        1 => p::BlendOp::Add,
        2 => p::BlendOp::Subtract,
        3 => p::BlendOp::ReverseSubtract,
        4 => p::BlendOp::Min,
        5 => p::BlendOp::Max,
        _ => unreachable!(),
    };
    p::BlendComponent {
        src: blend_factor(src),
        dst: blend_factor(dst),
        op,
    }
}
fn blend_color(desc: Option<D3D11_BLEND_DESC1>, slot: usize, view: ImageView) -> ModernColor {
    let target = desc.map(|desc| {
        desc.RenderTarget[if desc.IndependentBlendEnable != 0 {
            slot
        } else {
            0
        }]
    });
    ModernColor {
        view,
        load: Load::Load,
        store: Store::Store,
        blend: target
            .filter(|target| target.BlendEnable != 0)
            .map(|target| {
                (
                    blend_component(target.SrcBlend, target.DestBlend, target.BlendOp),
                    blend_component(
                        target.SrcBlendAlpha,
                        target.DestBlendAlpha,
                        target.BlendOpAlpha,
                    ),
                )
            }),
        write_mask: target.map_or(15, |target| target.RenderTargetWriteMask),
    }
}
fn depth_stencil(
    desc: Option<D3D11_DEPTH_STENCIL_DESC>,
    view: ImageView,
    aspects: Aspects,
    bias: bool,
) -> ModernDepth {
    let depth_write = aspects.intersects(Aspects::DEPTH)
        && desc.is_none_or(|desc| desc.DepthEnable != 0 && desc.DepthWriteMask != 0);
    let stencil = |desc: D3D11_DEPTH_STENCILOP_DESC| {
        let op = |value| match value {
            1 => p::StencilOp::Keep,
            2 => p::StencilOp::Zero,
            3 => p::StencilOp::Replace,
            4 => p::StencilOp::IncrementClamp,
            5 => p::StencilOp::DecrementClamp,
            6 => p::StencilOp::Invert,
            7 => p::StencilOp::IncrementWrap,
            8 => p::StencilOp::DecrementWrap,
            _ => unreachable!(),
        };
        p::StencilFace {
            compare: compare(desc.StencilFunc),
            fail: op(desc.StencilFailOp),
            depth_fail: op(desc.StencilDepthFailOp),
            pass: op(desc.StencilPassOp),
        }
    };
    ModernDepth {
        state: p::DepthStencil {
            format: view.format(),
            depth_compare: desc.map_or(p::Compare::Less, |desc| {
                if desc.DepthEnable != 0 {
                    compare(desc.DepthFunc)
                } else {
                    p::Compare::Always
                }
            }),
            depth_write,
            stencil: desc
                .filter(|desc| desc.StencilEnable != 0)
                .map(|desc| (stencil(desc.FrontFace), stencil(desc.BackFace))),
            stencil_read_mask: desc.map_or(255, |desc| desc.StencilReadMask),
            stencil_write_mask: if aspects.intersects(Aspects::STENCIL) {
                desc.map_or(255, |desc| desc.StencilWriteMask)
            } else {
                0
            },
            depth_bias: bias,
        },
        view,
        depth: (Load::Load, Store::Store),
        stencil: (Load::Load, Store::Store),
        read_only: !aspects.intersects(Aspects::DEPTH) && !aspects.intersects(Aspects::STENCIL),
    }
}

fn render(env: &mut Env<'_>, indexed: bool, mut args: DrawRange) -> Result<(), Error> {
    if null_device(env)?
        || matches!(&args, DrawRange::Direct { vertices, instances, .. } if vertices.is_empty() || instances.is_empty())
    {
        return Ok(());
    }
    let snapshot = context::groups(env)?.clone();
    let vertex = snapshot.stages[Stage::Vertex as usize].shader;
    if vertex == 0 {
        return Ok(());
    }
    let pixel = snapshot.stages[Stage::Pixel as usize].shader;
    let geometry = snapshot.stages[Stage::Geometry as usize].shader;
    let so = bound_shader(env, &snapshot, Stage::Geometry)
        .and_then(|shader| shader.stream_output.as_ref());
    let (raster_stream, so_declarations): (_, Vec<_>) = if let Some(so) = so {
        let targets = so
            .targets
            .iter()
            .zip(snapshot.so.buffers)
            .filter_map(|(target, buffer)| {
                let target = target.as_ref().filter(|_| buffer != 0)?;
                Some((
                    buffer,
                    expand::StreamOutput {
                        stream: target.stream,
                        elements: target.elements.clone(),
                    },
                ))
            })
            .collect();
        (so.rasterized, targets)
    } else {
        (Some(0), Vec::new())
    };
    let layout = snapshot.ia.layout;
    let raster = match pipeline(env, snapshot.rs.state) {
        Some(PipelineState::Rasterizer(desc)) => *desc,
        _ => D3D11_RASTERIZER_DESC1 {
            FillMode: 3,
            CullMode: 3,
            DepthClipEnable: 1,
            ..Default::default()
        },
    };
    let blend = match pipeline(env, snapshot.om.blend) {
        Some(PipelineState::Blend(desc)) => Some(**desc),
        _ => None,
    };
    let depth = match pipeline(env, snapshot.om.depth_stencil) {
        Some(PipelineState::DepthStencil(desc)) => Some(*desc),
        _ => None,
    };
    let (topology, input_topology, input_vertices, step, flip_winding) = match snapshot.ia.topology
    {
        0 | 33..=64 => return Ok(()),
        1 => (p::Topology::Points, expand::Topology::Point, 1, 1, false),
        2 => (p::Topology::Lines, expand::Topology::Line, 2, 2, false),
        3 => (p::Topology::LineStrip, expand::Topology::Line, 2, 1, false),
        4 => (
            p::Topology::Triangles,
            expand::Topology::Triangle,
            3,
            3,
            false,
        ),
        5 => (
            p::Topology::TriangleStrip,
            expand::Topology::Triangle,
            3,
            1,
            true,
        ),
        10 => (p::Topology::Lines, expand::Topology::Line, 4, 4, false),
        11 => (p::Topology::LineStrip, expand::Topology::Line, 4, 1, false),
        12 => (
            p::Topology::Triangles,
            expand::Topology::Triangle,
            6,
            6,
            false,
        ),
        13 => (
            p::Topology::TriangleStrip,
            expand::Topology::Triangle,
            6,
            2,
            true,
        ),
        _ => return Err(Error::InvalidArgument),
    };
    let mut colors = Vec::new();
    let mut color_slots = 0;
    let mut writes = Vec::new();
    for slot in 0..super::super::super::state::RENDER_TARGETS {
        let Some(bound) = snapshot.view(ViewSlot::rtv(slot).unwrap()) else {
            continue;
        };
        let (view, key) = image_view(env, bound.pointer, ViewKind::RenderTarget)?;
        colors.push(blend_color(blend, slot, view));
        color_slots |= 1 << slot;
        writes.push((key, false));
    }
    let bias = raster.DepthBias != 0 || raster.SlopeScaledDepthBias != 0.;
    let depth = snapshot
        .view(ViewSlot::DSV)
        .map(|bound| {
            let (view, key) = image_view(env, bound.pointer, ViewKind::DepthStencil)?;
            writes.push((key, false));
            Ok::<_, Error>(depth_stencil(
                depth,
                view,
                bound.desc.writable_aspects(),
                bias,
            ))
        })
        .transpose()?;
    let vp = snapshot.rs.viewports[0];
    let raster_stream = raster_stream.filter(|_| {
        snapshot.rs.viewport_count != 0
            && vp.Width > 0.
            && vp.Height > 0.
            && (pixel != 0 || depth.is_some())
    });
    let (extent, samples) = if raster_stream.is_none() {
        (extent_rect([1, 1, 1]), 1)
    } else {
        match colors
            .first()
            .map(|color| &color.view)
            .or_else(|| depth.as_ref().map(|depth| &depth.view))
        {
            Some(attachment) => (view_rect(attachment), attachment.image().desc().samples),
            None => (
                extent_rect([
                    (vp.TopLeftX + vp.Width).ceil() as u32,
                    (vp.TopLeftY + vp.Height).ceil() as u32,
                    1,
                ]),
                1,
            ),
        }
    };
    let mut scissor = extent;
    if raster_stream.is_some() && raster.ScissorEnable != 0 {
        let raw = snapshot.rs.scissors[0];
        scissor = Rect {
            x: raw.left.max(0) as u32,
            y: raw.top.max(0) as u32,
            w: raw.right.saturating_sub(raw.left.max(0)).max(0) as u32,
            h: raw.bottom.saturating_sub(raw.top.max(0)).max(0) as u32,
        }
        .clipped(extent);
    }
    let raster_stream = raster_stream.filter(|_| !scissor.empty());
    if raster_stream.is_none() {
        if so_declarations.is_empty() {
            return Ok(());
        }
        writes.clear();
        scissor = extent;
    }
    let mut stream_targets = Vec::new();
    let mut stream_output = Vec::new();
    for (pointer, declaration) in so_declarations {
        let desc = description(env, pointer)?;
        let Resource::Buffer(buffer) = desc else {
            unreachable!()
        };
        let Backing::Buffer(native) = backing(env, pointer, desc)? else {
            unreachable!()
        };
        stream_targets.push((
            BufferRange {
                buffer: native,
                range: 0..u64::from(buffer.size()),
            },
            stream_cursor(env, pointer)?,
        ));
        writes.push((key(env, pointer, 0), false));
        stream_output.push(declaration);
    }
    let alpha_to_coverage = blend.is_some_and(|blend| blend.AlphaToCoverageEnable != 0);
    let mut reads = Vec::new();
    let mut capture = IaCapture::default();
    let (index, vertices) = if indexed {
        let strip = matches!(
            topology,
            p::Topology::LineStrip | p::Topology::TriangleStrip
        );
        let index = ensure_index(env, &snapshot, &mut args, strip, &mut capture)?;
        reads.push(key(env, snapshot.ia.indices.buffer, 0));
        (Some(index.binding), index.vertices)
    } else if let DrawRange::Direct { vertices, .. } = &args {
        (None, Some(vertices.clone()))
    } else {
        (None, None)
    };
    let (
        vertex_shader,
        pixel_shader,
        mut vertex_bindings,
        fragment_bindings,
        geometry_bindings,
        expansion,
        captures,
        fetches,
    ) = {
        let caps = env
            .ext
            .get::<State>()
            .execution
            .as_ref()
            .ok_or(Error::Device)?
            .gpu
            .device()
            .caps();
        let vertex_shader =
            bound_shader(env, &snapshot, Stage::Vertex).ok_or(Error::InvalidArgument)?;
        let pixel_shader = raster_stream.and_then(|_| bound_shader(env, &snapshot, Stage::Pixel));
        let reflection = vertex_shader.reflection();
        let pixel_reflection = pixel_shader.map(|shader| (u64::from(pixel), shader.reflection()));
        let input_layout = context::owned(env, layout).and_then(Object::input_layout);
        let mut fetches = Vec::new();
        let mut captures: [Option<Range<u32>>; STREAMS] = std::array::from_fn(|_| None);
        for input in &reflection.signatures.input {
            if input.system_value != SystemValue::None {
                continue;
            }
            let entry = input_layout
                .and_then(|layout| layout.element(input.semantic.as_bytes(), input.semantic_index));
            let Some(entry) = entry else { continue };
            let bound = snapshot.ia.vertices[entry.slot];
            if bound.buffer == 0 {
                continue;
            }
            let fetch = VertexFetch {
                register: input.register,
                mask: input.mask,
                stream: entry.slot as u32,
                offset: bound
                    .offset
                    .checked_add(entry.offset)
                    .ok_or(Error::InvalidArgument)?,
                stride: bound.stride,
                step: match entry.rate {
                    InputRate::Vertex => InputStep::Vertex,
                    InputRate::Instance(rate) => InputStep::Instance(rate),
                },
                format: vertex_format(entry.format)?,
            };
            let desc = description(env, bound.buffer)?;
            let Resource::Buffer(buffer) = desc else {
                return Err(Error::InvalidArgument);
            };
            if desc.usage() == Usage::Dynamic {
                let range = vertex_capture(fetch, buffer.size(), &args, vertices.as_ref());
                if !range.is_empty() {
                    let captured = captures[entry.slot].get_or_insert_with(|| range.clone());
                    captured.start = captured.start.min(range.start);
                    captured.end = captured.end.max(range.end);
                }
            }
            fetches.push(fetch);
        }
        for fetch in &mut fetches {
            if let Some(range) = &captures[fetch.stream as usize] {
                fetch.offset = fetch.offset.wrapping_sub(range.start);
            }
        }
        let geometry_shader = bound_shader(env, &snapshot, Stage::Geometry)
            .filter(|shader| shader.module().stage == crate::shader::ir::Stage::Geometry);
        let producer = geometry_shader.map_or(reflection, |shader| shader.reflection());
        let clip_components = producer
            .signatures
            .output
            .iter()
            .filter(|element| element.system_value == SystemValue::ClipDistance)
            .map(|element| element.mask.count_ones() as u8)
            .sum();
        let facts = DrawFacts {
            fragment: pixel_reflection,
            clip_components,
            producer_identity: Some(u64::from(if geometry == 0 { vertex } else { geometry })),
            alpha_to_coverage,
            wireframe: raster.FillMode == 2,
            geometry: geometry_shader.is_some(),
            stream_output: !stream_output.is_empty(),
            rasterization_disabled: raster_stream.is_none(),
            ..Default::default()
        };
        let variant = Variant::select(reflection, &facts, caps);
        let sampling = Sampling::new(env, &snapshot, Stage::Vertex, reflection);
        let views = ShaderViews::new(&snapshot, Stage::Vertex, reflection)?;
        let options = Options {
            raster_samples: samples,
            ..views.options(sampling.options())
        };
        let expanded = if variant.pulled() || input_vertices > input_topology.vertices() {
            let DrawRange::Direct {
                vertices,
                instances,
                base_vertex,
            } = &args
            else {
                return Err(Error::InvalidArgument);
            };
            let assembly = expand::Assembly {
                first: vertices.start,
                count: vertices.len() as u32,
                base_vertex: *base_vertex,
                first_instance: instances.start,
                index: index.as_ref().map(|(buffer, format)| expand::IndexInput {
                    offset: buffer.range.start as u32,
                    wide: *format == IndexFormat::U32,
                }),
                step,
                flip_winding,
            };
            let primitives = assembly
                .primitives(input_vertices)
                .checked_mul(instances.len() as u32)
                .ok_or(Error::InvalidArgument)?;
            if primitives == 0 {
                return Ok(());
            }
            let geometry_variant =
                geometry_shader.map(|shader| Variant::select(shader.reflection(), &facts, caps));
            let geometry_sampling = geometry_shader
                .map(|shader| Sampling::new(env, &snapshot, Stage::Geometry, shader.reflection()));
            let geometry_views = geometry_shader
                .map(|shader| ShaderViews::new(&snapshot, Stage::Geometry, shader.reflection()))
                .transpose()?;
            let geometry_stage =
                geometry_shader
                    .zip(geometry_variant.as_ref())
                    .map(|(shader, variant)| expand::Shader {
                        ir: shader.module(),
                        variant,
                        options: geometry_views
                            .as_ref()
                            .unwrap()
                            .options(geometry_sampling.as_ref().unwrap().options()),
                    });
            let bins = [geometry_variant.as_ref().unwrap_or(&variant)];
            let programs = expand::Plan {
                source: expand::Source::InputAssembler {
                    shader: expand::Shader {
                        ir: vertex_shader.module(),
                        variant: &variant,
                        options,
                    },
                    fetches: &fetches,
                    assembly,
                },
                topology: input_topology,
                input_vertices,
                geometry: geometry_stage,
                raster: raster_stream.map(|stream| expand::Raster {
                    bins: &bins,
                    fragment: pixel_reflection.map(|(_, reflection)| reflection),
                    stream,
                    cull: match raster.CullMode {
                        1 => expand::Cull::None,
                        2 => expand::Cull::Front,
                        3 => expand::Cull::Back,
                        _ => unreachable!(),
                    },
                    front_ccw: raster.FrontCounterClockwise != 0,
                    depth_clip: raster.DepthClipEnable != 0,
                }),
                stream_output: &stream_output,
            }
            .emit()
            .map_err(shader_error)?;
            Some((programs, primitives))
        } else {
            None
        };
        let (pixel, pixel_code) = if let Some(shader) = pixel_shader {
            let views = ShaderViews::new(&snapshot, Stage::Pixel, shader.reflection())?;
            views.writes(env, &mut writes);
            let variant = Variant::select(
                shader.reflection(),
                &DrawFacts {
                    storage_formats: &views.storage_formats,
                    ..facts
                },
                caps,
            );
            let sampling = Sampling::new(env, &snapshot, Stage::Pixel, shader.reflection());
            let options = Options {
                raster_samples: samples,
                image_atomics: caps.features.contains(adv_gpu::Features::IMAGE_ATOMICS),
                ..views.options(sampling.options())
            };
            let code = if let Some((programs, _)) = &expanded {
                programs.fragment_code(shader.module(), &variant, producer, options)
            } else {
                wgsl::emit_variant(shader.module(), &variant, None, options)
            }
            .map_err(shader_error)?;
            (
                Some(modern_shader(&code, adv_gpu::shader::Stage::Fragment)?),
                Some(code),
            )
        } else {
            (None, None)
        };
        let vertex_code;
        let gs_reflection = geometry_shader.map(|shader| shader.reflection().clone());
        let vertex_reflection = reflection.clone();
        let (vertex, vertex_slots, gs_slots) = if let Some((programs, _)) = &expanded {
            (
                raster_stream
                    .map(|_| programs.vertex_shader(0))
                    .transpose()?,
                (
                    &vertex_reflection,
                    programs.source.bindings.as_slice(),
                    programs.source.counters.as_slice(),
                ),
                gs_reflection.as_ref().map(|reflection| {
                    (
                        reflection,
                        programs.primitive.bindings.as_slice(),
                        programs.primitive.counters.as_slice(),
                    )
                }),
            )
        } else {
            vertex_code = pulled::vertex(
                vertex_shader.module(),
                &variant,
                pixel_reflection.map(|(_, reflection)| reflection),
                options,
                &fetches,
                args.start(indexed),
            )
            .map_err(shader_error)?;
            (
                Some(modern_shader(&vertex_code, adv_gpu::shader::Stage::Vertex)?),
                bindings(&vertex_code),
                None,
            )
        };
        let vertex_bindings = slots_for(env, &snapshot, Stage::Vertex, vertex_slots, &mut reads)?;
        let geometry_bindings = gs_slots
            .map(|layout| slots_for(env, &snapshot, Stage::Geometry, layout, &mut reads))
            .transpose()?
            .unwrap_or_default();
        let fragment = pixel_code
            .as_ref()
            .map(|code| slots_for(env, &snapshot, Stage::Pixel, bindings(code), &mut reads))
            .transpose()?
            .unwrap_or_default();
        (
            vertex,
            pixel,
            vertex_bindings,
            fragment,
            geometry_bindings,
            expanded,
            captures,
            fetches,
        )
    };
    if let DrawRange::Indirect(args, resource) = &args {
        reads.push(*resource);
        set_slot(
            &mut vertex_bindings.storage,
            DRAW_ARGUMENTS_SLOT as usize,
            StorageBinding::Buffer(BufferRange {
                buffer: args.buffer.clone(),
                range: 0..args.buffer.desc().size,
            }),
        );
    }
    for fetch in fetches {
        let stream = fetch.stream as usize;
        let slot = VERTEX_STREAM_BASE as usize + stream;
        if vertex_bindings
            .storage
            .get(slot)
            .is_some_and(Option::is_some)
        {
            continue;
        }
        let pointer = snapshot.ia.vertices[stream].buffer;
        let buffer = capture_buffer(
            env,
            pointer,
            BufferUsage::STORAGE,
            captures[stream].clone().unwrap_or(0..0),
            &mut capture,
        )?;
        reads.push(key(env, pointer, 0));
        let range = 0..buffer.desc().size;
        set_slot(
            &mut vertex_bindings.storage,
            slot,
            StorageBinding::Buffer(BufferRange { buffer, range }),
        );
    }
    let (vertices, primitives) = match &args {
        DrawRange::Direct {
            vertices,
            instances,
            ..
        } => {
            let count = vertices.len() as u64;
            let instances = instances.len() as u64;
            let primitives = match topology {
                p::Topology::Points => count,
                p::Topology::Lines => count / 2,
                p::Topology::LineStrip => count.saturating_sub(1),
                p::Topology::Triangles => count / 3,
                p::Topology::TriangleStrip => count.saturating_sub(2),
                _ => unreachable!(),
            };
            (count * instances, primitives * instances)
        }
        DrawRange::Indirect(..) => (0, 0),
    };
    let (occlusion, statistics) = queries::draw_slots(env, vertices, primitives)?;
    let predicate = queries::predicate(
        env,
        snapshot.predication.predicate,
        snapshot.predication.value,
    );
    let indices = index.as_ref().map(|(index, _)| BufferRange {
        buffer: index.buffer.clone(),
        range: 0..index.buffer.desc().size,
    });
    let draw = vertex_shader.map(|vertex| ModernDraw {
        state_ids: [
            layout,
            snapshot.rs.state,
            snapshot.om.blend,
            snapshot.om.depth_stencil,
        ]
        .map(u64::from),
        vertex,
        fragment: pixel_shader,
        colors: colors.into_iter().collect(),
        color_slots: Some(color_slots),
        depth,
        samples,
        alpha_to_coverage,
        primitive: p::Primitive {
            topology,
            cull: match raster.CullMode {
                1 => p::Cull::None,
                2 => p::Cull::Front,
                3 => p::Cull::Back,
                _ => unreachable!(),
            },
            front_ccw: raster.FrontCounterClockwise != 0,
            wireframe: raster.FillMode == 2,
            unclipped_depth: raster.DepthClipEnable == 0,
        },
        area: extent.into(),
        viewport: [
            vp.TopLeftX,
            vp.TopLeftY,
            vp.Width,
            vp.Height,
            vp.MinDepth,
            vp.MaxDepth,
        ],
        scissor: scissor.into(),
        stencil_reference: snapshot.om.stencil_reference,
        blend_constant: snapshot.om.blend_factor,
        depth_bias: [
            raster.DepthBias as f32,
            raster.SlopeScaledDepthBias,
            raster.DepthBiasClamp,
        ],
        arguments: args.arguments(),
        index,
        occlusion,
        predicate,
    });
    let execution = execution(env.ext)?;
    let command = if let Some((programs, primitives)) = expansion {
        Command::Expand(Box::new(
            programs.command(
                &execution.gpu,
                expand::Inputs {
                    primitives,
                    vertices: None,
                    indices,
                    vertex: vertex_bindings,
                    geometry: geometry_bindings,
                    statistics: None,
                    stream_output: stream_targets,
                },
                draw.map(|draw| expand::BinDraw {
                    draw,
                    fragment: fragment_bindings.clone(),
                    statistics: statistics.clone(),
                })
                .into_iter()
                .collect(),
            )?,
        ))
    } else {
        Command::Draw {
            draw: Box::new(draw.unwrap()),
            vertex: vertex_bindings,
            fragment: fragment_bindings,
            statistics,
        }
    };
    execution.recording.append(
        &mut execution.gpu,
        command,
        Effects {
            extent,
            bounds: scissor,
            key: capture.hash.digest128(),
            reads: &reads,
            writes: &writes,
            retained_bytes: capture.bytes,
        },
    )?;
    Ok(())
}

fn shader_error(error: crate::shader::ir::Error) -> Error {
    log::debug!("D3D11 shader variant: {error}");
    Error::InvalidArgument
}

fn modern_shader(
    code: &Translation,
    stage: adv_gpu::shader::Stage,
) -> Result<Arc<ModernShader>, Error> {
    let id = xxhash_rust::xxh3::xxh3_64(code.source.as_bytes());
    Ok(Arc::new(ModernShader::wgsl(
        id,
        &code.source,
        code.entry_point,
        stage,
    )?))
}

pub(in crate::d3d11::glue) fn dispatch(env: &mut Env<'_>) -> Result<(), Error> {
    compute(
        env,
        DispatchArguments::Direct([env.arg(1), env.arg(2), env.arg(3)]),
        None,
    )
}
pub(in crate::d3d11::glue) fn dispatch_indirect(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (arguments, resource) = indirect(env, 12)?;
    compute(env, DispatchArguments::Indirect(arguments), Some(resource))
}

fn compute(
    env: &mut Env<'_>,
    arguments: DispatchArguments,
    indirect_read: Option<ResourceKey>,
) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let snapshot = context::groups(env)?.clone();
    let Some(shader) = bound_shader(env, &snapshot, Stage::Compute) else {
        return Ok(());
    };
    let views = ShaderViews::new(&snapshot, Stage::Compute, shader.reflection())?;
    let mut writes = Vec::new();
    views.writes(env, &mut writes);
    let caps = env
        .ext
        .get::<State>()
        .execution
        .as_ref()
        .ok_or(Error::Device)?
        .gpu
        .device()
        .caps();
    let facts = DrawFacts {
        storage_formats: &views.storage_formats,
        ..Default::default()
    };
    let variant = Variant::select(shader.reflection(), &facts, caps);
    let sampling = Sampling::new(env, &snapshot, Stage::Compute, shader.reflection());
    let code = wgsl::emit_variant(
        shader.module(),
        &variant,
        None,
        Options {
            image_atomics: caps.features.contains(adv_gpu::Features::IMAGE_ATOMICS),
            ..views.options(sampling.options())
        },
    )
    .map_err(shader_error)?;
    let mut reads = Vec::new();
    reads.extend(indirect_read);
    let bindings = slots_for(env, &snapshot, Stage::Compute, bindings(&code), &mut reads)?;
    let execution = execution(env.ext)?;
    let pipeline = Arc::new(
        execution
            .gpu
            .prepare_dispatch(modern_shader(&code, adv_gpu::shader::Stage::Compute)?)?,
    );
    let extent = extent_rect([1, 1, 1]);
    execution.recording.append(
        &mut execution.gpu,
        Command::Dispatch {
            pipeline,
            bindings,
            arguments,
        },
        Effects {
            extent,
            bounds: extent,
            key: 0,
            reads: &reads,
            writes: &writes,
            retained_bytes: 0,
        },
    )?;
    Ok(())
}

pub(super) fn storage_format(format: u32) -> Result<naga::StorageFormat, Error> {
    use naga::StorageFormat::*;
    Ok(match format {
        2 => Rgba32Float,
        3 => Rgba32Uint,
        4 => Rgba32Sint,
        10 => Rgba16Float,
        11 => Rgba16Unorm,
        12 => Rgba16Uint,
        13 => Rgba16Snorm,
        14 => Rgba16Sint,
        16 => Rg32Float,
        17 => Rg32Uint,
        18 => Rg32Sint,
        24 => Rgb10a2Unorm,
        25 => Rgb10a2Uint,
        26 => Rg11b10Ufloat,
        28 => Rgba8Unorm,
        30 => Rgba8Uint,
        31 => Rgba8Snorm,
        32 => Rgba8Sint,
        34 => Rg16Float,
        35 => Rg16Unorm,
        36 => Rg16Uint,
        37 => Rg16Snorm,
        38 => Rg16Sint,
        41 => R32Float,
        42 => R32Uint,
        43 => R32Sint,
        49 => Rg8Unorm,
        50 => Rg8Uint,
        51 => Rg8Snorm,
        52 => Rg8Sint,
        54 => R16Float,
        56 => R16Unorm,
        57 => R16Uint,
        58 => R16Snorm,
        59 => R16Sint,
        61 => R8Unorm,
        62 => R8Uint,
        63 => R8Snorm,
        64 => R8Sint,
        87 => Bgra8Unorm,
        _ => return Err(Error::InvalidArgument),
    })
}

fn vertex_format(format: &crate::dxgi::formats::FormatLayout) -> Result<VertexFormat, Error> {
    use crate::dxgi::formats::Interpretation;
    let mut kinds = [VertexComponent::Uint; 4];
    for (index, (kind, bits)) in format
        .interpretations
        .iter()
        .zip(format.component_bits)
        .enumerate()
    {
        kinds[index] = match kind {
            Interpretation::Float => VertexComponent::Float,
            Interpretation::Sint => VertexComponent::Sint,
            Interpretation::Snorm => VertexComponent::Snorm,
            Interpretation::Uint => VertexComponent::Uint,
            Interpretation::Unorm => VertexComponent::Unorm,
            Interpretation::Fixed28 | Interpretation::Typeless | Interpretation::UnormSrgb
                if bits == 0 =>
            {
                VertexComponent::Uint
            }
            Interpretation::Fixed28 | Interpretation::Typeless | Interpretation::UnormSrgb => {
                return Err(Error::InvalidArgument);
            }
        };
    }
    Ok(VertexFormat {
        bytes: u32::from(format.aligned_unit_bytes),
        bits: format.component_bits,
        kinds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: GFX-REC-7
    #[test]
    fn dynamic_ia_captures_only_the_window_determined_at_admission() {
        let draw = DrawRange::direct(3, 5, 5, 3, 0).ok().unwrap();
        let fetch = VertexFetch {
            register: 0,
            mask: 15,
            stream: 0,
            offset: 12,
            stride: 24,
            step: InputStep::Vertex,
            format: VertexFormat {
                bytes: 16,
                bits: [32; 4],
                kinds: [VertexComponent::Float; 4],
            },
        };
        assert_eq!(vertex_capture(fetch, 1024, &draw, Some(&(5..8))), 132..196);
        assert_eq!(vertex_capture(fetch, 1024, &draw, None), 12..1024);
        let instanced = VertexFetch {
            step: InputStep::Instance(2),
            ..fetch
        };
        assert_eq!(vertex_capture(instanced, 1024, &draw, None), 84..148);
    }
}
