//! Complete command fingerprints and logical subresource identities. Float
//! payloads use their stored IEEE bits; shader keys are generational identities,
//! never WGSL text hashed again for every draw.
use super::*;
use adv_device::gpu::{DrawState, Image};
use smallvec::SmallVec;
use std::hash::Hash;
type Hasher = xxhash_rust::xxh3::Xxh3;

pub(in crate::d3d9) use crate::record::key::{hash, rect};
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
        blend_constant,
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
    let key = hash((
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
    ));
    blend_constant.map_or(key, |color| hash((key, color)))
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
        material,
        lights,
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
        material.map(f32::to_bits),
        lights,
    ))
}
struct ResourceIdentity {
    owner: Id,
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
                Self {
                    owner,
                    level: r.level,
                    levels: r.levels.len().max(1) as u32,
                }
            }
            _ => Self {
                owner: id,
                level: 0,
                levels: 1,
            },
        }
    }
    fn content(&self, level: Option<u32>, aspect: u8) -> Content {
        crate::record::ResourceKey(
            self.owner.key(),
            u64::from(level.unwrap_or(self.level)),
            aspect,
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
    signature: (Option<Id>, Option<Id>, u64, u64),
    pub(super) output: [Option<Content>; 3],
    pub(super) samples: Vec<Content>,
}
pub(super) fn bind(env: &mut Env<'_>, owner: Id) -> Result<(), u32> {
    #[cfg(feature = "d3d9-counters")]
    lookup(1);
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return Err(INVALID);
    };
    let hot = d.hot.borrow();
    let signature = (
        d.render_target.or(d.back_buffer),
        d.depth_stencil,
        hot.versions[12],
        hot.target_version,
    );
    if d.frame
        .binding_cache
        .as_ref()
        .is_some_and(|b| b.signature == signature)
    {
        return Ok(());
    }
    let mut samples = Vec::new();
    for id in std::iter::once(hot.texture)
        .chain(hot.shaders.textures)
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
    drop(hot);
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
    pub reads: SmallVec<[Content; 4]>,
    pub samples: SmallVec<[damage::sampling::Read; 2]>,
    pub writes: SmallVec<[(Content, bool); 3]>,
    pub bytes: usize,
    pub bounds: adv_device::surface::Rect,
}
impl Prepared {
    pub(super) fn kind(&self) -> crate::record::Kind {
        use crate::record::Kind;
        match self.operation {
            Operation::Draw(_) => Kind::Draw,
            Operation::Clear { .. } => Kind::Clear,
            Operation::Copy { .. } => Kind::Copy,
            Operation::Upload { .. } => Kind::Upload,
            _ => unreachable!(),
        }
    }

    pub(super) fn damage(&self, target: Option<&Target>) -> crate::record::damage::Input<'_> {
        let image = damage::image(&self.operation).unwrap();
        let depth = match &self.operation {
            Operation::Draw(d) => d.target.depth(),
            Operation::Clear { target, .. } => target.depth(),
            _ => None,
        };
        let attachment = depth.map(|d| {
            (
                adv_device::surface::Rect {
                    x: 0,
                    y: 0,
                    w: d.width(),
                    h: d.height(),
                },
                d.format() as u8,
            )
        });
        crate::record::damage::Input {
            kind: self.kind(),
            view: damage::full(image),
            attachment,
            color_shape: image.alpha_mode() as u8,
            slot: hash(target.map_or(image, |t| &t.image).id()),
            bounds: self.bounds,
            key: self.key,
            reads: &self.reads,
            samples: &self.samples,
            writes: &self.writes,
            ignores_sampled_color: self.ignores_sampled_color(),
            clear_keys: self.clear_keys(),
        }
    }
    // A fixed-function draw without an alpha test never uses sampled color
    // to decide coverage or the written depth and stencil values.
    fn ignores_sampled_color(&self) -> bool {
        matches!(&self.operation, Operation::Draw(d)
                if (d.programmable.is_none() || d.fixed_vertex.as_ref().is_some_and(|v| v.fixed_pixel))
                    && d.state.alpha_test == adv_device::gpu::Compare::Always)
    }

    fn clear_keys(&self) -> [Option<u128>; 3] {
        let Operation::Clear {
            target,
            rectangle,
            color,
            depth,
            stencil,
        } = &self.operation
        else {
            return [None; 3];
        };
        let mut keys = [None; 3];
        for &(content, _) in &self.writes {
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
            keys[content.2 as usize] = Some(hash((content, rect(*rectangle), component)));
        }
        keys
    }
}
pub(super) fn prepare(env: &mut Env<'_>, owner: Id, operation: Operation) -> Result<Prepared, u32> {
    if matches!(operation, Operation::Draw(_)) {
        env.ext.get_mut::<State>().frames.draw_key_hashes += 1;
    }
    prepare_checked(env, owner, operation)
}

