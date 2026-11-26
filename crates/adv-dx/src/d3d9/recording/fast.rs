//! A validated single-stream fixed draw caches the state that does not depend
//! on guest vertices. Uncached and retained draws still use general preparation.
//! One walk captures bytes, feeds the same xxh3 stream digest and records the
//! projected positions and texture coordinates used by damage analysis.
use super::*;
use adv_device::gpu::{AttributeFormat, ImageId, Topology};
use adv_device::surface::Rect;
use smallvec::SmallVec;
use std::ops::Range;
use xxhash_rust::xxh3::Xxh3;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    request: (i32, u32, u32),
    state: u128,
    attachments: (Option<Id>, Option<Id>, ImageId, key::TargetKey),
    texture: Option<(Id, ImageId, u8, u32)>,
    samples: SmallVec<[Content; 4]>,
}

pub(super) struct Input {
    key: Key,
    target: Target,
    address: u32,
    available: u32,
    vertices: u32,
}

struct Cached {
    prepared: key::Prepared,
    prefix: Xxh3,
    suffix: Vec<u8>,
    bytes: u32,
    position: usize,
    uv: Option<(usize, usize)>,
    source: Option<Content>,
}

#[derive(Default)]
pub(super) struct Cache {
    states: HashMap<Key, Cached>,
}

impl Cache {
    pub fn clear(&mut self) {
        self.states.clear();
    }
}

pub(super) fn input(
    env: &mut Env<'_>,
    pointer: u32,
    owner: Id,
    request: (i32, u32, reuse::Mode),
    up: Option<u32>,
) -> Option<Input> {
    let (kind, count, mode) = request;
    let d = device(env, pointer)?;
    #[cfg(test)]
    if d.frame.general_draws {
        return None;
    }
    if d.needs_reset
        || d.gpu_lost
        || d.frame.active.is_some()
        || trace::capturing()
        || !matches!(kind, 1 | 4..=6)
    {
        return None;
    }
    let hot = d.hot.borrow();
    if !hot.scene
        || hot.shaders.vertex.is_some()
        || hot.shaders.pixel.is_some()
        || hot.shaders.textures.iter().any(Option::is_some)
    {
        return None;
    }
    let (stride, address, available) = match mode {
        reuse::Mode::Up(stride) => (stride, up.unwrap(), u32::MAX),
        reuse::Mode::Bound(start) => {
            let stream = hot.buffers.streams[0];
            let Payload::Buffer(buffer) =
                com(env).get_identity(&env.sys.objects, stream.buffer?)?
            else {
                return None;
            };
            if buffer.locks != 0 {
                return None;
            }
            let offset = start
                .checked_mul(stream.stride)?
                .checked_add(stream.offset)?;
            (
                stream.stride,
                buffer.storage.checked_add(offset)?,
                buffer.len.checked_sub(offset)?,
            )
        }
        reuse::Mode::Indexed => return None,
    };
    if stride == 0 || stride > 2048 || !stride.is_multiple_of(4) {
        return None;
    }
    let texture = if let Some(id) = hot.texture {
        let Payload::Resource(resource) = com(env).get_identity(&env.sys.objects, id)? else {
            return None;
        };
        if resources::sampling_blocked(env, resource) {
            return None;
        }
        let image = (env.ext.get::<State>().provider.target)(env.ext, resource.surface?)?.image;
        Some((id, image.id(), image.alpha_mode() as u8, resource.lod))
    } else {
        None
    };
    let mut target = targets::draw_target(env, pointer).ok()?;
    if hot.fixed.render[&7] != 0 || hot.fixed.render[&52] != 0 {
        target = depth::attach(env, pointer, target).ok()?;
    }
    let attachments = (
        d.render_target.or(d.back_buffer),
        d.depth_stencil,
        target.image.id(),
        key::target_key(&target),
    );
    drop(hot);
    let (state, hashed) = draw_state_identity(env, pointer);
    env.ext.get_mut::<State>().frames.draw_key_hashes += u64::from(hashed);
    key::bind(env, owner).ok()?;
    let samples = &frame(env, owner).binding_cache.as_ref()?.samples;
    if samples.len() > 4 {
        return None;
    }
    Some(Input {
        key: Key {
            request: (kind, count, stride),
            state,
            attachments,
            texture,
            samples: SmallVec::from_slice(samples),
        },
        target,
        address,
        available,
        vertices: fixed::primitive_length(kind, count).ok()?,
    })
}

