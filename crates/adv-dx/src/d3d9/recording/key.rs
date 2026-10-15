//! Complete command fingerprints and logical subresource identities. Float
//! payloads use their stored IEEE bits; shader keys are generational identities,
//! never WGSL text hashed again for every draw.
use super::*;
use adv_device::gpu::{DrawState, Image};
use std::hash::{Hash, Hasher};

pub(in crate::d3d9) fn hash(value: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}
pub(in crate::d3d9) fn rect(r: adv_device::surface::Rect) -> (u32, u32, u32, u32) {
    (r.x, r.y, r.w, r.h)
}
fn image(i: &Image) -> (u32, u32, u32, u8) {
    (i.width(), i.height(), i.mip_levels(), i.alpha_mode() as u8)
}
fn target_key(t: &Target) -> impl Hash {
    (
        image(&t.image),
        t.depth().map(|d| (d.width(), d.height(), d.format())),
    )
}
pub(super) fn raster(s: DrawState) -> u64 {
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
pub(in crate::d3d9) fn fixed(f: &fixed::Fixed) -> u64 {
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
fn content(env: &Env<'_>, id: Id, level: Option<u32>, aspect: u8) -> Content {
    match com(env).get_identity(&env.sys.objects, id) {
        Some(Payload::Resource(r)) => {
            let identity = r.container.unwrap_or(id);
            let pointer = match com(env).get_identity(&env.sys.objects, identity) {
                Some(Payload::Resource(r)) => r.pointer,
                _ => r.pointer,
            };
            Content(identity, level.unwrap_or(r.level), aspect, pointer)
        }
        _ => Content(id, level.unwrap_or(0), aspect, 0),
    }
}
fn sample(env: &Env<'_>, id: Id, out: &mut Vec<Content>) {
    let count = match com(env).get_identity(&env.sys.objects, id) {
        Some(Payload::Resource(r)) => r.levels.len().max(1),
        _ => 1,
    };
    out.extend((0..count as u32).map(|n| content(env, id, Some(n), 0)));
}
pub(super) struct Prepared {
    pub operation: Operation,
    pub key: u64,
    pub reads: Vec<Content>,
    pub writes: Vec<(Content, bool)>,
    pub bytes: usize,
    pub bounds: adv_device::surface::Rect,
}
pub(super) fn prepare(env: &Env<'_>, owner: Id, operation: Operation) -> Result<Prepared, u32> {
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, owner) else {
        return Err(INVALID);
    };
    validate(&operation)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let (mut reads, mut writes, mut bytes) = (Vec::new(), Vec::new(), 0);
    let mut write = |id, level, aspect, overwrite| {
        let c = content(env, id, level, aspect);
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
                write(
                    d.render_target.or(d.back_buffer).ok_or(INVALID)?,
                    None,
                    0,
                    full,
                );
            }
            if let Some(id) = d.depth_stencil {
                let full = full
                    && target
                        .depth()
                        .is_some_and(|t| t.width() == rectangle.w && t.height() == rectangle.h);
                if depth.is_some() {
                    write(id, None, 1, full);
                }
                if stencil.is_some() {
                    write(id, None, 2, full);
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
            write(*resource, *level, 0, true);
            (
                1u8,
                target_key(target),
                pixels.as_slice(),
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
                            .is_some_and(|t| t.image.view() == destination.image.view())
                    })
                    .map(|n| n as u32),
                _ => None,
            };
            write(resources[1], level, 0, false);
            sample(env, resources[0], &mut reads);
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
                write(
                    d.render_target.or(d.back_buffer).ok_or(INVALID)?,
                    None,
                    0,
                    false,
                );
            }
            if let Some(id) = d.depth_stencil {
                if state.depth.is_some() || state.stencil.is_some() {
                    reads.push(content(env, id, None, 1));
                    reads.push(content(env, id, None, 2));
                }
                if state.depth.is_some() && state.depth_write {
                    write(id, None, 1, false);
                }
                if state.stencil.is_some_and(|s| s.write_mask != 0) {
                    write(id, None, 2, false);
                }
            }
            for id in std::iter::once(d.texture)
                .chain(d.shaders.textures.iter().copied())
                .flatten()
            {
                sample(env, id, &mut reads);
            }
            (
                3u8,
                target_key(t),
                texture.as_ref().map(image),
                raster(*state),
                recording_key,
                program_key,
            )
                .hash(&mut h);
            bytemuck::cast_slice::<_, u8>(vertices).hash(&mut h);
            bytes = std::mem::size_of_val(vertices.as_slice());
            if let Some((count, values)) = texcoords {
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
                    vertex_constants,
                    pixel_constants,
                    textures,
                    parameters,
                } = p;
                program.textures.hash(&mut h);
                for s in streams {
                    (s.stride, &s.attributes, &s.bytes).hash(&mut h);
                    bytes += s.bytes.len();
                }
                (indices, vertex_count, parameters).hash(&mut h);
                bytes += indices
                    .as_ref()
                    .map_or(0, |i| std::mem::size_of_val(i.as_slice()));
                bytemuck::bytes_of(vertex_constants).hash(&mut h);
                bytemuck::bytes_of(pixel_constants).hash(&mut h);
                for t in textures {
                    (t.sampler, t.image.as_ref().map(image)).hash(&mut h);
                }
            }
        }
        _ => return Err(INVALID),
    }
    let bounds = super::damage::bounds(env, d, &operation);
    Ok(Prepared {
        bounds,
        operation,
        key: h.finish(),
        reads,
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
            bounds(&target.image, *rectangle, false)
                && u64::from(*pitch) >= u64::from(rectangle.w) * 4
                && pitch.is_multiple_of(4)
                && u64::from(*pitch) * u64::from(rectangle.h - 1) + u64::from(rectangle.w) * 4
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
                    let n = p.indices.as_ref().map_or(p.vertex_count as usize, Vec::len);
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
