//! Reuse synchronous indexed-draw preparation, never ordered content updates.
use super::*;
use std::hash::Hash;

type StateStamp = ([u64; 15], [u64; 2], u128);

#[derive(Default)]
pub(super) struct Cache {
    generations: [HashMap<u128, Entry, std::hash::BuildHasherDefault<xxhash_rust::xxh3::Xxh3>>; 2],
    bytes: usize,
    entries: usize,
    state: std::cell::Cell<Option<(StateStamp, u128)>>,
}
struct Entry {
    variants: HashMap<u128, key::Prepared, std::hash::BuildHasherDefault<xxhash_rust::xxh3::Xxh3>>,
    spans: Vec<(u32, u32, u128)>,
}
impl Cache {
    pub(super) fn finish(&mut self) {
        self.generations.swap(0, 1);
        self.generations[0].clear();
        self.bytes = 0;
        self.entries = 0;
    }
}
fn signature(env: &mut Env<'_>, pointer: u32, request: (i32, u32, &[u32])) -> Result<u128, u32> {
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .ok_or(INVALID)?;
    key::bind(env, owner)?;
    let d = device(env, pointer).ok_or(INVALID)?;
    if !d.scene || d.needs_reset {
        return Err(INVALID);
    }
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    let stamp = (
        d.versions,
        [
            d.shaders.vertex_constants.revision(),
            d.shaders.pixel_constants.revision(),
        ],
        d.fixed.key(),
    );
    let cached = d.frame.reuse.state.get().filter(|(old, _)| *old == stamp);
    let state = cached.map(|(_, key)| key).unwrap_or_else(|| {
        let key = draw_key(env, pointer);
        d.frame.reuse.state.set(Some((stamp, key)));
        key
    });
    (request, state).hash(&mut h);
    let target = targets::draw_target(env, pointer)?;
    (
        d.render_target.or(d.back_buffer),
        d.depth_stencil,
        target.image.view(),
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
        d.frame.damage.written(content).hash(&mut h);
    }
    for content in &bindings.samples {
        (
            d.frame.contents.get(content),
            d.frame.damage.written(content),
        )
            .hash(&mut h);
    }
    Ok(h.digest128())
}
fn contents(env: &Env<'_>, spans: &[(u32, u32, u128)]) -> Option<u128> {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    spans.len().hash(&mut h);
    for &(address, length, _) in spans {
        adv_abi::guest::check_range(env.cpu, address, length, Prot::READ).ok()?;
        let bytes = match env.cpu.shared_bytes(address, length as usize) {
            Some(bytes) => std::borrow::Cow::Borrowed(bytes),
            None => {
                let mut bytes = vec![0; length as usize];
                env.cpu.read(address, &mut bytes).ok()?;
                std::borrow::Cow::Owned(bytes)
            }
        };
        (address, length, xxhash_rust::xxh3::xxh3_128(&bytes)).hash(&mut h);
    }
    Some(h.digest128())
}
pub(in crate::d3d9) fn indexed(
    env: &mut Env<'_>,
    pointer: u32,
    request: (i32, u32, &[u32]),
    build: impl FnOnce(&mut Env<'_>) -> Result<fixed::Draw, u32>,
) -> Outcome {
    #[cfg(feature = "d3d9-counters")]
    let before = lookups();
    let signature = match signature(env, pointer, request) {
        Ok(key) => key,
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
    let template = d
        .frame
        .reuse
        .generations
        .iter()
        .find_map(|g| g.get(&signature));
    let content = template.and_then(|entry| contents(env, &entry.spans));
    let cached = content.and_then(|content| {
        d.frame
            .reuse
            .generations
            .iter()
            .find_map(|g| g.get(&signature)?.variants.get(&content))
    });
    let (prepared, spans, content) = if let Some(prepared) = cached {
        (
            prepared.clone(),
            template.unwrap().spans.clone(),
            content.unwrap(),
        )
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
        let content = hash(&spans);
        (prepared, spans, content)
    };
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let cache = &mut frame(env, owner).reuse;
    if !spans.is_empty()
        && cache.generations[0]
            .get(&signature)
            .is_none_or(|e| !e.variants.contains_key(&content))
        && cache.entries < 4096
        && cache.bytes + prepared.bytes <= 64 * 1024 * 1024
    {
        cache.bytes += prepared.bytes;
        cache.entries += 1;
        cache.generations[0]
            .entry(signature)
            .or_insert_with(|| Entry {
                variants: Default::default(),
                spans,
            })
            .variants
            .insert(content, prepared.clone());
    }
    #[cfg(feature = "d3d9-counters")]
    let prepared = key::Prepared {
        lookups: lookups() - before,
        ..prepared
    };
    if env.ext.get::<State>().gpu.is_none() {
        return Outcome::Return(UNAVAILABLE);
    }
    barrier_if_busy(env, owner, move |env| accept(env, owner, prepared))
}
