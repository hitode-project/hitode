//! The preceding frame owns shared prepared indexed draws and their immutable
//! effects. Guest-writable inputs are compared with retained bytes before reuse.
use super::*;
use adv_device::gpu::ImageId;
use smallvec::SmallVec;

pub(super) use crate::record::reuse::{Core, Saved};
pub(super) type Cache = crate::record::reuse::Cache<Id, Entry>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Mode {
    Indexed,
    Up(u32),
    Bound(u32),
}

type SampleBinding = (usize, Id, u32, ImageId, u8);

#[derive(PartialEq, Eq, Hash)]
struct Signature {
    request: (i32, u32, Mode),
    state: [u128; 15],
    target: (Option<Id>, Option<Id>, key::TargetKey),
    textures: SmallVec<[SampleBinding; 2]>,
    reads: SmallVec<[(Option<u128>, bool); 4]>,
    independent: [bool; 3],
}
pub(super) struct Entry {
    signature: Arc<Signature>,
    index_key: u128,
    indices: Arc<[u32]>,
    prepared: Arc<key::Prepared>,
    spans: Arc<fixed::CapturedSpans>,
    pub saved: std::sync::OnceLock<Saved>,
    /// The preparation of an earlier occurrence with other destination versions.
    pub core: Option<Arc<Core>>,
}
#[cfg(test)]
impl Entry {
    pub(super) fn draws_into(&self, image: &adv_device::gpu::Image) -> bool {
        matches!(&self.prepared.operation, Operation::Draw(d) if d.target.image.shares_storage(image))
    }
}
fn signature(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, Mode),
) -> Result<(Signature, Target), u32> {
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .ok_or(INVALID)?;
    key::bind(env, owner)?;
    let d = device(env, pointer).ok_or(INVALID)?;
    let hot = d.hot.borrow();
    if !hot.scene
        || d.needs_reset
        || d.frame.reuse.disabled
        || d.frame.core.partial
        || d.frame.active.is_some()
    {
        return Err(INVALID);
    }
    if !matches!(request.2, Mode::Up(_)) {
        for stream in hot.buffers.streams.iter().filter_map(|s| s.buffer) {
            let Some(Payload::Buffer(b)) = com(env).get_identity(&env.sys.objects, stream) else {
                return Err(INVALID);
            };
            if b.locks != 0 {
                return Err(INVALID);
            }
        }
    }
    let target = targets::draw_target(env, pointer)?;
    let target = if hot.fixed.render[&7] != 0 || hot.fixed.render[&52] != 0 {
        depth::attach(env, pointer, target)?
    } else {
        target
    };
    let mut textures = SmallVec::new();
    for (slot, id) in std::iter::once(hot.texture)
        .chain(hot.shaders.textures)
        .enumerate()
    {
        if let Some(id) = id {
            let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
                return Err(INVALID);
            };
            if resources::sampling_blocked(env, r) {
                return Err(INVALID);
            }
            let image =
                (env.ext.get::<State>().provider.target)(env.ext, r.surface.ok_or(INVALID)?)
                    .ok_or(INVALID)?
                    .image;
            textures.push((slot, id, r.lod, image.id(), image.alpha_mode() as u8));
        }
    }
    // Local sampling proofs depend on which producers precede this draw.
    let bindings = d.frame.binding_cache.as_ref().unwrap();
    let reads = bindings.output[1..]
        .iter()
        .flatten()
        .chain(&bindings.samples)
        .map(|c| {
            (
                d.frame.core.contents.get(c).copied(),
                d.frame.core.damage.written(c),
            )
        })
        .collect();
    let independent = bindings
        .output
        .map(|c| c.is_some_and(|c| d.frame.core.damage.independent(&c)));
    let mut state = hot.fingerprints;
    state[13] = hot.shaders.vertex.map_or(0, |id| {
        constant_key(env, Some(id), &hot.shaders.vertex_constants)
    });
    state[14] = hot.shaders.pixel.map_or(0, |id| {
        constant_key(env, Some(id), &hot.shaders.pixel_constants)
    });
    if matches!(request.2, Mode::Up(_)) {
        // UP ignores both bound buffers, including their offsets and strides.
        state[10] = 0;
        state[11] = 0;
    }
    let signature = Signature {
        request,
        state,
        target: (
            d.render_target.or(d.back_buffer),
            d.depth_stencil,
            key::target_key(&target),
        ),
        textures,
        reads,
        independent,
    };
    Ok((signature, target))
}
fn unchanged(env: &Env<'_>, spans: &fixed::CapturedSpans, up: Option<u32>) -> bool {
    spans.iter().all(|span| {
        let address = up.unwrap_or(span.address);
        if adv_abi::guest::check_range(env.cpu, address, span.length as u32, Prot::READ).is_err() {
            return false;
        }
        if let Some(bytes) = env.cpu.shared_bytes(address, span.length) {
            span.matches(bytes)
        } else {
            let mut bytes = vec![0; span.length];
            env.cpu.read(address, &mut bytes).is_ok() && span.matches(&bytes)
        }
    })
}
pub(in crate::d3d9) fn indexed(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, &[u32]),
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    draw(
        env,
        pointer,
        (request.0, request.1, Mode::Indexed),
        request.2,
        None,
        build,
    )
}

pub(in crate::d3d9) fn up(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    data: u32,
    stride: u32,
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    draw(
        env,
        pointer,
        (kind, count, Mode::Up(stride)),
        &[],
        Some(data),
        build,
    )
}

