//! The preceding frame owns shared prepared indexed draws and their immutable
//! effects. Guest-writable inputs are compared with retained bytes before reuse.
use super::*;
use adv_device::gpu::ImageId;
use smallvec::SmallVec;

#[derive(Default)]
pub(super) struct Cache {
    targets: HashMap<Id, Sequence>,
    disabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Mode {
    Indexed,
    Up(u32),
    Bound(u32),
}

#[derive(PartialEq, Eq, Hash)]
struct Signature {
    request: (i32, u32, Mode),
    state: [u128; 15],
    target: (Option<Id>, Option<Id>, key::TargetKey),
    textures: SmallVec<[(usize, Id, u32, ImageId, u8); 2]>,
    reads: SmallVec<[(Option<u128>, bool); 4]>,
    independent: [bool; 3],
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
    signature: Arc<Signature>,
    index_key: u128,
    indices: Arc<[u32]>,
    prepared: Arc<key::Prepared>,
    spans: fixed::CapturedSpans,
    pub saved: std::sync::OnceLock<Saved>,
}
#[derive(Clone)]
pub(super) struct Saved {
    pub command: u128,
    pub attachment: u128,
    pub damage: Arc<damage::Record>,
    pub pass: Arc<passes::Effects>,
    pub destinations: Vec<Option<u128>>,
    pub outputs: Vec<u128>,
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
        || d.frame.partial
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
        .map(|c| (d.frame.contents.get(c).copied(), d.frame.damage.written(c)))
        .collect();
    let independent = bindings
        .output
        .map(|c| c.is_some_and(|c| d.frame.damage.independent(&c)));
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
    spans.iter().all(|(address, old)| {
        let address = up.unwrap_or(*address);
        if adv_abi::guest::check_range(env.cpu, address, old.len() as u32, Prot::READ).is_err() {
            return false;
        }
        if let Some(bytes) = env.cpu.shared_bytes(address, old.len()) {
            bytes == old.as_slice()
        } else {
            let mut bytes = vec![0; old.len()];
            env.cpu.read(address, &mut bytes).is_ok() && bytes == old.as_slice()
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
    let (signature, target) = match signature(env, pointer, request) {
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
                    record(env, owner, Operation::Draw(draw))
                }
                Err(code) => Outcome::Return(code),
            };
        }
    };
    let d = device(env, pointer).unwrap();
    let target_id = d.render_target.or(d.back_buffer).unwrap();
    let sequence = d.frame.reuse.targets.get(&target_id);
    let same_state = |e: &&Arc<Entry>| *e.signature == signature && *e.indices == *indices;
    let same_bytes = |e: &&Arc<Entry>| unchanged(env, &e.spans, up);
    let expected = sequence
        .and_then(|s| s.previous.get(s.cursor))
        .filter(same_state);
    let index_key = expected.map_or_else(|| hash((&signature, indices)), |e| e.index_key);
    let cached = expected
        .filter(same_bytes)
        .or_else(|| {
            sequence.and_then(|s| {
                s.index
                    .get(&index_key)
                    .into_iter()
                    .flatten()
                    .filter_map(|&n| s.previous.get(n))
                    .chain(
                        s.next_index
                            .get(&index_key)
                            .into_iter()
                            .flatten()
                            .filter_map(|&n| s.next.get(n)),
                    )
                    .find(|e| same_state(e) && same_bytes(e))
            })
        })
        .cloned();
    let expected = expected.is_some();
    let origin = cached.clone();
    let entry = if let Some(entry) = cached {
        let same_destination = entry.saved.get().is_none_or(|saved| {
            entry
                .prepared
                .writes
                .iter()
                .zip(&saved.destinations)
                .all(|((content, _), old)| d.frame.contents.get(content).copied() == *old)
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
            Arc::new(Entry {
                signature: entry.signature.clone(),
                index_key: entry.index_key,
                indices: entry.indices.clone(),
                prepared,
                spans: entry.spans.clone(),
                saved,
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
            spans,
            saved: Default::default(),
        })
    };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    if !expected {
        env.ext.get_mut::<State>().frames.draw_key_hashes += 1;
    }
    if up.is_some() {
        buffers::clear_up_bindings(env, pointer, false);
    }
    let sequence = frame(env, owner)
        .reuse
        .targets
        .entry(target_id)
        .or_default();
    if let Some(positions) = sequence.index.get(&index_key)
        && let Some(&n) = positions.iter().find(|&&n| {
            n >= sequence.cursor
                && origin
                    .as_ref()
                    .is_some_and(|o| Arc::ptr_eq(&sequence.previous[n], o))
        })
    {
        sequence.cursor = n + 1;
    } else if expected {
        sequence.cursor += 1;
    }
    sequence
        .next_index
        .entry(index_key)
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
