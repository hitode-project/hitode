//! SYSTEMMEM to DEFAULT updates over the existing mip layout and GPU uploads.
//! Dirty regions may conservatively expand to the complete matching mip chain.
use super::*;
use adv_device::surface::Rect;
use resources::{Kind, Lock};
use std::collections::VecDeque;

#[derive(Default)]
struct Transfers {
    active: Vec<Transfer>,
    scratch: Scratch,
}
// Retain at most two initialized CPU buffers, independently of live transfers.
// Larger updates still work, but do not leave their allocations in the cache.
const MAX_SCRATCH: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct Scratch {
    raw: Vec<u8>,
    pixels: Vec<u8>,
}
impl Scratch {
    fn keep(slot: &mut Vec<u8>, candidate: Vec<u8>) {
        if candidate.capacity() <= MAX_SCRATCH && candidate.capacity() > slot.capacity() {
            *slot = candidate;
        }
    }
}
struct Transfer {
    context: u32,
    device: Id,
    source: Id,
    destination: Id,
    source_dirty: Vec<Option<Rect>>,
    destination_dirty: Vec<Option<Rect>>,
    remaining: VecDeque<Upload>,
    current: Option<Commit>,
}
struct Upload {
    level: u32,
    target: Target,
    pixels: Arc<Vec<u8>>,
    pitch: u32,
    rectangle: Rect,
    commit: Commit,
}
struct Commit {
    level: u32,
    address: Option<u32>,
    raw: Arc<Vec<u8>>,
    converted: Option<Arc<Vec<u8>>>,
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(update_ABI);
}

/// The transaction reserves both complete textures, including borrowed views.
/// Upload completion clears an individual level lock before its continuation;
/// this reservation therefore lasts until the whole API call finishes.
pub(super) fn reserved(env: &Env<'_>, resource: &Resource) -> bool {
    let Some(transfers) = env.ext.try_get::<Transfers>() else {
        return false;
    };
    let owner = resource
        .container
        .or_else(|| com(env).identity::<Payload>(&env.sys.objects, resource.pointer));
    owner.is_some_and(|owner| {
        transfers
            .active
            .iter()
            .any(|t| t.source == owner || t.destination == owner)
    })
}

#[win32(d3d9, name = "IDirect3DDevice9::UpdateTexture", level = implement)]
fn update(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    source: GuestPtr<()>,
    destination: GuestPtr<()>,
) -> Outcome {
    let Some(device) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| super::device(env, this.addr()).is_some())
    else {
        return Outcome::Return(INVALID);
    };
    if env.ext.try_get::<Transfers>().is_none() {
        env.ext.insert(Transfers::default());
    }
    let mut scratch = std::mem::take(&mut env.ext.get_mut::<Transfers>().scratch);
    let prepared = prepare(env, device, source.addr(), destination.addr(), &mut scratch);
    let cached = &mut env.ext.get_mut::<Transfers>().scratch;
    Scratch::keep(&mut cached.raw, scratch.raw);
    Scratch::keep(&mut cached.pixels, scratch.pixels);
    let mut transfer = match prepared {
        Ok(Some(transfer)) => transfer,
        Ok(None) => return Outcome::Return(0),
        Err(error) => {
            if error == UNAVAILABLE {
                adv_abi::warn_unavailable(
                    env,
                    "IDirect3DDevice9::UpdateTexture",
                    "texture upload requires the installed GPU backend",
                );
            }
            return Outcome::Return(error);
        }
    };
    let identities = [device, transfer.source, transfer.destination];
    for (index, id) in identities.into_iter().enumerate() {
        if !com(env).retain_internal(&mut env.sys.objects, id) {
            for &retained in &identities[..index] {
                drop_ref(env, retained);
            }
            return Outcome::Return(OUT_OF_MEMORY);
        }
    }
    // Consume only the marks represented by this snapshot. Later AddDirtyRect
    // marks remain dirty through completion; failure restores these old marks.
    if let Some(Payload::Resource(r)) =
        com(env).get_identity_mut(&mut env.sys.objects, transfer.source)
    {
        for level in &mut r.levels {
            level.dirty = None;
        }
    }
    transfer.context = env.context;
    let destination = transfer.destination;
    env.ext.get_mut::<Transfers>().active.push(transfer);
    next(env, destination)
}