pub(super) fn remember(
    env: &mut Env<'_>,
    owner: Id,
    pointer: u32,
    input: Input,
    p: &key::Prepared,
) {
    if p.reads.len() > 4 {
        return;
    }
    let Operation::Draw(draw) = &p.operation else {
        return;
    };
    let (Some(fixed), Some(program)) = (&draw.fixed_vertex, &draw.programmable) else {
        return;
    };
    let [stream] = program.streams.as_slice() else {
        return;
    };
    if stream.instance
        || program.instance_count != 1
        || stream.stride != input.key.request.2
        || program.parameters[2][3] != 0
        || program.vertex_count != input.vertices
        || stream.bytes.content_key().is_none()
        || fixed.point_scale.is_some()
    {
        return;
    }
    // The cache only takes layouts whose complete fixed input is on stream zero.
    let Ok(layout) = fixed::layout(env, pointer) else {
        return;
    };
    if layout.streams[0] != 0
        || layout.color.is_some() && layout.streams[1] != 0
        || layout.normal.is_some_and(|(s, _)| s != 0)
        || layout.specular.is_some_and(|(s, _)| s != 0)
        || layout.texcoords.iter().flatten().any(|&(s, _, _)| s != 0)
    {
        return;
    }
    let position = layout.position as usize;
    let uv = stream
        .attributes
        .iter()
        .find(|a| a.location == 2)
        .map(|a| (a.offset as usize, (a.format.size() as usize / 4).min(2)));
    let samples_supported = stream.attributes.iter().all(|a| {
        matches!(
            a.format,
            AttributeFormat::Float1
                | AttributeFormat::Float2
                | AttributeFormat::Float3
                | AttributeFormat::Float4
                | AttributeFormat::Bgra8Unorm
        )
    });
    let source = input
        .key
        .texture
        .filter(|_| {
            samples_supported
                && matches!(
                    draw.state.topology,
                    Topology::Triangles | Topology::TriangleStrip
                )
        })
        .map(|(id, ..)| key::content(env, id, Some(0), 0));
    let prefix = key::fixed_prefix(draw, &p.writes);
    let suffix = key::fixed_suffix(draw);
    let bytes = stream.bytes.len() as u32;
    let mut prepared = p.clone();
    prepared.samples.clear();
    let Operation::Draw(draw) = &mut prepared.operation else {
        unreachable!()
    };
    Arc::make_mut(draw.programmable.as_mut().unwrap()).streams[0].bytes = Default::default();
    let states = &mut frame(env, owner).fast.states;
    if states.len() == 256 {
        states.clear();
    }
    states.insert(
        input.key,
        Cached {
            prepared,
            prefix,
            suffix,
            bytes,
            position,
            uv,
            source,
        },
    );
}

