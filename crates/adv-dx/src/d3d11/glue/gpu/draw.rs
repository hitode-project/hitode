//! Lower immutable immediate-context bindings to the existing modern executor.
use super::{resource::storage_format, *};
use crate::{
    d3d11::{
        desc::{Aspects, Bind, InputRate, Misc, TextureKind},
        state::{RENDER_TARGETS, STREAMS, Snapshot, Stage, VIEWPORTS, ViewSlot},
    },
    shader::{
        analysis::Reflection,
        expand,
        ir::{BindingKind, Register, ResourceDimension, SamplerMode, SystemValue},
        variant::{Bin, DrawFacts, Variant},
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
    Indirect(BufferRange, ResourceKey, Option<u32>),
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
            Self::Indirect(buffer, ..) => DrawArguments::Indirect(buffer.clone()),
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
            Self::Indirect(args, ..) => DrawStart::Indirect {
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
    render(env, false, DrawRange::Indirect(args, resource, None))
}
pub(in crate::d3d11::glue) fn draw_indexed_indirect(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (args, resource) = indirect(env, 20)?;
    render(env, true, DrawRange::Indirect(args, resource, None))
}

pub(in crate::d3d11::glue) fn draw_auto(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let slot = context::groups(env)?.ia.vertices[0];
    let source = slot.buffer;
    if slot.stride == 0
        || !context::owned(env, source)
            .and_then(Object::resource)
            .is_some_and(|resource| resource.bind().contains(Bind::STREAM_OUTPUT))
    {
        return Ok(());
    }
    let cursor = stream_cursor(env, source)?;
    let key = key(env, source, 0);
    let exec = execution(env.ext)?;
    let (pipeline, slots, arguments) =
        expand::draw_auto(&exec.gpu, cursor, slot.offset, slot.stride)?;
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
    let Resource::Buffer(buffer) = description(env, source)? else {
        unreachable!()
    };
    render(
        env,
        false,
        DrawRange::Indirect(
            arguments,
            key,
            Some(buffer.size().saturating_sub(slot.offset) / slot.stride),
        ),
    )
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
    let capture_shadow = captures_shadow(env, binding.buffer, desc);
    if capture_shadow
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
    let native = if capture_shadow {
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

fn captures_shadow(env: &Env<'_>, pointer: u32, desc: Resource) -> bool {
    desc.usage() == Usage::Dynamic && !context::owned(env, pointer).unwrap().dynamic_shadow_stale()
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
    if captures_shadow(env, pointer, desc) {
        let bytes = shadow_bytes(env, pointer, range.clone())?;
        capture.record(pointer, &range, &bytes);
        let size = bytes.len();
        let buffer = resource::upload(
            &execution(env.ext)?.gpu,
            &bytes,
            usage | BufferUsage::COPY_SRC,
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
    if let Some(upload) = backing.upload_texture(
        &execution.gpu,
        texture,
        0,
        [0; 3],
        [width, height, depth],
        &bytes,
    )? {
        execution.gpu.transfer(upload)?;
    }
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
                let length = u64::from(length);
                let mut window = None;
                if constant.buffer != 0 {
                    let desc = description(env, constant.buffer)?;
                    let super::Resource::Buffer(buf) = desc else {
                        unreachable!()
                    };
                    let available = u64::from(buf.size())
                        .saturating_sub(offset)
                        .min(u64::from(selected));
                    if available != 0 {
                        let key = key(env, constant.buffer, 0);
                        reads.push(key);
                        let Backing::Constant(buffer) = backing(env, constant.buffer, desc)? else {
                            unreachable!()
                        };
                        window = Some(if available == length {
                            buffer.slice(offset..offset + length)
                        } else {
                            let execution = execution(env.ext)?;
                            let padded = execution.gpu.uniform_window(length as u32, |out| {
                                out[available as usize..].fill(0);
                                Ok::<_, Error>(())
                            })?;
                            execution.append(
                                Command::Transfer(ModernTransfer::CopyBufferWindow {
                                    source: BufferWindow::Uniform(
                                        buffer.slice(offset..offset + available),
                                    ),
                                    destination: BufferWindow::Uniform(padded.clone()),
                                }),
                                extent_rect([length as u32, 1, 1]),
                                Some(key),
                                key,
                            )?;
                            padded
                        });
                    }
                }
                let window = match window {
                    Some(window) => window,
                    None => {
                        let execution = execution(env.ext)?;
                        if execution.zero_constants.is_none() {
                            execution.zero_constants =
                                Some(execution.gpu.uniform_window(65536, |out| {
                                    out.fill(0);
                                    Ok::<_, Error>(())
                                })?);
                        }
                        execution.zero_constants.as_ref().unwrap().slice(0..length)
                    }
                };
                set_slot(&mut slots.uniforms, binding, BufferWindow::Uniform(window));
            }
            Sampler => {
                let mut desc = sampler_description(env, state.samplers[index]);
                let shader_bias = location.point_sampler || round_point_mip(desc);
                if location.point_sampler {
                    desc.Filter = 0;
                    desc.MinLOD = 0.;
                    desc.MaxLOD = f32::MAX;
                }
                if let Some(minimum) = location.sampler_min_lod_bits.map(f32::from_bits) {
                    desc.MinLOD = desc.MinLOD.max(minimum);
                    desc.MaxLOD = desc.MaxLOD.max(desc.MinLOD);
                }
                let mode = if location.point_sampler {
                    BindingKind::Sampler(SamplerMode::Default)
                } else {
                    reflection
                        .bindings
                        .iter()
                        .find(|binding| {
                            binding.register == Register::Sampler && binding.slot == location.slot
                        })
                        .unwrap()
                        .kind
                };
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
                    super::Resource::Texture(texture)
                        if captures_shadow(env, view.resource, desc) =>
                    {
                        capture_texture(env, view.resource, desc, texture)?
                    }
                    super::Resource::Buffer(_) if desc.usage() == Usage::Dynamic => {
                        Location::new(env, view.resource, 0)?.acquire(env, true)?
                    }
                    _ => backing(env, view.resource, desc)?,
                };
                match (backing, desc) {
                    (Backing::Image(image), super::Resource::Texture(texture))
                        if location.register == UnorderedAccessView
                            && resource::storage_uses_buffer(
                                execution(env.ext)?.gpu.device().caps(),
                                view.desc.format(),
                                reflection
                                    .atomic_bindings
                                    .contains(&(location.register, location.slot)),
                            ) =>
                    {
                        let range = view.desc.subresources();
                        reads.extend(range.layers.clone().map(|layer| {
                            key(
                                env,
                                view.resource,
                                layer * texture.levels() + range.mips.start,
                            )
                        }));
                        let alias = resource::LinearStorage::new(texture, &view.desc).alias(
                            &execution(env.ext)?.gpu,
                            texture,
                            &view.desc,
                            &image,
                        )?;
                        set_slot(
                            &mut slots.storage,
                            binding,
                            StorageBinding::ImageAlias(alias),
                        );
                    }
                    (backing @ (Backing::Image(_) | Backing::Blocks(_)), _) => {
                        let view = backing.view(&execution(env.ext)?.gpu, &view.desc)?;
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
    storage_extents: Vec<(u32, [u32; 4])>,
    storage_slices: Vec<(u32, std::ops::Range<u32>)>,
    alpha_uavs: u32,
    buffer_formats: Vec<(u32, naga::StorageFormat)>,
    rgb_buffers: Vec<u32>,
    buffer_elements: Vec<(Register, u32, u32)>,
    writable: Vec<(u32, u32)>,
}

impl ShaderViews {
    fn new(
        env: &Env<'_>,
        snapshot: &Snapshot,
        stage: Stage,
        reflection: &Reflection,
    ) -> Result<Self, Error> {
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
                let range = view.desc.subresources();
                if let super::Resource::Texture(texture) = description(env, view.resource)? {
                    views.writable.extend(
                        range.layers.clone().map(|layer| {
                            (view.resource, layer * texture.levels() + range.mips.start)
                        }),
                    );
                    let atomic = reflection
                        .atomic_bindings
                        .contains(&(binding.register, binding.slot));
                    if resource::storage_uses_buffer(
                        caps(env.ext).ok_or(Error::Device)?,
                        view.desc.format(),
                        atomic,
                    ) {
                        let extent = resource::LinearStorage::new(texture, &view.desc).extent;
                        views.storage_extents.push((binding.slot, extent));
                    } else if texture.kind() == TextureKind::D3 {
                        views
                            .storage_slices
                            .push((binding.slot, view.desc.elements()));
                    }
                } else {
                    views.writable.push((view.resource, 0));
                }
            }
            if let BindingKind::Typed { dimension, .. } = binding.kind {
                if writable {
                    views
                        .storage_formats
                        .push((binding.slot, storage_format(view.desc.format())?));
                    if view.desc.format() == 65 {
                        views.alpha_uavs |= 1 << binding.slot;
                    }
                } else if dimension == ResourceDimension::Buffer {
                    let (format, rgb) = resource::buffer_format(view.desc.format())?;
                    views.buffer_formats.push((binding.slot, format));
                    if rgb {
                        views.rgb_buffers.push(binding.slot);
                    }
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
            storage_extents: &self.storage_extents,
            storage_slices: &self.storage_slices,
            alpha_uavs: self.alpha_uavs,
            buffer_formats: &self.buffer_formats,
            rgb_buffers: &self.rgb_buffers,
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
    manual_filter: Vec<u32>,
    manual_samplers: Vec<BorderSampler>,
}

impl Sampling {
    fn new(env: &Env<'_>, snapshot: &Snapshot, stage: Stage, reflection: &Reflection) -> Self {
        use crate::dxgi::formats::{Channel, Interpretation};
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
            let filter = BorderSampler {
                slot,
                axes: [sampler.AddressU, sampler.AddressV, sampler.AddressW]
                    .map(|address| address == 4),
                color: sampler.BorderColor,
                linear: [0x10, 0x4, 0x1].map(|mask| sampler.Filter & mask != 0),
                lod,
                comparison: sampler.ComparisonFunc,
            };
            state.manual_samplers.push(filter);
            if custom_border(sampler) {
                state.border_samplers.push(filter);
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
            if resource::unfilterable_view(caps(env.ext).unwrap(), view.desc.format(), depth) {
                state.manual_filter.push(binding.slot);
            }
            if depth {
                state.depths.push(binding.slot);
            }
            if view.desc.format() == 46 {
                state.depths24.push(binding.slot);
            }
            let format = crate::dxgi::formats::format_layout(view.desc.format());
            let lanes = |format: &crate::dxgi::formats::FormatLayout, lane: usize| {
                let channel = [Channel::R, Channel::G, Channel::B, Channel::A][lane];
                format.channels.iter().position(|&value| value == channel)
            };
            if !depth
                && let Some(format) = format
                && format.alignment == [1; 3]
            {
                let widths = std::array::from_fn(|lane| {
                    lanes(format, lane).map_or(0, |component| {
                        let bits = format.component_bits[component];
                        if format.interpretations[component] == Interpretation::Unorm && bits <= 16
                        {
                            bits
                        } else {
                            0
                        }
                    })
                });
                if widths != [0; 4] {
                    state.unorm.push((binding.slot, widths));
                }
            } else if view.desc.format() == 98 {
                state.unorm.push((binding.slot, [8; 4]));
            }
            if (!state.border_samplers.is_empty() || state.manual_filter.contains(&binding.slot))
                && let Some(format) = format
            {
                let channels = std::array::from_fn(|lane| {
                    lanes(format, lane).map_or(BorderChannel::Missing, |component| {
                        let bits = format.component_bits[component];
                        match format.interpretations[component] {
                            _ if bits == 0 => BorderChannel::Missing,
                            Interpretation::Unorm => BorderChannel::Unorm(bits),
                            Interpretation::Snorm => BorderChannel::Snorm(bits),
                            _ => BorderChannel::Float,
                        }
                    })
                });
                state.border_textures.push(BorderTexture {
                    slot: binding.slot,
                    channels,
                });
            }
            if minimum > 0. {
                let format = format.unwrap();
                let default_alpha = if format.channels.contains(&Channel::A) {
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
            manual_filter: &self.manual_filter,
            manual_samplers: &self.manual_samplers,
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
fn blend_color(
    desc: Option<D3D11_BLEND_DESC1>,
    slot: usize,
    view: ImageView,
    format: u32,
) -> ModernColor {
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
                let mut rgb = blend_component(target.SrcBlend, target.DestBlend, target.BlendOp);
                let alpha = blend_component(
                    target.SrcBlendAlpha,
                    target.DestBlendAlpha,
                    target.BlendOpAlpha,
                );
                if format == 65 {
                    let alpha_to_red = |factor| match factor {
                        p::Factor::DstAlpha => p::Factor::Dst,
                        p::Factor::OneMinusDstAlpha => p::Factor::OneMinusDst,
                        p::Factor::SrcAlphaSaturated => p::Factor::One,
                        factor => factor,
                    };
                    rgb = p::BlendComponent {
                        src: alpha_to_red(alpha.src),
                        dst: alpha_to_red(alpha.dst),
                        op: alpha.op,
                    };
                } else if matches!(format, 88 | 93) {
                    let opaque = |factor| match factor {
                        p::Factor::DstAlpha => p::Factor::One,
                        p::Factor::OneMinusDstAlpha | p::Factor::SrcAlphaSaturated => {
                            p::Factor::Zero
                        }
                        factor => factor,
                    };
                    rgb.src = opaque(rgb.src);
                    rgb.dst = opaque(rgb.dst);
                }
                (rgb, alpha)
            }),
        write_mask: target.map_or(if format == 65 { 1 } else { 15 }, |target| {
            if format == 65 {
                target.RenderTargetWriteMask >> 3 & 1
            } else {
                target.RenderTargetWriteMask
            }
        }),
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
    let (raster_stream, so_declarations, declared_streams): (_, Vec<_>, _) = if let Some(so) = so {
        let missing = so.targets.iter().zip(snapshot.so.buffers).fold(
            0,
            |mask, (target, buffer)| match target {
                Some(target) if buffer == 0 => mask | (1u32 << target.stream),
                _ => mask,
            },
        );
        let targets = so
            .targets
            .iter()
            .zip(snapshot.so.buffers)
            .filter_map(|(target, buffer)| {
                let target = target
                    .as_ref()
                    .filter(|target| missing & (1 << target.stream) == 0)?;
                Some((
                    buffer,
                    expand::StreamOutput {
                        stream: target.stream,
                        elements: target.elements.clone(),
                    },
                ))
            })
            .collect();
        let declared = so
            .targets
            .iter()
            .flatten()
            .fold(0, |mask, target| mask | (1u32 << target.stream));
        (so.rasterized, targets, declared)
    } else {
        (Some(0), Vec::new(), 0)
    };
    let stream_statistics = if declared_streams == 0 {
        None
    } else {
        queries::stream_queries(env)?
    };
    let counted_streams = if stream_statistics.is_some() {
        declared_streams
    } else {
        0
    };
    let statistics_active = queries::active_statistics(env)?;
    let bound_predicate = queries::predicate(env, snapshot.predication.predicate);
    let predicate_value = snapshot.predication.value;
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
    let tessellation = if (33..=64).contains(&snapshot.ia.topology) {
        let Some(hull) = bound_shader(env, &snapshot, Stage::Hull) else {
            return Ok(());
        };
        let Some(domain) = bound_shader(env, &snapshot, Stage::Domain) else {
            return Ok(());
        };
        let layout =
            expand::Tessellation::new(hull.module(), domain.module()).map_err(shader_error)?;
        if layout.input_points != snapshot.ia.topology - 32 {
            return Err(Error::InvalidArgument);
        }
        Some(layout)
    } else {
        None
    };
    let (topology, input_topology, input_vertices, step, flip_winding) = match snapshot.ia.topology
    {
        0 => return Ok(()),
        33..=64 => {
            let layout = tessellation.unwrap();
            let topology = match layout.topology {
                expand::Topology::Point => p::Topology::Points,
                expand::Topology::Line => p::Topology::Lines,
                expand::Topology::Triangle => p::Topology::Triangles,
            };
            (
                topology,
                layout.topology,
                layout.topology.vertices(),
                layout.input_points,
                false,
            )
        }
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
    let mut alpha_targets = 0;
    let mut writes = Vec::new();
    for slot in 0..RENDER_TARGETS {
        let Some(bound) = snapshot.view(ViewSlot::rtv(slot).unwrap()) else {
            continue;
        };
        let (view, key) = image_view(env, bound.pointer, ViewKind::RenderTarget)?;
        colors.push(blend_color(blend, slot, view, bound.desc.format()));
        if bound.desc.format() == 65 {
            alpha_targets |= 1 << slot;
        }
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
    let (viewport_index, layer_index) = raster_stream
        .and_then(|stream| {
            let shader = bound_shader(env, &snapshot, Stage::Geometry)?;
            (shader.module().stage == crate::shader::ir::Stage::Geometry).then(|| {
                let writes_index =
                    |system_value| {
                        shader.reflection().signatures.output.iter().any(|entry| {
                            entry.stream == stream && entry.system_value == system_value
                        })
                    };
                (
                    writes_index(SystemValue::ViewportArrayIndex),
                    writes_index(SystemValue::RenderTargetArrayIndex),
                )
            })
        })
        .unwrap_or_default();
    let raster_stream = raster_stream.filter(|_| {
        snapshot.rs.viewport_count != 0
            && (pixel != 0
                || depth.as_ref().is_some_and(|depth| {
                    depth.state.depth_compare != p::Compare::Always
                        || depth.state.depth_write
                        || depth.state.stencil.is_some()
                }))
    });
    let viewport_count = if viewport_index {
        snapshot.rs.viewport_count
    } else {
        snapshot.rs.viewport_count.min(1)
    };
    let viewports = &snapshot.rs.viewports[..viewport_count];
    let (extent, samples) = if raster_stream.is_none() {
        (extent_rect([1, 1, 1]), 1)
    } else {
        match colors
            .first()
            .map(|color| &color.view)
            .or_else(|| depth.as_ref().map(|depth| &depth.view))
        {
            Some(attachment) => (view_rect(attachment), attachment.image().desc().samples),
            None => {
                let extent = viewports.iter().fold(Rect::default(), |area, vp| {
                    area.union(extent_rect([
                        (vp.TopLeftX + vp.Width).ceil() as u32,
                        (vp.TopLeftY + vp.Height).ceil() as u32,
                        1,
                    ]))
                });
                (extent, 1)
            }
        }
    };
    let mut layer_count = 1;
    if layer_index {
        layer_count = (0..RENDER_TARGETS)
            .filter_map(|slot| snapshot.view(ViewSlot::rtv(slot).unwrap()))
            .chain(snapshot.view(ViewSlot::DSV))
            .map(|bound| bound.desc.attachment_layers().len())
            .min()
            .unwrap_or(1);
    }
    let mut raster_bins = Vec::new();
    let mut scissor = Rect::default();
    if raster_stream.is_some() {
        for (index, vp) in viewports.iter().enumerate() {
            let bounds = if raster.ScissorEnable == 0 {
                extent
            } else if index < snapshot.rs.scissor_count {
                let raw = snapshot.rs.scissors[index];
                Rect {
                    x: raw.left.max(0) as u32,
                    y: raw.top.max(0) as u32,
                    w: raw.right.saturating_sub(raw.left.max(0)).max(0) as u32,
                    h: raw.bottom.saturating_sub(raw.top.max(0)).max(0) as u32,
                }
                .clipped(extent)
            } else {
                Rect::default()
            };
            if vp.Width <= 0. || vp.Height <= 0. || bounds.empty() {
                continue;
            }
            scissor = scissor.union(bounds);
            let viewport = [
                vp.TopLeftX,
                vp.TopLeftY,
                vp.Width,
                vp.Height,
                vp.MinDepth,
                vp.MaxDepth,
            ];
            for layer in 0..layer_count {
                raster_bins.push((
                    Bin {
                        viewport: index as u16,
                        first_layer: layer as u16,
                        layers: 1,
                    },
                    viewport,
                    bounds,
                ));
            }
        }
    }
    let raster_stream = raster_stream.filter(|_| !raster_bins.is_empty());
    if raster_stream.is_none() {
        if so_declarations.is_empty() && counted_streams == 0 && !statistics_active {
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
    let indexed_strip = indexed && step < input_vertices && tessellation.is_none();
    // Keep the unmasked pipeline and ignore bits beyond the attachment's sample count.
    let active_samples = u32::MAX >> (u32::BITS - samples);
    let sample_mask = if raster_stream.is_some() {
        snapshot.om.sample_mask | !active_samples
    } else {
        u32::MAX
    };
    let metal_mask = if cfg!(target_vendor = "apple") {
        sample_mask
    } else {
        u32::MAX
    };
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
        hull_bindings,
        domain_bindings,
        expansion,
        captures,
        fetches,
        alpha_to_coverage,
        inexact_sample_mask,
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
        let alpha_to_coverage = blend.is_some_and(|blend| blend.AlphaToCoverageEnable != 0)
            && pixel_shader.is_some_and(|shader| {
                !shader
                    .reflection()
                    .signatures
                    .output
                    .iter()
                    .any(|e| e.system_value == SystemValue::Coverage)
            });
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
            if captures_shadow(env, bound.buffer, desc) {
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
        let (hull_shader, domain_shader) = if tessellation.is_some() {
            (
                bound_shader(env, &snapshot, Stage::Hull),
                bound_shader(env, &snapshot, Stage::Domain),
            )
        } else {
            (None, None)
        };
        let producer = geometry_shader
            .or(domain_shader)
            .map_or(reflection, |shader| shader.reflection());
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
            emulated_sample_mask: metal_mask != u32::MAX,
            wireframe: raster.FillMode == 2,
            geometry: geometry_shader.is_some(),
            tessellated: tessellation.is_some(),
            stream_output: !stream_output.is_empty() || counted_streams != 0,
            rasterization_disabled: raster_stream.is_none(),
            statistics: statistics_active,
            software_statistics: pixel_shader.is_none(),
            viewport_count: if viewport_index { VIEWPORTS as u32 } else { 1 },
            layer_count: layer_count as u32,
            ..Default::default()
        };
        let variant = Variant::select(reflection, &facts, caps);
        let expand = variant.pulled()
            || input_vertices > input_topology.vertices()
            || raster_stream.is_none()
            || (statistics_active && indexed_strip);
        let facts = DrawFacts {
            software_statistics: facts.software_statistics || expand,
            ..facts
        };
        let variant = Variant::select(reflection, &facts, caps);
        let sampling = Sampling::new(env, &snapshot, Stage::Vertex, reflection);
        let views = ShaderViews::new(env, &snapshot, Stage::Vertex, reflection)?;
        let options = Options {
            raster_samples: samples,
            ..views.options(sampling.options())
        };
        let expanded = if expand {
            let (first, count, base_vertex, first_instance, instance_count, indirect) = match &args
            {
                DrawRange::Direct {
                    vertices,
                    instances,
                    base_vertex,
                } => (
                    vertices.start,
                    vertices.len() as u32,
                    *base_vertex,
                    instances.start,
                    instances.len() as u32,
                    None,
                ),
                DrawRange::Indirect(arguments, _, automatic) => {
                    let (vertices, instances) = if let Some(vertices) = automatic {
                        (*vertices, 1)
                    } else {
                        let index_extent = index.as_ref().map(|(buffer, format)| {
                            let stride = if *format == IndexFormat::U32 { 4 } else { 2 };
                            (buffer.range.end - buffer.range.start) as u32 / stride
                        });
                        let Some(limits) = indirect_bounds(env, &snapshot, &fetches, index_extent)?
                        else {
                            let direct = read_indirect_draw(env, arguments, indexed)?;
                            return render(env, indexed, direct);
                        };
                        limits
                    };
                    (
                        0,
                        vertices,
                        0,
                        0,
                        instances,
                        Some(expand::Indirect {
                            first_word: (arguments.range.start / 4) as u32,
                            instances,
                        }),
                    )
                }
            };
            let assembly = expand::Assembly {
                count_statistics: statistics_active,
                predicate: bound_predicate.as_ref().map(|_| !predicate_value),
                first,
                count,
                base_vertex,
                first_instance,
                indirect,
                index: index.as_ref().map(|(buffer, format)| expand::IndexInput {
                    offset: buffer.range.start as u32,
                    wide: *format == IndexFormat::U32,
                }),
                step,
                flip_winding,
            };
            let primitives = assembly
                .primitives(tessellation.map_or(input_vertices, |layout| layout.input_points))
                .checked_mul(instance_count)
                .ok_or(Error::InvalidArgument)?;
            if primitives == 0 {
                return Ok(());
            }
            let mut compile_stage = |shader: Option<&crate::d3d11::desc::Shader>, stage| {
                shader
                    .map(|shader| {
                        let reflection = shader.reflection();
                        let views = ShaderViews::new(env, &snapshot, stage, reflection)?;
                        views.writes(env, &mut writes);
                        Ok::<_, Error>((
                            Variant::select(
                                reflection,
                                &DrawFacts {
                                    storage_formats: &views.storage_formats,
                                    ..facts
                                },
                                caps,
                            ),
                            Sampling::new(env, &snapshot, stage, reflection),
                            views,
                        ))
                    })
                    .transpose()
            };
            let geometry_options = compile_stage(geometry_shader, Stage::Geometry)?;
            let hull_options = compile_stage(hull_shader, Stage::Hull)?;
            let domain_options = compile_stage(domain_shader, Stage::Domain)?;
            let geometry_stage = geometry_shader.zip(geometry_options.as_ref()).map(
                |(shader, (variant, sampling, views))| expand::Shader {
                    ir: shader.module(),
                    variant,
                    options: views.options(sampling.options()),
                },
            );
            let vertex = expand::Shader {
                ir: vertex_shader.module(),
                variant: &variant,
                options,
            };
            let hs;
            let ds;
            let source = if let Some(layout) = tessellation {
                let (hs_variant, hs_sampling, hs_views) = hull_options.as_ref().unwrap();
                let (ds_variant, ds_sampling, ds_views) = domain_options.as_ref().unwrap();
                hs = expand::Shader {
                    ir: hull_shader.unwrap().module(),
                    variant: hs_variant,
                    options: hs_views.options(hs_sampling.options()),
                };
                ds = expand::Shader {
                    ir: domain_shader.unwrap().module(),
                    variant: ds_variant,
                    options: ds_views.options(ds_sampling.options()),
                };
                expand::Source::Tessellated {
                    vertex,
                    hull: &hs,
                    domain: &ds,
                    fetches: &fetches,
                    assembly,
                    layout,
                }
            } else {
                expand::Source::InputAssembler {
                    shader: vertex,
                    fetches: &fetches,
                    assembly,
                }
            };
            let variants: Vec<_> = raster_bins
                .iter()
                .map(|(bin, _, _)| {
                    Variant::select(producer, &DrawFacts { bin: *bin, ..facts }, caps)
                })
                .collect();
            let bins: Vec<_> = variants.iter().collect();
            let programs = expand::Plan {
                source,
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
                stream_statistics: counted_streams,
            }
            .emit()
            .map_err(shader_error)?;
            Some((programs, primitives))
        } else {
            None
        };
        let (pixel, pixel_code, inexact_sample_mask) = if let Some(shader) =
            pixel_shader.filter(|_| metal_mask & active_samples != 0)
        {
            let views = ShaderViews::new(env, &snapshot, Stage::Pixel, shader.reflection())?;
            views.writes(env, &mut writes);
            let variant = Variant::select(
                shader.reflection(),
                &DrawFacts {
                    storage_formats: &views.storage_formats,
                    ..facts
                },
                caps,
            );
            let reflection = shader.reflection();
            let inexact_sample_mask = metal_mask != u32::MAX
                && (variant.early_tests() && depth.is_some()
                    || samples > 1 && {
                        let writes_uav = reflection
                            .writes
                            .keys()
                            .any(|(register, _)| *register == Register::UnorderedAccessView);
                        let reads_coverage = reflection
                            .reads
                            .contains_key(&(Register::InputCoverageMask, Some(0)));
                        writes_uav
                            || reads_coverage && (metal_mask & active_samples).count_ones() > 1
                    });
            let sampling = Sampling::new(env, &snapshot, Stage::Pixel, shader.reflection());
            let options = Options {
                raster_samples: samples,
                raster_sample_mask: metal_mask,
                alpha_targets,
                image_atomics: true,
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
                inexact_sample_mask,
            )
        } else if metal_mask != u32::MAX {
            // Depth-only draws still need a mask output. An empty mask also
            // suppresses the guest shader's side effects.
            let source = format!(
                "@fragment fn fragment_main() -> @builtin(sample_mask) u32 {{ return {metal_mask}u; }}"
            );
            (
                Some(Arc::new(ModernShader::wgsl(
                    xxhash_rust::xxh3::xxh3_64(source.as_bytes()),
                    &source,
                    "fragment_main",
                    adv_gpu::shader::Stage::Fragment,
                )?)),
                None,
                false,
            )
        } else {
            (None, None, false)
        };
        let vertex_code;
        let gs_reflection = geometry_shader.map(|shader| shader.reflection().clone());
        let vertex_reflection = reflection.clone();
        let hull_reflection = hull_shader.map(|shader| shader.reflection().clone());
        let domain_reflection = domain_shader.map(|shader| shader.reflection().clone());
        let mut tessellation_bindings = None;
        let (vertex, vertex_slots, gs_slots) = if let Some((programs, _)) = &expanded {
            let vertex_source = if let Some((vertex, hull)) = programs.tessellation_stages() {
                tessellation_bindings = Some((
                    (
                        hull_reflection.as_ref().unwrap(),
                        hull.bindings.as_slice(),
                        hull.counters.as_slice(),
                    ),
                    (
                        domain_reflection.as_ref().unwrap(),
                        programs.source.bindings.as_slice(),
                        programs.source.counters.as_slice(),
                    ),
                ));
                vertex
            } else {
                &programs.source
            };
            (
                None,
                (
                    &vertex_reflection,
                    vertex_source.bindings.as_slice(),
                    vertex_source.counters.as_slice(),
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
        let (hull, domain) = if let Some((hull, domain)) = tessellation_bindings {
            (
                slots_for(env, &snapshot, Stage::Hull, hull, &mut reads)?,
                slots_for(env, &snapshot, Stage::Domain, domain, &mut reads)?,
            )
        } else {
            (SlotBindings::default(), SlotBindings::default())
        };
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
            hull,
            domain,
            expanded,
            captures,
            fetches,
            alpha_to_coverage && metal_mask & active_samples != 0,
            inexact_sample_mask,
        )
    };
    if let DrawRange::Indirect(args, resource, _) = &args {
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
    let (occlusion, statistics) =
        queries::draw_slots(env, raster_stream.is_some(), expansion.is_none())?;
    let gpu_statistics = if statistics_active && expansion.is_some() {
        let counters = queries::counter_buffer(
            &execution(env.ext)?.gpu,
            (expand::statistics::GPU_WORDS * 4) as u64,
        )?;
        queries::record_statistics(
            env,
            [0; 11],
            Some((
                counters.clone(),
                crate::d3d11::object::query::Sample::Expansion,
            )),
        )?;
        Some(counters)
    } else {
        None
    };
    let native_args = args.arguments();
    let gated_args = if let Some(buffer) = &bound_predicate
        && (expansion.is_none() || statistics_active && !indexed_strip)
    {
        let words = match &native_args {
            DrawArguments::Direct {
                vertices,
                instances,
                base_vertex,
            } => {
                let mut words = vec![
                    vertices.len() as u32,
                    instances.len() as u32,
                    vertices.start,
                ];
                if indexed {
                    words.push(*base_vertex as u32);
                }
                words.push(instances.start);
                queries::ArgumentWords::Direct(words)
            }
            DrawArguments::Indirect(arguments) => {
                queries::ArgumentWords::Indirect(arguments.clone())
            }
        };
        Some(queries::gate_arguments(
            env.ext,
            buffer,
            predicate_value,
            words,
            &reads,
        )?)
    } else {
        None
    };
    if statistics_active && !indexed_strip {
        use expand::statistics::{AssemblyTopology, Indirect, Rasterization};
        let width = tessellation.map_or(input_vertices, |layout| layout.input_points);
        let assembly = if step < width {
            AssemblyTopology::Strip(width, step)
        } else {
            AssemblyTopology::List(width)
        };
        let count_kind = Indirect::Draw {
            topology: assembly,
            gs_instances: 0,
            hull: tessellation.is_some(),
            rasterization: Rasterization::select(
                raster_stream.is_some() && expansion.is_none(),
                pixel != 0,
                depth.is_some(),
            ),
        };
        if let Some(arguments) = gated_args.as_ref().or(match &native_args {
            DrawArguments::Indirect(arguments) => Some(arguments),
            _ => None,
        }) {
            queries::count_indirect(env, arguments, count_kind, &reads)?;
        } else if let DrawRange::Direct {
            vertices,
            instances,
            ..
        } = &args
        {
            let count = vertices.len() as u32;
            let vertices = u64::from(count) * instances.len() as u64;
            let primitives = u64::from(assembly.primitives(count)) * instances.len() as u64;
            let mut counters = [0; 11];
            counters[0] = vertices;
            counters[1] = primitives;
            counters[2] = vertices;
            if raster_stream.is_some() && expansion.is_none() {
                counters[5] = primitives;
            }
            if tessellation.is_some() {
                counters[8] = primitives;
            }
            queries::record_statistics(env, counters, None)?;
        }
    }
    let indices = index.as_ref().map(|(index, _)| BufferRange {
        buffer: index.buffer.clone(),
        range: 0..index.buffer.desc().size,
    });
    let make_draw = |vertex, viewport, bounds: Rect, occlusion| ModernDraw {
        state_ids: [
            layout,
            snapshot.rs.state,
            snapshot.om.blend,
            snapshot.om.depth_stencil,
        ]
        .map(u64::from),
        vertex,
        fragment: pixel_shader.clone(),
        colors: colors.iter().cloned().collect(),
        color_slots: Some(color_slots),
        depth: depth.clone(),
        samples,
        sample_mask,
        inexact_sample_mask,
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
        viewport,
        scissor: bounds.into(),
        stencil_reference: snapshot.om.stencil_reference,
        blend_constant: snapshot.om.blend_factor,
        depth_bias: [
            raster.DepthBias as f32,
            raster.SlopeScaledDepthBias,
            raster.DepthBiasClamp,
        ],
        arguments: gated_args
            .clone()
            .map_or_else(|| native_args.clone(), DrawArguments::Indirect),
        index: index.clone(),
        occlusion,
        predicate: None,
    };
    let command = if let Some((programs, primitives)) = expansion {
        let mut bins = Vec::new();
        for (i, (bin, viewport, bounds)) in raster_bins.into_iter().enumerate() {
            let (occlusion, statistics) = if i == 0 {
                (occlusion.clone(), statistics.clone())
            } else {
                queries::draw_slots(env, true, false)?
            };
            let mut draw = make_draw(programs.vertex_shader(i)?, viewport, bounds, occlusion);
            if layer_count > 1 {
                let layer = u32::from(bin.first_layer);
                for (slot, color) in (0..RENDER_TARGETS)
                    .filter_map(|slot| snapshot.view(ViewSlot::rtv(slot).unwrap()))
                    .zip(&mut draw.colors)
                {
                    color.view = attachment_layer(&color.view, &slot.desc, layer)?;
                }
                if let Some(depth) = &mut draw.depth {
                    let slot = snapshot.view(ViewSlot::DSV).unwrap();
                    depth.view = attachment_layer(&depth.view, &slot.desc, layer)?;
                }
            }
            bins.push(expand::BinDraw {
                draw,
                fragment: fragment_bindings.clone(),
                statistics,
            });
        }
        let execution = execution(env.ext)?;
        Command::Expand(Box::new(programs.command(
            &execution.gpu,
            expand::Inputs {
                primitives,
                vertices: None,
                indices,
                vertex: vertex_bindings,
                geometry: geometry_bindings,
                hull: hull_bindings,
                domain: domain_bindings,
                indirect: match args {
                    DrawRange::Indirect(arguments, ..) => Some(BufferRange {
                        range: 0..arguments.buffer.desc().size,
                        buffer: arguments.buffer,
                    }),
                    _ => None,
                },
                statistics: gpu_statistics,
                predicate: bound_predicate,
                stream_output: stream_targets,
                stream_statistics,
            },
            bins,
        )?))
    } else {
        Command::Draw {
            draw: Box::new(make_draw(
                vertex_shader.unwrap(),
                raster_bins[0].1,
                raster_bins[0].2,
                occlusion,
            )),
            vertex: vertex_bindings,
            fragment: fragment_bindings,
            statistics,
        }
    };
    let execution = execution(env.ext)?;
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

pub(super) fn attachment_layer(
    view: &ImageView,
    desc: &View,
    layer: u32,
) -> Result<ImageView, Error> {
    let image = view.image();
    let range = desc.subresources();
    let layer = desc.attachment_layers().start + layer;
    Ok(image.create_view(&r::ViewDesc {
        format: (view.format() != image.desc().format).then_some(view.format()),
        dimension: matches!(
            image.desc().dimension,
            r::Dimension::Cube | r::Dimension::D3
        )
        .then_some(r::Dimension::D2),
        mips: range.mips.clone(),
        layers: layer..layer + 1,
        plane: 0,
    })?)
}

fn read_indirect_draw(
    env: &mut Env<'_>,
    arguments: &BufferRange,
    indexed: bool,
) -> Result<DrawRange, Error> {
    let exec = execution(env.ext)?;
    exec.barrier(Boundary::Map)?;
    let buffer = exec.gpu.device().create_buffer(&BufferDesc {
        size: arguments.range.end - arguments.range.start,
        memory: Memory::Readback,
        usage: BufferUsage::COPY_DST,
        label: "indirect draw argument readback",
    })?;
    exec.gpu.transfer(ModernTransfer::CopyBuffer {
        source: arguments.clone(),
        destination: buffer.clone(),
        offset: 0,
    })?;
    exec.gpu.completion()?.recv().map_err(|_| Error::Device)?;
    #[cfg(feature = "d3d9-counters")]
    exec.recording
        .count_indirect_readback(arguments.range.end - arguments.range.start);
    #[cfg(feature = "d3d9-counters")]
    exec.gpu
        .count_readback(arguments.range.end - arguments.range.start);
    // The completion marker follows the argument transfer.
    let words: &[u32] = bytemuck::cast_slice(unsafe { buffer.mapped().unwrap().as_ref() });
    let (base_vertex, first_instance) = if indexed {
        (words[3] as i32, words[4])
    } else {
        (0, words[3])
    };
    DrawRange::direct(words[0], words[2], words[1], first_instance, base_vertex)
}

fn indirect_bounds(
    env: &Env<'_>,
    snapshot: &Snapshot,
    fetches: &[VertexFetch],
    index_extent: Option<u32>,
) -> Result<Option<(u32, u32)>, Error> {
    let mut vertex_extent = None::<u32>;
    let mut instance_extent = None::<u32>;
    for fetch in fetches {
        if fetch.stride == 0 {
            continue;
        }
        let resource = description(env, snapshot.ia.vertices[fetch.stream as usize].buffer)?;
        let Resource::Buffer(buffer) = resource else {
            unreachable!()
        };
        let available = buffer.size().saturating_sub(fetch.offset);
        let records = if available < fetch.format.bytes {
            0
        } else {
            1 + (available - fetch.format.bytes) / fetch.stride
        };
        match fetch.step {
            InputStep::Vertex => {
                vertex_extent = Some(vertex_extent.map_or(records, |current| current.min(records)));
            }
            InputStep::Instance(rate) if rate != 0 => {
                let instances = records.saturating_mul(rate);
                instance_extent =
                    Some(instance_extent.map_or(instances, |current| current.min(instances)));
            }
            InputStep::Instance(_) => (),
        }
    }
    Ok(index_extent.or(vertex_extent).zip(instance_extent))
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
    let views = ShaderViews::new(env, &snapshot, Stage::Compute, shader.reflection())?;
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
            image_atomics: true,
            ..views.options(sampling.options())
        },
    )
    .map_err(shader_error)?;
    let mut reads = Vec::new();
    reads.extend(indirect_read);
    let threads = shader
        .reflection()
        .workgroup_size
        .unwrap()
        .into_iter()
        .product::<u32>();
    let bindings = slots_for(env, &snapshot, Stage::Compute, bindings(&code), &mut reads)?;
    let mut arguments = arguments;
    if let Some(buffer) = queries::predicate(env, snapshot.predication.predicate) {
        let words = match arguments {
            DispatchArguments::Direct(groups) => queries::ArgumentWords::Direct(groups.to_vec()),
            DispatchArguments::Indirect(arguments) => queries::ArgumentWords::Indirect(arguments),
        };
        arguments = DispatchArguments::Indirect(queries::gate_arguments(
            env.ext,
            &buffer,
            snapshot.predication.value,
            words,
            &reads,
        )?);
    }
    if queries::active_statistics(env)? {
        match &arguments {
            DispatchArguments::Direct(groups) => {
                let mut counters = [0; 11];
                counters[10] = groups
                    .iter()
                    .copied()
                    .chain([threads])
                    .map(u64::from)
                    .product();
                queries::record_statistics(env, counters, None)?;
            }
            DispatchArguments::Indirect(arguments) => queries::count_indirect(
                env,
                arguments,
                expand::statistics::Indirect::Dispatch {
                    threads_per_group: threads,
                },
                &reads,
            )?,
        }
    }
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

fn vertex_format(format: &crate::dxgi::formats::FormatLayout) -> Result<VertexFormat, Error> {
    use crate::dxgi::formats::{Channel, Interpretation};
    let mut widths = [0; 4];
    let mut offsets = [0; 4];
    let mut kinds = [VertexComponent::Uint; 4];
    let mut offset = 0;
    for ((channel, interpretation), bits) in format
        .channels
        .iter()
        .zip(format.interpretations)
        .zip(format.component_bits)
    {
        let lane = match channel {
            Channel::R => Some(0),
            Channel::G => Some(1),
            Channel::B => Some(2),
            Channel::A => Some(3),
            Channel::D | Channel::S | Channel::X => None,
        };
        if let Some(lane) = lane {
            widths[lane] = bits;
            offsets[lane] = offset;
            kinds[lane] = match interpretation {
                Interpretation::Float => VertexComponent::Float,
                Interpretation::Sint => VertexComponent::Sint,
                Interpretation::Snorm => VertexComponent::Snorm,
                Interpretation::Uint => VertexComponent::Uint,
                Interpretation::Unorm => VertexComponent::Unorm,
                Interpretation::Fixed28 | Interpretation::Typeless | Interpretation::UnormSrgb => {
                    return Err(Error::InvalidArgument);
                }
            };
        }
        offset += bits;
    }
    Ok(VertexFormat {
        bytes: u32::from(format.aligned_unit_bytes),
        bits: widths,
        kinds,
        offsets,
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
                offsets: [0, 32, 64, 96],
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