fn prepare(
    env: &Env<'_>,
    device: Id,
    source: u32,
    destination: u32,
    scratch: &mut Scratch,
) -> Result<Option<Transfer>, u32> {
    let src = resources::resource(env, source).ok_or(INVALID)?;
    let dst = resources::resource(env, destination).ok_or(INVALID)?;
    if src.kind != Kind::Texture
        || dst.kind != Kind::Texture
        || src.device != device
        || dst.device != device
        || src.desc.Pool != 2
        || dst.desc.Pool != 0
        || src.desc.Format != dst.desc.Format
        || resources::locked(env, src)
        || resources::locked(env, dst)
        || src.levels.is_empty()
        || dst.levels.is_empty()
        || src.levels.len() < dst.levels.len()
        || src.storage == 0
    {
        return Err(INVALID);
    }
    let offset = src.levels.len() - dst.levels.len();
    for (s, d) in src.levels[offset..].iter().zip(&dst.levels) {
        if s.desc.Width != d.desc.Width
            || s.desc.Height != d.desc.Height
            || s.desc.Format != d.desc.Format
            || s.desc.Type != d.desc.Type
        {
            return Err(INVALID);
        }
    }
    if !src.levels.iter().any(|level| level.dirty.is_some()) {
        return Ok(None);
    }
    let state = env.ext.get::<State>();
    if state.gpu.is_none() {
        return Err(UNAVAILABLE);
    }
    let mut remaining = VecDeque::with_capacity(dst.levels.len());
    // Snapshot and validate every matched level before any guest/GPU mutation.
    // A later unreadable mip must not cause an avoidable partial first upload.
    for (index, (s, d)) in src.levels[offset..].iter().zip(&dst.levels).enumerate() {
        let length = format::length(s.desc.Format, s.desc.Width, s.desc.Height).ok_or(INVALID)?;
        let pitch = format::pitch(s.desc.Format, s.desc.Width).ok_or(INVALID)?;
        let source = src.storage.checked_add(s.offset).ok_or(INVALID)?;
        adv_abi::guest::check_range(env.cpu, source, length, Prot::READ).map_err(|_| INVALID)?;
        let address = if dst.storage != 0 {
            let address = dst.storage.checked_add(d.offset).ok_or(INVALID)?;
            adv_abi::guest::check_range(env.cpu, address, length, Prot::WRITE)
                .map_err(|_| INVALID)?;
            Some(address)
        } else {
            None
        };
        let target = d
            .surface
            .and_then(|surface| (state.provider.target)(env.ext, surface))
            .ok_or(INVALID)?;
        let mut raw = std::mem::take(&mut scratch.raw);
        raw.resize(length as usize, 0);
        env.cpu.read(source, &mut raw).map_err(|_| INVALID)?;
        let raw = Arc::new(raw);
        let (pixels, pitch) = if s.desc.Format == 21 {
            // BGRA8 already matches the GPU layout. Keep one immutable owned
            // snapshot for both submission retries and guest shadow writeback.
            (Arc::clone(&raw), pitch)
        } else {
            let input = if s.desc.Format == 22 {
                let mut pixels = std::mem::take(&mut scratch.pixels);
                pixels.clone_from(raw.as_ref());
                pixels
            } else {
                raw.as_ref().clone()
            };
            let (pixels, pitch) =
                format::upload(s.desc.Format, input, pitch, s.desc.Width, s.desc.Height)
                    .ok_or(INVALID)?;
            (Arc::new(pixels), pitch)
        };
        let level = index as u32;
        let converted = (s.desc.Format == 22).then(|| Arc::clone(&pixels));
        remaining.push_back(Upload {
            level,
            target,
            pixels,
            pitch,
            rectangle: Rect {
                x: 0,
                y: 0,
                w: d.desc.Width,
                h: d.desc.Height,
            },
            commit: Commit {
                level,
                address,
                raw,
                converted,
            },
        });
    }
    Ok(Some(Transfer {
        context: 0,
        device,
        source: com(env)
            .identity::<Payload>(&env.sys.objects, src.pointer)
            .ok_or(INVALID)?,
        destination: com(env)
            .identity::<Payload>(&env.sys.objects, dst.pointer)
            .ok_or(INVALID)?,
        source_dirty: src.levels.iter().map(|l| l.dirty).collect(),
        destination_dirty: dst.levels.iter().map(|l| l.dirty).collect(),
        remaining,
        current: None,
    }))
}
fn index(env: &Env<'_>, destination: Id) -> usize {
    env.ext
        .get::<Transfers>()
        .active
        .iter()
        .position(|t| t.destination == destination)
        .expect("owned texture update")
}
fn next(env: &mut Env<'_>, destination: Id) -> Outcome {
    let index = index(env, destination);
    let transfer = &mut env.ext.get_mut::<Transfers>().active[index];
    let Some(upload) = transfer.remaining.pop_front() else {
        retire(env, destination, true);
        return Outcome::Return(0);
    };
    let device = transfer.device;
    transfer.current = Some(upload.commit);
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, destination)
    {
        r.levels[upload.level as usize].lock = Some(Lock {
            rectangle: upload.rectangle,
            flags: 0,
            ready: false,
        });
    }
    let outcome = dispatch(
        env,
        device,
        Operation::Upload {
            resource: destination,
            level: Some(upload.level),
            target: upload.target,
            pixels: Pixels::Host(upload.pixels),
            pitch: upload.pitch,
            rectangle: upload.rectangle,
        },
    );
    uploaded(env, destination, outcome)
}
fn uploaded(env: &mut Env<'_>, destination: Id, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Return(0) => {
            let index = index(env, destination);
            let commit = env.ext.get::<Transfers>().active[index]
                .current
                .as_ref()
                .unwrap();
            if let Some(address) = commit.address
                && (adv_abi::guest::check_range(
                    env.cpu,
                    address,
                    commit.raw.len() as u32,
                    Prot::WRITE,
                )
                .is_err()
                    || env.cpu.write(address, &commit.raw).is_err())
            {
                retire(env, destination, false);
                return Outcome::Return(INVALID);
            }
            let level = commit.level;
            if let Some(Payload::Resource(r)) =
                com(env).get_identity_mut(&mut env.sys.objects, destination)
            {
                r.levels[level as usize].dirty = None;
            }
            let commit = env.ext.get_mut::<Transfers>().active[index]
                .current
                .take()
                .unwrap();
            let scratch = &mut env.ext.get_mut::<Transfers>().scratch;
            if let Ok(raw) = Arc::try_unwrap(commit.raw) {
                Scratch::keep(&mut scratch.raw, raw);
            }
            if let Some(converted) = commit.converted
                && let Ok(pixels) = Arc::try_unwrap(converted)
            {
                Scratch::keep(&mut scratch.pixels, pixels);
            }
            next(env, destination)
        }
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let outcome = then(env, resume);
                uploaded(env, destination, outcome)
            }),
        },
        other => {
            retire(env, destination, false);
            other
        }
    }
}
fn drop_ref(env: &mut Env<'_>, id: Id) {
    if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
        destroy(env, dead);
    }
}
fn retire(env: &mut Env<'_>, destination: Id, success: bool) {
    let index = index(env, destination);
    let transfer = env.ext.get_mut::<Transfers>().active.swap_remove(index);
    if !success {
        if let Some(Payload::Resource(r)) =
            com(env).get_identity_mut(&mut env.sys.objects, transfer.source)
        {
            for (level, dirty) in r.levels.iter_mut().zip(transfer.source_dirty) {
                if let Some(rect) = dirty {
                    level.mark(rect);
                }
            }
        }
        if let Some(current) = transfer.current
            && let Some(Payload::Resource(r)) =
                com(env).get_identity_mut(&mut env.sys.objects, destination)
        {
            let level = &mut r.levels[current.level as usize];
            level.lock = None;
            level.dirty = transfer.destination_dirty[current.level as usize];
        }
    }
    // The device stays alive while either resource is being destroyed.
    for id in [transfer.source, transfer.destination, transfer.device] {
        drop_ref(env, id);
    }
}
pub(super) fn reap_cancelled(env: &mut Env<'_>) {
    let Some(transfers) = env.ext.try_get::<Transfers>() else {
        return;
    };
    let cancelled: Vec<_> = transfers
        .active
        .iter()
        .filter(|t| {
            env.sys
                .contexts
                .get(t.context as usize)
                .is_none_or(|c| c.is_done())
        })
        .map(|t| t.destination)
        .collect();
    for destination in cancelled {
        retire(env, destination, false);
    }
}
pub(super) fn cancel_all(env: &mut Env<'_>) {
    while let Some(destination) = env
        .ext
        .try_get::<Transfers>()
        .and_then(|t| t.active.last())
        .map(|t| t.destination)
    {
        retire(env, destination, false);
    }
}

#[cfg(test)]
mod tests;
