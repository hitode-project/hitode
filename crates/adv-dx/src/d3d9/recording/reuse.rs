//! The preceding frame owns shared prepared indexed draws and their immutable
//! effects. Guest-writable inputs are compared with retained bytes before reuse.
use super::*;
use std::hash::Hash;

#[derive(Default)]
pub(super) struct Cache {
    targets: HashMap<Id, Sequence>,
    disabled: bool,
}
#[derive(Default)]
struct Sequence {
    previous: Vec<Arc<Entry>>,
    next: Vec<Arc<Entry>>,
    index: HashMap<u128, Vec<usize>>,
    next_index: HashMap<u128, Vec<usize>>,
    cursor: usize,
}
pub(super) struct Entry {
    signature: u128,
    prepared: Arc<key::Prepared>,
    spans: fixed::CapturedSpans,
    pub saved: std::sync::OnceLock<Saved>,
}
#[derive(Clone)]
pub(super) struct Saved {
    pub command: u128,
    pub damage: Arc<damage::Record>,
    pub pass: Arc<passes::Effects>,
    pub destinations: Vec<Option<u128>>,
}
impl Cache {
    pub(super) fn invalidate(&mut self) {
        self.targets.clear();
        self.disabled = true;
    }
    /// Whether a retained entry still draws into `image`'s allocation.
    #[cfg(test)]
    pub(super) fn retains(&self, image: &adv_device::gpu::Image) -> bool {
        self.targets.values().any(|s| {
            s.previous.iter().chain(&s.next).any(|e| {
                matches!(&e.prepared.operation,
                    Operation::Draw(d) if d.target.image.shares_storage(image))
            })
        })
    }
    pub(super) fn finish(&mut self) {
        if self.disabled {
            *self = Self::default();
            return;
        }
        self.targets.retain(|_, s| {
            s.previous = std::mem::take(&mut s.next);
            s.index = std::mem::take(&mut s.next_index);
            s.cursor = 0;
            !s.previous.is_empty()
        });
    }
}
fn signature(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, &[u32]),
) -> Result<(u128, Target), u32> {
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .ok_or(INVALID)?;
    key::bind(env, owner)?;
    let d = device(env, pointer).ok_or(INVALID)?;
    if !d.scene
        || d.needs_reset
        || d.frame.reuse.disabled
        || d.frame.partial
        || d.frame.active.is_some()
    {
        return Err(INVALID);
    }
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    #[cfg(any(test, feature = "d3d9-counters"))]
    debug_assert_eq!(
        d.fingerprints,
        std::array::from_fn(|class| d.class_key(class))
    );
    (request, d.fingerprints).hash(&mut h);
    let target = targets::draw_target(env, pointer)?;
    // Presentation rotates back-buffer images; commands depend only on the
    // target's shape, and a reused command is rebased onto the current one.
    (
        d.render_target.or(d.back_buffer),
        d.depth_stencil,
        key::target_key(&target),
    )
        .hash(&mut h);
    d.buffers
        .streams
        .iter()
        .filter(|s| s.buffer.is_some())
        .count()
        .hash(&mut h);
    for (slot, stream) in d.buffers.streams.iter().enumerate() {
        if let Some(id) = stream.buffer {
            (slot, id, stream.offset, stream.stride).hash(&mut h);
            let Some(Payload::Buffer(b)) = com(env).get_identity(&env.sys.objects, id) else {
                return Err(INVALID);
            };
            if b.locks != 0 {
                return Err(INVALID);
            }
        }
    }
    d.buffers.indices.hash(&mut h);
    std::iter::once(d.texture)
        .chain(d.shaders.textures)
        .flatten()
        .count()
        .hash(&mut h);
    for (slot, id) in std::iter::once(d.texture)
        .chain(d.shaders.textures)
        .enumerate()
    {
        if let Some(id) = id {
            (slot, id).hash(&mut h);
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
            (r.lod, image.view(), image.alpha_mode() as u8).hash(&mut h);
        }
    }
    // Local sampling proofs depend on which producers precede this draw.
    let bindings = d.frame.binding_cache.as_ref().unwrap();
    for content in bindings.output.iter().skip(1).flatten() {
        (
            d.frame.damage.written(content),
            d.frame.contents.get(content),
        )
            .hash(&mut h);
    }
    for content in bindings.output.iter().flatten() {
        let independent = d.frame.damage.independent(content);
        independent.hash(&mut h);
    }
    for content in &bindings.samples {
        (
            d.frame.contents.get(content),
            d.frame.damage.written(content),
        )
            .hash(&mut h);
    }
    Ok((h.digest128(), target))
}
fn unchanged(env: &Env<'_>, spans: &[(u32, Arc<[u8]>)]) -> bool {
    spans.iter().all(|(address, old)| {
        if adv_abi::guest::check_range(env.cpu, *address, old.len() as u32, Prot::READ).is_err() {
            return false;
        }
        if let Some(bytes) = env.cpu.shared_bytes(*address, old.len()) {
            bytes == old.as_ref()
        } else {
            let mut bytes = vec![0; old.len()];
            env.cpu.read(*address, &mut bytes).is_ok() && bytes == old.as_ref()
        }
    })
}
pub(in crate::d3d9) fn indexed(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, &[u32]),
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    let (signature, target) = match signature(env, pointer, request) {
        Ok(signed) => signed,
        Err(_) => {
            return match build(env) {
                Ok(draw) => {
                    let owner = com(env)
                        .identity::<Payload>(&env.sys.objects, pointer)
                        .unwrap();
                    record(env, owner, Operation::Draw(draw))
                }
                Err(code) => Outcome::Return(code),
            };
        }
    };
    let d = device(env, pointer).unwrap();
    let target_id = d.render_target.or(d.back_buffer).unwrap();
    let sequence = d.frame.reuse.targets.get(&target_id);
    let expected = sequence
        .and_then(|s| s.previous.get(s.cursor))
        .filter(|e| e.signature == signature && unchanged(env, &e.spans));
    let cached = expected
        .or_else(|| {
            sequence.and_then(|s| {
                s.index
                    .get(&signature)
                    .into_iter()
                    .flatten()
                    .filter_map(|&n| s.previous.get(n))
                    .chain(
                        s.next_index
                            .get(&signature)
                            .into_iter()
                            .flatten()
                            .filter_map(|&n| s.next.get(n)),
                    )
                    .find(|entry| unchanged(env, &entry.spans))
            })
        })
        .cloned();
    let origin = cached.clone();
    let entry = if let Some(entry) = cached {
        #[cfg(any(test, feature = "d3d9-counters"))]
        {
            let owner = com(env)
                .identity::<Payload>(&env.sys.objects, pointer)
                .unwrap();
            let reference = key::prepare(env, owner, entry.prepared.operation.clone()).unwrap();
            debug_assert_eq!(reference.key, entry.prepared.key);
            debug_assert_eq!(reference.bounds, entry.prepared.bounds);
            debug_assert!(
                reference.reads == entry.prepared.reads
                    && reference.writes == entry.prepared.writes
                    && reference.samples == entry.prepared.samples
            );
        }
        // Retained entries must not pin a rotated back-buffer image: rebase
        // the shared preparation onto the current target when it moved.
        match &entry.prepared.operation {
            Operation::Draw(draw) if !draw.target.image.shares_storage(&target.image) => {
                let mut prepared = (*entry.prepared).clone();
                if let Operation::Draw(draw) = &mut prepared.operation {
                    draw.target = target.clone();
                }
                let saved = std::sync::OnceLock::new();
                if let Some(old) = entry.saved.get() {
                    let _ = saved.set(old.clone());
                }
                Arc::new(Entry {
                    signature: entry.signature,
                    prepared: Arc::new(prepared),
                    spans: entry.spans.clone(),
                    saved,
                })
            }
            _ => entry,
        }
    } else {
        *d.fixed.captures.borrow_mut() = Some(Vec::new());
        let draw = build(env);
        let spans = device(env, pointer)
            .unwrap()
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
            signature,
            prepared: Arc::new(prepared),
            spans,
            saved: Default::default(),
        })
    };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let sequence = frame(env, owner)
        .reuse
        .targets
        .entry(target_id)
        .or_default();
    if let Some(positions) = sequence.index.get(&signature)
        && let Some(&n) = positions.iter().find(|&&n| {
            n >= sequence.cursor
                && origin
                    .as_ref()
                    .is_some_and(|o| Arc::ptr_eq(&sequence.previous[n], o))
        })
    {
        sequence.cursor = n + 1;
    }
    sequence
        .next_index
        .entry(signature)
        .or_default()
        .push(sequence.next.len());
    sequence.next.push(entry.clone());
    if env.ext.get::<State>().gpu.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    barrier_if_busy(env, owner, move |env| {
        append(env, owner, &entry.prepared, Some(target), Some(&entry))
    })
}