pub(in crate::d3d9) fn bound(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    start: u32,
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    draw(
        env,
        pointer,
        (kind, count, Mode::Bound(start)),
        &[],
        None,
        build,
    )
}

fn draw(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, Mode),
    indices: &[u32],
    up: Option<u32>,
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    let rests = device(env, pointer).is_some_and(|d| {
        d.render_target
            .or(d.back_buffer)
            .is_some_and(|target| d.frame.reuse.rests(target))
    });
    if rests {
        env.ext.get_mut::<State>().frames.rested_draws += 1;
    }
    let owner = com(env).identity::<Payload>(&env.sys.objects, pointer);
    let fast_input = owner
        .filter(|_| {
            rests
                || device(env, pointer)
                    .is_some_and(|d| d.frame.reuse.disabled || d.frame.core.partial)
        })
        .and_then(|owner| fast::input(env, pointer, owner, request, up));
    if let (Some(owner), Some(input)) = (owner, &fast_input)
        && let Some((prepared, vertices)) = fast::capture(env, owner, input)
    {
        if up.is_some() {
            buffers::clear_up_bindings(env, pointer, false);
        }
        return append(env, owner, &prepared, None, None, Some(vertices));
    }
    let signed = if rests {
        Err(INVALID)
    } else {
        signature(env, pointer, request)
    };
    let (signature, target) = match signed {
        Ok(signed) => signed,
        Err(_) => {
            return match build(env) {
                Ok(draw) => {
                    if up.is_some() {
                        buffers::clear_up_bindings(env, pointer, false);
                    }
                    let owner = com(env)
                        .identity::<Payload>(&env.sys.objects, pointer)
                        .unwrap();
                    if let Some(input) = fast_input {
                        if env.ext.get::<State>().gpu.is_none() {
                            return Outcome::Return(UNAVAILABLE);
                        }
                        let p = match key::prepare(env, owner, Operation::Draw(draw)) {
                            Ok(p) => p,
                            Err(code) => return Outcome::Return(code),
                        };
                        fast::remember(env, owner, pointer, input, &p);
                        return accept(env, owner, p);
                    }
                    record(env, owner, Operation::Draw(draw))
                }
                Err(code) => Outcome::Return(code),
            };
        }
    };
    let d = device(env, pointer).unwrap();
    let target_id = d.render_target.or(d.back_buffer).unwrap();
    let (index_key, expected, mut candidates) = d.frame.reuse.candidates(
        target_id,
        |e| *e.signature == signature && *e.indices == *indices,
        || hash((&signature, indices)),
        |e| e.index_key,
    );
    let cached = candidates.find(|e| unchanged(env, &e.spans, up)).cloned();
    drop(candidates);
    let origin = cached.clone();
    let entry =
        if let Some(entry) = cached {
            let same_destination =
                entry.saved.get().is_none_or(|saved| {
                    entry.prepared.writes.iter().zip(&saved.destinations).all(
                        |((content, _), old)| d.frame.core.contents.get(content).copied() == *old,
                    )
                });
            // Keep an occurrence's output versions alongside the shared geometry.
            // The same command can occur twice with different destination inputs.
            let mut prepared = entry.prepared.clone();
            let rebase = matches!(&prepared.operation, Operation::Draw(draw)
            if !draw.target.image.shares_storage(&target.image));
            if rebase && let Operation::Draw(draw) = &mut Arc::make_mut(&mut prepared).operation {
                draw.target = target.clone();
            }
            if rebase || !same_destination {
                let saved = std::sync::OnceLock::new();
                if same_destination && let Some(old) = entry.saved.get() {
                    let _ = saved.set(old.clone());
                }
                let core = entry
                    .saved
                    .get()
                    .map(|old| old.core.clone())
                    .or_else(|| entry.core.clone());
                Arc::new(Entry {
                    signature: entry.signature.clone(),
                    index_key: entry.index_key,
                    indices: entry.indices.clone(),
                    prepared,
                    spans: entry.spans.clone(),
                    saved,
                    core,
                })
            } else {
                entry
            }
        } else {
            *d.hot.borrow().fixed.captures.borrow_mut() = Some(Vec::new());
            let draw = build(env);
            let spans = device(env, pointer)
                .unwrap()
                .hot
                .borrow()
                .fixed
                .captures
                .borrow_mut()
                .take()
                .unwrap();
            let draw = match draw {
                Ok(draw) => draw,
                Err(code) => return Outcome::Return(code),
            };
            let owner = com(env)
                .identity::<Payload>(&env.sys.objects, pointer)
                .unwrap();
            let prepared = match key::prepare(env, owner, Operation::Draw(draw)) {
                Ok(prepared) => prepared,
                Err(code) => return Outcome::Return(code),
            };
            Arc::new(Entry {
                signature: Arc::new(signature),
                index_key,
                indices: indices.into(),
                prepared: Arc::new(prepared),
                spans: Arc::new(spans),
                saved: Default::default(),
                core: None,
            })
        };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let counts = &mut env.ext.get_mut::<State>().frames;
    counts.draw_key_hashes += u64::from(!expected);
    counts.reused_draws += u64::from(origin.is_some());
    if up.is_some() {
        buffers::clear_up_bindings(env, pointer, false);
    }
    frame(env, owner).reuse.remember(
        target_id,
        index_key,
        expected,
        origin.as_ref(),
        entry.clone(),
    );
    if env.ext.get::<State>().gpu.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    barrier_if_busy(env, owner, move |env| {
        append(
            env,
            owner,
            &entry.prepared,
            Some(target),
            Some(&entry),
            None,
        )
    })
}
