//! Lower immutable immediate-context bindings to the existing modern executor.
use super::*;
use crate::{
    d3d11::{
        desc::{Aspects, InputRate, Misc},
        state::{Snapshot, Stage, ViewSlot},
    },
    shader::{
        ir::{BindingKind, Register, ResourceDimension, SamplerMode, SystemValue},
        variant::{DrawFacts, Variant},
        wgsl::{
            self, Options, Translation,
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

fn pipeline<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a PipelineState> {
    table(env).get(&env.sys.objects, pointer)?.pipeline()
}

fn ensure_index(
    env: &mut Env<'_>,
    state: &Snapshot,
    args: &DrawRange,
) -> Result<(BufferRange, IndexFormat), Error> {
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
    let backing = capture_buffer(env, binding.buffer, BufferUsage::INDEX)?;
    Ok((
        BufferRange {
            buffer: backing,
            range: u64::from(binding.offset)..u64::from(buffer.size()),
        },
        format,
    ))
}

fn capture_buffer(
    env: &mut Env<'_>,
    pointer: u32,
    usage: BufferUsage,
) -> Result<adv_gpu::Buffer, Error> {
    let desc = description(env, pointer)?;
    let Resource::Buffer(buffer) = desc else {
        return Err(Error::InvalidArgument);
    };
    if desc.usage() == Usage::Dynamic {
        let table = table(env);
        let object = table.get_mut(&mut env.sys.objects, pointer).unwrap();
        let pointer = object.mapping().unwrap().pointer();
        let bytes = array::<u8>(env, pointer, buffer.size())?;
        return resource::upload(
            &execution(env.ext)?.gpu,
            &bytes,
            usage,
            u64::from(buffer.size()).next_multiple_of(4),
        );
    }
    let Backing::Buffer(buffer) = backing(env, pointer, desc)? else {
        unreachable!()
    };
    Ok(buffer)
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
    shader: &Translation,
    reads: &mut Vec<ResourceKey>,
) -> Result<SlotBindings, Error> {
    let state = &snapshot.stages[stage as usize];
    let mut slots = SlotBindings::default();
    use Register::*;
    for location in &shader.locations {
        let index = location.slot as usize;
        let binding = location.binding as usize;
        match location.register {
            ConstantBuffer => {
                let constant = state.constants[index];
                let length = shader
                    .reflection
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
                let desc = match pipeline(env, state.samplers[index]) {
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
                };
                let mode = shader
                    .reflection
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.register == Register::Sampler && binding.slot == location.slot
                    })
                    .unwrap()
                    .kind;
                let sampler = execution(env.ext)?
                    .gpu
                    .device()
                    .sampler(&sampler_desc(desc, mode))?;
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
                        let unit = crate::dxgi::formats::format_layout(view.desc.format())
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
    for &(slot, binding) in &shader.counters {
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

fn set_slot<T>(slots: &mut Vec<Option<T>>, slot: usize, value: T) {
    if slots.len() <= slot {
        slots.resize_with(slot + 1, || None);
    }
    slots[slot] = Some(value);
}

fn sampler_desc(desc: D3D11_SAMPLER_DESC, mode: BindingKind) -> r::SamplerDesc {
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
    } else if desc.BorderColor[3] == 1. {
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
        lod_bias: (desc.MipLODBias * 256.).round() as i16,
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

fn render(env: &mut Env<'_>, indexed: bool, args: DrawRange) -> Result<(), Error> {
    if null_device(env)?
        || matches!(&args, DrawRange::Direct { vertices, instances, .. } if vertices.is_empty() || instances.is_empty())
    {
        return Ok(());
    }
    let snapshot = context::groups(env)?.clone();
    let vertex = snapshot.stages[Stage::Vertex as usize].shader;
    if vertex == 0 || snapshot.rs.viewport_count == 0 {
        return Ok(());
    }
    let pixel = snapshot.stages[Stage::Pixel as usize].shader;
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
    let topology = match snapshot.ia.topology {
        0 => return Ok(()),
        1 => p::Topology::Points,
        2 => p::Topology::Lines,
        3 => p::Topology::LineStrip,
        4 => p::Topology::Triangles,
        5 => p::Topology::TriangleStrip,
        // Adjacency and patch topologies require the excluded stages.
        10..=13 | 33..=64 => return Ok(()),
        _ => return Err(Error::InvalidArgument),
    };
    let mut colors = Vec::new();
    let mut writes = Vec::new();
    for slot in 0..super::super::super::state::RENDER_TARGETS {
        let Some(bound) = snapshot.view(ViewSlot::rtv(slot).unwrap()) else {
            continue;
        };
        let (view, key) = image_view(env, bound.pointer, ViewKind::RenderTarget)?;
        colors.push(blend_color(blend, slot, view));
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
    let Some(attachment) = colors
        .first()
        .map(|color| &color.view)
        .or_else(|| depth.as_ref().map(|depth| &depth.view))
    else {
        return Ok(());
    };
    let extent = view_rect(attachment);
    let samples = attachment.image().desc().samples;
    let vp = snapshot.rs.viewports[0];
    if vp.Width <= 0. || vp.Height <= 0. {
        return Ok(());
    }
    let mut scissor = extent;
    if raster.ScissorEnable != 0 {
        if snapshot.rs.scissor_count == 0 {
            return Ok(());
        }
        let raw = snapshot.rs.scissors[0];
        scissor = Rect {
            x: raw.left.max(0) as u32,
            y: raw.top.max(0) as u32,
            w: raw.right.saturating_sub(raw.left.max(0)).max(0) as u32,
            h: raw.bottom.saturating_sub(raw.top.max(0)).max(0) as u32,
        };
        scissor = scissor.clipped(extent);
        if scissor.empty() {
            return Ok(());
        }
    }
    let alpha_to_coverage = blend.is_some_and(|blend| blend.AlphaToCoverageEnable != 0);
    let (vertex_shader, pixel_shader, vertex_code, pixel_code, fetches) = {
        let caps = env
            .ext
            .get::<State>()
            .execution
            .as_ref()
            .ok_or(Error::Device)?
            .gpu
            .device()
            .caps();
        let vertex_shader = context::owned(env, vertex)
            .and_then(Object::shader)
            .ok_or(Error::InvalidArgument)?;
        let pixel_shader = context::owned(env, pixel).and_then(Object::shader);
        let reflection = vertex_shader.reflection();
        let pixel_reflection = pixel_shader.map(|shader| (u64::from(pixel), shader.reflection()));
        let input_layout = context::owned(env, layout).and_then(Object::input_layout);
        let mut fetches = Vec::new();
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
            fetches.push(VertexFetch {
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
            });
        }
        let facts = DrawFacts {
            fragment: pixel_reflection,
            alpha_to_coverage,
            wireframe: raster.FillMode == 2,
            ..Default::default()
        };
        let variant = Variant::select(reflection, &facts, caps);
        if variant.pulled() {
            return Err(Error::InvalidArgument);
        }
        let options = Options {
            raster_samples: samples,
            ..Options::default()
        };
        let vertex_code = pulled::vertex(
            vertex_shader.module(),
            &variant,
            pixel_reflection.map(|(_, reflection)| reflection),
            options,
            &fetches,
            args.start(indexed),
        )
        .map_err(shader_error)?;
        let vertex = modern_shader(&vertex_code, adv_gpu::shader::Stage::Vertex)?;
        let (pixel, pixel_code) = if let Some(shader) = pixel_shader {
            let variant = Variant::select(shader.reflection(), &facts, caps);
            let code = wgsl::emit_variant(shader.module(), &variant, None, options)
                .map_err(shader_error)?;
            (
                Some(modern_shader(&code, adv_gpu::shader::Stage::Fragment)?),
                Some(code),
            )
        } else {
            (None, None)
        };
        (vertex, pixel, vertex_code, pixel_code, fetches)
    };
    let mut reads = Vec::new();
    let mut vertex_bindings = slots_for(env, &snapshot, Stage::Vertex, &vertex_code, &mut reads)?;
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
    let fragment_bindings = pixel_code
        .as_ref()
        .map(|code| slots_for(env, &snapshot, Stage::Pixel, code, &mut reads))
        .transpose()?
        .unwrap_or_default();
    for fetch in fetches {
        let slot = (VERTEX_STREAM_BASE + fetch.stream) as usize;
        if vertex_bindings
            .storage
            .get(slot)
            .is_some_and(Option::is_some)
        {
            continue;
        }
        let pointer = snapshot.ia.vertices[fetch.stream as usize].buffer;
        let buffer = capture_buffer(env, pointer, BufferUsage::STORAGE)?;
        reads.push(key(env, pointer, 0));
        let range = 0..buffer.desc().size;
        set_slot(
            &mut vertex_bindings.storage,
            slot,
            StorageBinding::Buffer(BufferRange { buffer, range }),
        );
    }
    let index = if indexed {
        let index = ensure_index(env, &snapshot, &args)?;
        reads.push(key(env, snapshot.ia.indices.buffer, 0));
        Some(index)
    } else {
        None
    };
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
    let command = Command::Draw {
        draw: Box::new(ModernDraw {
            state_ids: [
                layout,
                snapshot.rs.state,
                snapshot.om.blend,
                snapshot.om.depth_stencil,
            ]
            .map(u64::from),
            vertex: vertex_shader,
            fragment: pixel_shader,
            colors: colors.into_iter().collect(),
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
        }),
        vertex: vertex_bindings,
        fragment: fragment_bindings,
        statistics,
    };
    let execution = execution(env.ext)?;
    execution.recording.append(
        &mut execution.gpu,
        command,
        Effects {
            extent,
            bounds: scissor,
            key: 0,
            reads: &reads,
            writes: &writes,
            retained_bytes: 0,
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
    let pointer = snapshot.stages[Stage::Compute as usize].shader;
    let Some(shader) = context::owned(env, pointer).and_then(Object::shader) else {
        return Ok(());
    };
    let mut uav_formats = Vec::new();
    let mut buffer_formats = Vec::new();
    let mut buffer_elements = Vec::new();
    let mut writes = Vec::new();
    for binding in &shader.reflection().bindings {
        if let Some(view) = match binding.register {
            Register::Resource => ViewSlot::srv(Stage::Compute, binding.slot as usize),
            Register::UnorderedAccessView => ViewSlot::cs_uav(binding.slot as usize),
            _ => None,
        }
        .and_then(|slot| snapshot.view(slot))
        {
            let typed_format = match binding.kind {
                BindingKind::Typed { dimension, .. } => {
                    Some((dimension, storage_format(view.desc.format())?))
                }
                _ => None,
            };
            if binding.register == Register::UnorderedAccessView {
                writes.push((
                    key(env, view.resource, view.desc.subresources().mips.start),
                    false,
                ));
                if let Some((_, format)) = typed_format {
                    uav_formats.push((binding.slot, format));
                }
            } else if let Some((ResourceDimension::Buffer, format)) = typed_format {
                buffer_formats.push((binding.slot, format));
            }
            if let Some((ResourceDimension::Buffer, _)) = typed_format {
                buffer_elements.push((
                    binding.register,
                    binding.slot,
                    view.desc.elements().len() as u32,
                ));
            }
        }
    }
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
        storage_formats: &uav_formats,
        ..Default::default()
    };
    let variant = Variant::select(shader.reflection(), &facts, caps);
    let code = wgsl::emit_variant(
        shader.module(),
        &variant,
        None,
        Options {
            buffer_formats: &buffer_formats,
            buffer_elements: &buffer_elements,
            image_atomics: caps.features.contains(adv_gpu::Features::IMAGE_ATOMICS),
            ..Default::default()
        },
    )
    .map_err(shader_error)?;
    let mut reads = Vec::new();
    reads.extend(indirect_read);
    let bindings = slots_for(env, &snapshot, Stage::Compute, &code, &mut reads)?;
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

fn storage_format(format: u32) -> Result<naga::StorageFormat, Error> {
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
