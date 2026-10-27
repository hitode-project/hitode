//! Complete command fingerprints and logical subresource identities. Float
//! payloads use their stored IEEE bits; shader keys are generational identities,
//! never WGSL text hashed again for every draw.
use super::*;
use adv_device::gpu::{DrawState, Image};
use std::hash::Hash;

pub(in crate::d3d9) fn hash(value: impl Hash) -> u128 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    value.hash(&mut h);
    h.digest128()
}
pub(in crate::d3d9) fn rect(r: adv_device::surface::Rect) -> (u32, u32, u32, u32) {
    (r.x, r.y, r.w, r.h)
}
fn image(i: &Image) -> (u32, u32, u32, u8) {
    (i.width(), i.height(), i.mip_levels(), i.alpha_mode() as u8)
}
pub(super) type TargetKey = (
    (u32, u32, u32, u8),
    Option<(u32, u32, adv_device::gpu::DepthFormat)>,
);
pub(super) fn target_key(t: &Target) -> TargetKey {
    (
        image(&t.image),
        t.depth().map(|d| (d.width(), d.height(), d.format())),
    )
}
pub(super) fn raster(s: DrawState) -> u128 {
    let DrawState {
        topology,
        last_pixel,
        cull,
        blend,
        alpha_blend,
        depth,
        depth_write,
        stencil,
        stencil_reference,
        viewport,
        scissor,
        sampler,
        texture_op,
        alpha_texture_op,
        color_write,
        alpha_test,
        alpha_reference,
    } = s;
    hash((
        hash((
            topology,
            last_pixel,
            cull,
            blend,
            alpha_blend,
            depth,
            depth_write,
            stencil,
            stencil_reference,
        )),
        [
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            viewport.minimum_depth,
            viewport.maximum_depth,
        ]
        .map(f32::to_bits),
        scissor.map(rect),
        sampler,
        texture_op,
        alpha_texture_op,
        color_write,
        alpha_test,
        alpha_reference.to_bits(),
    ))
}
pub(in crate::d3d9) fn fixed(f: &fixed::Fixed) -> u128 {
    let fixed::Fixed {
        fvf,
        render,
        stage,
        stage_slots,
        sampler,
        sampler_slots,
        viewport,
        scissor,
        transforms,
        clip_planes,
    } = f;
    hash((
        fvf,
        render,
        stage,
        stage_slots,
        sampler,
        sampler_slots,
        (
            viewport.X,
            viewport.Y,
            viewport.Width,
            viewport.Height,
            viewport.MinZ.to_bits(),
            viewport.MaxZ.to_bits(),
        ),
        (scissor.left, scissor.top, scissor.right, scissor.bottom),
        transforms.map(|m| m.map(f32::to_bits)),
        clip_planes.map(|m| m.map(f32::to_bits)),
    ))
}
struct ResourceIdentity {
    owner: Id,
    pointer: u32,
    level: u32,
    levels: u32,
}
impl ResourceIdentity {
    fn resolve(env: &Env<'_>, id: Id) -> Self {
        #[cfg(feature = "d3d9-counters")]
        lookup(1);
        let com = com(env);
        match com.get_identity(&env.sys.objects, id) {
            Some(Payload::Resource(r)) => {
                let owner = r.container.unwrap_or(id);
                let pointer = if let Some(container) = r.container {
                    #[cfg(feature = "d3d9-counters")]
                    lookup(1);
                    match com.get_identity(&env.sys.objects, container) {
                        Some(Payload::Resource(parent)) => parent.pointer,
                        _ => r.pointer,
                    }
                } else {
                    r.pointer
                };
                Self {
                    owner,
                    pointer,
                    level: r.level,
                    levels: r.levels.len().max(1) as u32,
                }
            }
            _ => Self {
                owner: id,
                pointer: 0,
                level: 0,
                levels: 1,
            },
        }
    }
    fn content(&self, level: Option<u32>, aspect: u8) -> Content {
        Content(
            self.owner,
            level.unwrap_or(self.level),
            aspect,
            self.pointer,
        )
    }
}
pub(super) fn content(env: &Env<'_>, id: Id, level: Option<u32>, aspect: u8) -> Content {
    ResourceIdentity::resolve(env, id).content(level, aspect)
}
fn sample(env: &Env<'_>, id: Id, out: &mut Vec<Content>) {
    let resource = ResourceIdentity::resolve(env, id);
    out.extend((0..resource.levels).map(|n| resource.content(Some(n), 0)));
}
pub(super) struct Bindings {
    signature: (Option<Id>, Option<Id>, u64),
    pub(super) output: [Option<Content>; 3],
    pub(super) samples: Vec<Content>,
}
pub(super) fn bind(env: &mut Env<'_>, owner: Id) -> Result<(), u32> {
    #[cfg(feature = "d3d9-counters")]
    lookup(1);
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return Err(INVALID);
    };
    let signature = (
        d.render_target.or(d.back_buffer),
        d.depth_stencil,
        d.versions[12],
    );
    if d.frame
        .binding_cache
        .as_ref()
        .is_some_and(|b| b.signature == signature)
    {
        return Ok(());
    }
    let mut samples = Vec::new();
    for id in std::iter::once(d.texture)
        .chain(d.shaders.textures)
        .flatten()
    {
        sample(env, id, &mut samples);
    }
    let depth = signature.1.map(|id| ResourceIdentity::resolve(env, id));
    let output = [
        signature.0.map(|id| content(env, id, None, 0)),
        depth.as_ref().map(|r| r.content(None, 1)),
        depth.as_ref().map(|r| r.content(None, 2)),
    ];
    #[cfg(feature = "d3d9-counters")]
    lookup(1);
    frame(env, owner).binding_cache = Some(Bindings {
        signature,
        output,
        samples,
    });
    Ok(())
}
#[derive(Clone)]
pub(super) struct Prepared {
    #[cfg(feature = "d3d9-counters")]
    pub lookups: u64,
    pub operation: Operation,
    pub key: u128,
    pub reads: Vec<Content>,
    pub samples: Vec<damage::sampling::Read>,
    pub writes: Vec<(Content, bool)>,
    pub bytes: usize,
    pub bounds: adv_device::surface::Rect,
}
impl Prepared {
    pub(super) fn write_key(&self, content: Content, command: u128) -> u128 {
        let Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } = &self.operation
        else {
            return command;
        };
        let component = match content.2 {
            0 => hash((image(&target.image), color.map(|c| c.map(f64::to_bits)))),
            1 => hash((
                target.depth().map(|d| (d.width(), d.height(), d.format())),
                depth.map(f32::to_bits),
            )),
            _ => hash((
                target.depth().map(|d| (d.width(), d.height(), d.format())),
                stencil,
            )),
        };
        hash((content, rect(*rectangle), component))
    }
}
pub(super) fn prepare(
    env: &mut Env<'_>,
    owner: Id,
    mut operation: Operation,
) -> Result<Prepared, u32> {
    #[cfg(feature = "d3d9-counters")]
    let before = {
        lookup(1);
        lookups() - 1
    };
    bind(env, owner)?;
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return Err(INVALID);
    };
    let bindings = d.frame.binding_cache.as_ref().unwrap();
    validate(&operation)?;
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    let (mut reads, mut writes, mut bytes) = (Vec::new(), Vec::new(), 0);
    let mut write = |c: Content, overwrite| {
        c.hash(&mut h);
        writes.push((c, overwrite));
    };
    match &operation {
        Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } => {
            if depth.is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v)) {
                return Err(INVALID);
            }
            let full = rectangle.x == 0
                && rectangle.y == 0
                && rectangle.w == target.image.width()
                && rectangle.h == target.image.height();
            if color.is_some() {
                write(bindings.output[0].ok_or(INVALID)?, full);
            }
            if d.depth_stencil.is_some() {
                let full = rectangle.x == 0
                    && rectangle.y == 0
                    && target
                        .depth()
                        .is_some_and(|t| t.width() == rectangle.w && t.height() == rectangle.h);
                if depth.is_some() {
                    write(bindings.output[1].unwrap(), full);
                }
                if stencil.is_some() {
                    write(bindings.output[2].unwrap(), full);
                }
            }
            (
                0u8,
                target_key(target),
                rect(*rectangle),
                color.map(|v| v.map(f64::to_bits)),
                depth.map(f32::to_bits),
                stencil,
            )
                .hash(&mut h);
        }
        Operation::Upload {
            resource,
            level,
            target,
            pixels,
            pitch,
            rectangle,
        } => {
            write(content(env, *resource, *level, 0), true);
            (1u8, target_key(target), pixels, pitch, rect(*rectangle)).hash(&mut h);
            bytes = pixels.len();
        }
        Operation::Copy {
            source,
            rectangle,
            destination,
            region,
            filter,
            resources,
        } => {
            let level = match com(env).get_identity(&env.sys.objects, resources[1]) {
                Some(Payload::Resource(r)) => r
                    .levels
                    .iter()
                    .position(|l| {
                        l.surface
                            .and_then(|s| (env.ext.get::<State>().provider.target)(env.ext, s))
                            .is_some_and(|t| t.image.view() == destination.image.view())
                    })
                    .map(|n| n as u32),
                _ => None,
            };
            write(content(env, resources[1], level, 0), false);
            reads.push(content(env, resources[0], None, 0));
            (
                2u8,
                image(source),
                rect(*rectangle),
                target_key(destination),
                rect(*region),
                filter,
                resources,
            )
                .hash(&mut h);
        }
        Operation::Draw(draw) => {
            let fixed::Draw {
                target: t,
                image: texture,
                vertices,
                texcoords,
                state,
                programmable,
                recording_key,
                program_key,
            } = draw;
            if texture
                .iter()
                .chain(
                    programmable
                        .iter()
                        .flat_map(|p| p.textures.iter().filter_map(|t| t.image.as_ref())),
                )
                .any(|i| i.shares_storage(&t.image))
            {
                return Err(INVALID);
            }
            if state.color_write != 0 {
                write(bindings.output[0].ok_or(INVALID)?, false);
            }
            if d.depth_stencil.is_some() {
                if state.depth.is_some() {
                    reads.push(bindings.output[1].unwrap());
                }
                if state.stencil.is_some() {
                    reads.push(bindings.output[2].unwrap());
                }
                if state.depth.is_some() && state.depth_write {
                    write(bindings.output[1].unwrap(), false);
                }
                if state.stencil.is_some_and(|s| s.write_mask != 0) {
                    write(bindings.output[2].unwrap(), false);
                }
            }
            reads.extend_from_slice(&bindings.samples);
            (
                3u8,
                target_key(t),
                texture.as_ref().map(image),
                raster(*state),
                recording_key,
                program_key,
            )
                .hash(&mut h);
            if programmable.is_none() {
                bytemuck::cast_slice::<_, u8>(vertices).hash(&mut h);
                bytes = std::mem::size_of_val(vertices.as_slice());
            }
            if let Some((count, values)) = texcoords.as_ref().filter(|_| programmable.is_none()) {
                count.hash(&mut h);
                bytemuck::cast_slice::<_, u8>(values).hash(&mut h);
                bytes += std::mem::size_of_val(values.as_slice());
            }
            programmable.is_some().hash(&mut h);
            if let Some(p) = programmable {
                let adv_device::gpu::ProgrammableDraw {
                    program,
                    streams,
                    indices,
                    vertex_count,
                    textures,
                    ..
                } = p.as_ref();
                program.textures.hash(&mut h);
                for s in streams {
                    (s.stride, &s.attributes).hash(&mut h);
                    s.bytes.content_key().is_some().hash(&mut h);
                    if let Some(key) = s.bytes.content_key() {
                        key.hash(&mut h);
                    } else {
                        s.bytes.hash(&mut h);
                    }
                    bytes += s.bytes.len();
                }
                (indices, vertex_count).hash(&mut h);
                bytes += indices
                    .as_ref()
                    .map_or(0, |i| std::mem::size_of_val(i.as_slice()));
                for (offset, bytes) in p.constant_ranges() {
                    (offset, bytes).hash(&mut h);
                }
                for t in textures {
                    (t.sampler, t.image.as_ref().map(image)).hash(&mut h);
                }
            }
        }
        _ => return Err(INVALID),
    }
    let bounds = super::damage::bounds(env, d, &operation);
    let samples = damage::sampling::prepare(env, d, &operation, &reads);
    // Programmable streams already own these exact inputs; bounds have consumed
    // the neutral representation and backend execution only reads the streams.
    if let Operation::Draw(d) = &mut operation
        && d.programmable.is_some()
    {
        d.vertices = Arc::default();
        d.texcoords = None;
    }
    Ok(Prepared {
        #[cfg(feature = "d3d9-counters")]
        lookups: lookups() - before,
        bounds,
        operation,
        key: h.digest128(),
        reads,
        samples,
        writes,
        bytes,
    })
}