pub(super) fn capture(
    env: &mut Env<'_>,
    owner: Id,
    input: &Input,
) -> Option<(key::Prepared, Range<usize>)> {
    let com = com(env);
    let Some(Payload::Device(d)) = com.get_identity_mut(&mut env.sys.objects, owner) else {
        return None;
    };
    let f = &mut d.frame;
    let cached = f.fast.states.get(&input.key)?;
    let Operation::Draw(draw) = &cached.prepared.operation else {
        unreachable!()
    };
    if !draw.target.same_attachments(&input.target)
        || input.available < cached.bytes
        || adv_abi::guest::check_range(env.cpu, input.address, cached.bytes, Prot::READ).is_err()
    {
        return None;
    }
    let raw = env.cpu.shared_bytes(input.address, cached.bytes as usize)?;
    let program = draw.programmable.as_ref().unwrap();
    let fixed = draw.fixed_vertex.as_ref().unwrap();
    let clip_count = program
        .indices
        .as_ref()
        .map_or(input.vertices as usize, |i| i.len());
    let sampling = cached.source.filter(|_| {
        (3..=64).contains(&clip_count)
            && cached
                .prepared
                .reads
                .iter()
                .any(|k| f.core.damage.written(k))
    });
    let mut fit = SmallVec::<[([f64; 2], [f64; 2]); 64]>::new();
    let mut bounds = (clip_count <= 64).then(damage::Bounds::default);
    let mut common_w = None;
    let mut samples_valid = sampling.is_some();
    let from = f.vertices.len();
    f.vertices.reserve(raw.len());
    let mut geometry = Xxh3::new();
    for vertex in raw.chunks(input.key.request.2 as usize) {
        f.vertices.extend_from_slice(vertex);
        geometry.update(vertex);
        if bounds.is_none() && !samples_valid {
            continue;
        }
        let at = cached.position;
        let position = std::array::from_fn(|c| {
            if c == 3 && fixed.transforms.is_some() {
                1.
            } else {
                float(vertex, at + c * 4)
            }
        });
        let Some((clip, _)) =
            crate::fixed::tnl::clip(position, fixed.transforms.as_ref(), draw.state.viewport)
        else {
            bounds = None;
            samples_valid = false;
            continue;
        };
        let screen = damage::screen(clip, draw.state.viewport);
        if let Some(b) = &mut bounds {
            if clip.into_iter().all(f32::is_finite)
                && clip[3] > 0.
                && screen.into_iter().all(f32::is_finite)
            {
                b.add(screen);
            } else {
                bounds = None;
            }
        }
        if samples_valid {
            let w = clip[3];
            samples_valid = w.is_finite()
                && w > 0.
                && common_w.is_none_or(|previous| previous == w)
                && screen.into_iter().all(f32::is_finite);
            common_w = Some(w);
            if samples_valid {
                let uv = cached.uv.map_or([0.; 2], |(offset, width)| {
                    std::array::from_fn(|c| {
                        if c < width {
                            f64::from(float(vertex, offset + c * 4)) + 0.
                        } else {
                            0.
                        }
                    })
                });
                fit.push((screen.map(f64::from), uv));
            }
        }
    }
    let mut p = cached.prepared.clone();
    p.key = key::fixed_key(cached.prefix.clone(), geometry.digest128(), &cached.suffix);
    #[cfg(feature = "d3d9-counters")]
    {
        p.lookups = 0;
    }
    let full = Rect {
        x: 0,
        y: 0,
        w: draw.target.image.width(),
        h: draw.target.image.height(),
    };
    p.bounds = bounds.map_or(full, |b| b.rectangle(draw, 1.));
    if samples_valid
        && let Some(source) = sampling
        && let Some(binding) = program.textures.first()
        && let Some(image) = &binding.image
    {
        let vertices =
            (0..clip_count).map(|n| fit[program.indices.as_ref().map_or(n, |i| i[n] as usize)]);
        if let Some(mapping) = damage::sampling::fit(vertices, image, binding.sampler) {
            p.samples.push(damage::sampling::Read { source, mapping });
        }
    }
    Some((p, from..f.vertices.len()))
}

fn float(vertex: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(vertex[offset..offset + 4].try_into().unwrap())
}

pub(super) fn materialize(draw: &mut fixed::Draw, bytes: &[u8]) {
    let program = Arc::make_mut(draw.programmable.as_mut().unwrap());
    program.streams[0].bytes = bytes.to_vec().into();
}