fn prepare_checked(
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
    let (mut reads, mut writes, mut bytes) = (SmallVec::new(), SmallVec::new(), 0);
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
            write(
                content(env, *resource, *level, 0),
                format::covers(*rectangle, &target.image),
            );
            (
                1u8,
                target_key(target),
                target.image.texel_format() as u8,
                pixels,
                pitch,
                rect(*rectangle),
            )
                .hash(&mut h);
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
                            .is_some_and(|t| t.image.id() == destination.image.id())
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
                ..
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
            draw_state(draw, &mut h);
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
                    ..
                } = p.as_ref();
                program.textures.hash(&mut h);
                for s in streams {
                    stream_prefix(s, s.bytes.content_key().is_some(), &mut h);
                    if let Some(key) = s.bytes.content_key() {
                        key.hash(&mut h);
                    } else {
                        s.bytes.hash(&mut h);
                    }
                    bytes += s.bytes.len();
                }
                bytes += indices
                    .as_ref()
                    .map_or(0, |i| std::mem::size_of_val(i.as_slice()));
                program_suffix(p, &mut h);
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

fn draw_state(draw: &fixed::Draw, h: &mut Hasher) {
    (
        3u8,
        target_key(&draw.target),
        draw.image.as_ref().map(image),
        raster(draw.state),
        &draw.recording_key,
        &draw.program_key,
    )
        .hash(h);
}

fn stream_prefix(s: &adv_device::gpu::VertexStream, captured: bool, h: &mut Hasher) {
    (s.stride, &s.attributes, s.instance).hash(h);
    captured.hash(h);
}

fn program_suffix(p: &adv_device::gpu::ProgrammableDraw, h: &mut impl std::hash::Hasher) {
    (&p.indices, &p.vertex_count, p.instance_count).hash(h);
    for (offset, bytes) in p.constant_ranges() {
        (offset, bytes).hash(h);
    }
    for t in &p.textures {
        (t.sampler, t.image.as_ref().map(image)).hash(h);
    }
}

pub(super) fn fixed_prefix(draw: &fixed::Draw, writes: &[(Content, bool)]) -> Hasher {
    let mut h = Hasher::new();
    for &(content, _) in writes {
        content.hash(&mut h);
    }
    draw_state(draw, &mut h);
    true.hash(&mut h);
    let program = draw.programmable.as_ref().unwrap();
    program.program.textures.hash(&mut h);
    stream_prefix(&program.streams[0], true, &mut h);
    h
}

/// Store the exact bytes written by Hash for the immutable suffix following
/// the stream digest. Xxh3 and this sink both use Hasher's default encodings.
#[derive(Default)]
struct HashBytes(Vec<u8>);
impl std::hash::Hasher for HashBytes {
    fn write(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn finish(&self) -> u64 {
        unreachable!("the suffix is a byte sequence")
    }
}

pub(super) fn fixed_suffix(draw: &fixed::Draw) -> Vec<u8> {
    let mut bytes = HashBytes::default();
    program_suffix(draw.programmable.as_ref().unwrap(), &mut bytes);
    bytes.0
}

pub(super) fn fixed_key(mut prefix: Hasher, geometry: u128, suffix: &[u8]) -> u128 {
    geometry.hash(&mut prefix);
    prefix.update(suffix);
    prefix.digest128()
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
            let width = match pixels {
                Pixels::Staged(..) => match target.image.texel_format() {
                    adv_device::gpu::TexelFormat::A8 => 1,
                    adv_device::gpu::TexelFormat::B5g6r5 | adv_device::gpu::TexelFormat::Bgra4 => 2,
                    _ => 4,
                },
                Pixels::Host(..) => 4,
            };
            bounds(&target.image, *rectangle, false)
                && u64::from(*pitch) >= u64::from(rectangle.w) * width
                && pitch.is_multiple_of(width as u32)
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
                    let mut locations = 0u16;
                    n <= 0x300000
                        && count(s.topology, n)
                        && last < p.vertex_count
                        && p.instance_count != 0
                        && p.textures.len() == p.program.textures.len()
                        && p.streams.iter().map(|s| s.bytes.len()).sum::<usize>()
                            <= 16 * 1024 * 1024
                        && p.streams.iter().all(|s| {
                            s.stride <= 2048
                                && s.stride.is_multiple_of(4)
                                && !s.attributes.is_empty()
                                && s.attributes.iter().all(|a| {
                                    a.offset.checked_add(a.format.size()).is_some_and(|end| {
                                        if a.location >= 16 || locations & (1 << a.location) != 0 {
                                            return false;
                                        }
                                        locations |= 1 << a.location;
                                        a.offset.is_multiple_of(4)
                                            && (s.stride == 0 || end <= s.stride)
                                            && u64::from(if s.instance {
                                                p.instance_count - 1
                                            } else {
                                                last
                                            }) * u64::from(s.stride)
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