// Validation remains synchronous even when queue capacity or image ownership
// would make backend execution Busy. Keep neutral limits in sync with Gpu.
fn bounds(i: &Image, r: adv_device::surface::Rect, empty: bool) -> bool {
    (empty || (r.w > 0 && r.h > 0))
        && r.x.checked_add(r.w).is_some_and(|x| x <= i.width())
        && r.y.checked_add(r.h).is_some_and(|y| y <= i.height())
}
fn count(topology: adv_device::gpu::Topology, n: usize) -> bool {
    use adv_device::gpu::Topology::*;
    n > 0
        && match topology {
            Lines => n.is_multiple_of(2),
            Triangles => n.is_multiple_of(3),
            LineStrip => n >= 2,
            TriangleStrip => n >= 3,
            Points => true,
        }
}
fn validate(operation: &Operation) -> Result<(), u32> {
    let valid = match operation {
        Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } => {
            bounds(&target.image, *rectangle, true)
                && (color.is_some() || depth.is_some() || stencil.is_some())
                && color.is_none_or(|c| c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
                && depth.is_none_or(|z| {
                    z.is_finite() && (0.0..=1.0).contains(&z) && target.depth().is_some()
                })
                && (stencil.is_none() || target.depth().is_some_and(|d| d.format().has_stencil()))
        }
        Operation::Upload {
            target,
            rectangle,
            pitch,
            pixels,
            ..
        } => {
            let width = if matches!(pixels, Pixels::Alpha8(_)) {
                1
            } else {
                4
            };
            bounds(&target.image, *rectangle, false)
                && u64::from(*pitch) >= u64::from(rectangle.w) * width
                && (width == 1 || pitch.is_multiple_of(4))
                && u64::from(*pitch) * u64::from(rectangle.h - 1) + u64::from(rectangle.w) * width
                    <= pixels.len() as u64
        }
        Operation::Copy {
            source,
            rectangle,
            destination,
            region,
            ..
        } => bounds(source, *rectangle, false) && bounds(&destination.image, *region, false),
        Operation::Draw(d) => {
            let s = &d.state;
            let v = s.viewport;
            let raster = [
                v.x,
                v.y,
                v.width,
                v.height,
                v.minimum_depth,
                v.maximum_depth,
            ]
            .iter()
            .all(|f| f.is_finite())
                && v.x >= 0.0
                && v.y >= 0.0
                && v.width > 0.0
                && v.height > 0.0
                && v.x + v.width <= d.target.image.width() as f32
                && v.y + v.height <= d.target.image.height() as f32
                && v.minimum_depth >= 0.0
                && v.maximum_depth <= 1.0
                && v.minimum_depth <= v.maximum_depth
                && s.color_write & !15 == 0
                && s.alpha_reference.is_finite()
                && (0.0..=1.0).contains(&s.alpha_reference)
                && (s.depth.is_none() || d.target.depth().is_some())
                && (s.stencil.is_none()
                    || d.target.depth().is_some_and(|d| d.format().has_stencil()))
                && s.scissor.is_none_or(|r| bounds(&d.target.image, r, false));
            raster
                && if let Some(p) = &d.programmable {
                    let n = p
                        .indices
                        .as_ref()
                        .map_or(p.vertex_count as usize, |v| v.len());
                    let last = p
                        .indices
                        .as_ref()
                        .and_then(|i| i.iter().max().copied())
                        .unwrap_or(p.vertex_count.saturating_sub(1));
                    let mut locations = std::collections::HashSet::new();
                    n <= 0x300000
                        && count(s.topology, n)
                        && last < p.vertex_count
                        && p.textures.len() == p.program.textures.len()
                        && p.streams.iter().map(|s| s.bytes.len()).sum::<usize>()
                            <= 16 * 1024 * 1024
                        && p.streams.iter().all(|s| {
                            s.stride <= 2048
                                && s.stride.is_multiple_of(4)
                                && !s.attributes.is_empty()
                                && s.attributes.iter().all(|a| {
                                    a.offset.checked_add(a.format.size()).is_some_and(|end| {
                                        a.offset.is_multiple_of(4)
                                            && a.location < 16
                                            && locations.insert(a.location)
                                            && (s.stride == 0 || end <= s.stride)
                                            && u64::from(last) * u64::from(s.stride)
                                                + u64::from(end)
                                                <= s.bytes.len() as u64
                                    })
                                })
                        })
                } else {
                    count(s.topology, d.vertices.len())
                        && d.vertices.iter().all(|v| {
                            v.position
                                .iter()
                                .chain(&v.color)
                                .chain(&v.uv)
                                .all(|f| f.is_finite())
                        })
                }
        }
        _ => false,
    };
    if valid { Ok(()) } else { Err(INVALID) }
}
