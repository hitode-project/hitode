//! Explicit GDI access borrows the existing lock/readback and upload paths.
//! A lease owns its controlling resource and device, including across waits.
use super::*;
use adv_abi::types::D3DLOCKED_RECT;
use adv_device::surface::Rect;
use resources::{Kind, Lock};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Acquiring,
    Active,
    Releasing,
}
#[derive(Clone, Copy)]
pub(super) struct Lease {
    context: u32,
    pointer: u32,
    owner: Id,
    device: Id,
    level: Option<u32>,
    width: u32,
    height: u32,
    scratch: u32,
    output: GuestPtr<u32>,
    dc: u32,
    phase: Phase,
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
    registry.register(release_ABI);
}
pub(super) fn leased(env: &Env<'_>, r: &Resource) -> bool {
    let owner = r
        .container
        .or_else(|| com(env).identity::<Payload>(&env.sys.objects, r.pointer));
    owner.is_some_and(|owner| env.ext.get::<State>().dcs.iter().any(|l| l.owner == owner))
}

#[win32(d3d9, name = "IDirect3DSurface9::GetDC", level = implement)]
fn get(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> Outcome {
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(r) = resources::resource(env, this.addr()).filter(|r| r.kind == Kind::Surface) else {
        return Outcome::Return(INVALID);
    };
    // X8R8G8B8 is the currently implemented non-alpha color format. SCRATCH,
    // depth and multisample surfaces cannot lend a GDI DC. Other supported
    // color resources (21/26) contain alpha and are explicitly ineligible.
    if r.desc.Format != 22
        || r.desc.Pool == 3
        || r.depth.is_some()
        || r.desc.MultiSampleType != 0
        || r.desc.MultiSampleQuality != 0
        || resources::locked(env, r)
    {
        return Outcome::Return(INVALID);
    }
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let owner = resources::binding_owner(env, id);
    if let Some(Payload::Resource(parent)) = com(env).get_identity(&env.sys.objects, owner)
        && (parent.lock.is_some() || parent.levels.iter().any(|l| l.lock.is_some()))
    {
        return Outcome::Return(INVALID);
    }
    let device = r.device;
    let level = r.container.map(|_| r.level);
    let (width, height) = (r.desc.Width, r.desc.Height);
    if !com(env).retain_internal(&mut env.sys.objects, device) {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    if !com(env).retain_internal(&mut env.sys.objects, owner) {
        drop_ref(env, device);
        return Outcome::Return(OUT_OF_MEMORY);
    }
    let Some(scratch) = alloc(env, 8) else {
        drop_ref(env, owner);
        drop_ref(env, device);
        return Outcome::Return(OUT_OF_MEMORY);
    };
    // Admission above guarantees there is no other lock. Insert the lease
    // after the internal lock call so its ordinary admission can be reused.
    let outcome = readback::lock_internal(env, this, GuestPtr::new(scratch), GuestPtr::new(0), 0);
    env.ext.get_mut::<State>().dcs.push(Lease {
        context: env.context,
        pointer: this.addr(),
        owner,
        device,
        level,
        width,
        height,
        scratch,
        output,
        dc: 0,
        phase: Phase::Acquiring,
    });
    acquired(env, owner, outcome)
}
fn acquired(env: &mut Env<'_>, owner: Id, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Return(0) => {
            let lease = *env
                .ext
                .get::<State>()
                .dcs
                .iter()
                .find(|l| l.owner == owner)
                .unwrap();
            let result = publish(env, lease);
            if result != 0 {
                retire(env, owner);
            }
            Outcome::Return(result)
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
                acquired(env, owner, outcome)
            }),
        },
        other => {
            retire(env, owner);
            other
        }
    }
}
fn publish(env: &mut Env<'_>, lease: Lease) -> u32 {
    if adv_abi::guest::check_range(env.cpu, lease.output.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let Ok(locked) = GuestPtr::<D3DLOCKED_RECT>::new(lease.scratch).read(env.cpu) else {
        return INVALID;
    };
    let Some(length) = (locked.Pitch as u32).checked_mul(lease.height) else {
        return INVALID;
    };
    if locked.Pitch <= 0
        || adv_abi::guest::check_range(env.cpu, locked.pBits, length, Prot::READ).is_err()
    {
        return INVALID;
    }
    let provider = env.ext.get::<State>().provider;
    let dc = match (provider.acquire_dc)(
        env,
        lease.width,
        lease.height,
        locked.pBits,
        locked.Pitch as u32,
    ) {
        Ok(dc) if dc != 0 => dc,
        result => {
            adv_abi::warn_unavailable(
                env,
                "IDirect3DSurface9::GetDC",
                &format!("GDI canvas allocation failed: {result:?}"),
            );
            return OUT_OF_MEMORY;
        }
    };
    let selected = env
        .ext
        .get_mut::<State>()
        .dcs
        .iter_mut()
        .find(|l| l.owner == lease.owner)
        .unwrap();
    selected.dc = dc;
    if lease.output.write(env.cpu, dc).is_err() {
        return INVALID;
    }
    selected.phase = Phase::Active;
    0
}

#[win32(d3d9, name = "IDirect3DSurface9::ReleaseDC", level = implement)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>, dc: Handle) -> Outcome {
    let Some(lease) = env
        .ext
        .get::<State>()
        .dcs
        .iter()
        .find(|l| l.pointer == this.addr() && l.dc == dc.0 && l.phase == Phase::Active)
        .copied()
    else {
        return Outcome::Return(INVALID);
    };
    let Some(r) = resources::resource(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Some(storage) = resources::storage(env, r) else {
        return Outcome::Return(INVALID);
    };
    let pitch = format::pitch(22, lease.width).unwrap();
    let length = pitch * lease.height;
    if adv_abi::guest::check_range(env.cpu, storage, length, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let provider = env.ext.get::<State>().provider;
    if (provider.writeback_dc)(env, dc.0, storage, pitch).is_err() {
        return Outcome::Return(INVALID);
    }
    let selected = env
        .ext
        .get_mut::<State>()
        .dcs
        .iter_mut()
        .find(|l| l.owner == lease.owner)
        .unwrap();
    selected.phase = Phase::Releasing;
    selected.context = env.context;
    let outcome = readback::unlock_internal(env, this);
    released(env, lease.owner, outcome)
}
fn released(env: &mut Env<'_>, owner: Id, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Return(0) => {
            retire(env, owner);
            Outcome::Return(0)
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
                released(env, owner, outcome)
            }),
        },
        Outcome::Return(error) => {
            let lease = *env
                .ext
                .get::<State>()
                .dcs
                .iter()
                .find(|l| l.owner == owner)
                .unwrap();
            reserve(env, lease, true);
            env.ext
                .get_mut::<State>()
                .dcs
                .iter_mut()
                .find(|l| l.owner == owner)
                .unwrap()
                .phase = Phase::Active;
            Outcome::Return(error)
        }
        other => {
            retire(env, owner);
            other
        }
    }
}
fn reserve(env: &mut Env<'_>, lease: Lease, locked: bool) {
    let lock = locked.then_some(Lock {
        rectangle: Rect {
            x: 0,
            y: 0,
            w: lease.width,
            h: lease.height,
        },
        flags: 0,
        ready: true,
    });
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, lease.owner)
    {
        if let Some(level) = lease.level {
            r.levels[level as usize].lock = lock;
        } else {
            r.lock = lock;
        }
    }
}
fn drop_ref(env: &mut Env<'_>, id: Id) {
    if let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id) {
        destroy(env, dead);
    }
}
fn retire(env: &mut Env<'_>, owner: Id) {
    let Some(index) = env
        .ext
        .get::<State>()
        .dcs
        .iter()
        .position(|l| l.owner == owner)
    else {
        return;
    };
    let lease = env.ext.get_mut::<State>().dcs.swap_remove(index);
    reserve(env, lease, false);
    if lease.dc != 0 {
        let provider = env.ext.get::<State>().provider;
        (provider.release_dc)(env, lease.dc);
    }
    free(env, lease.scratch);
    drop_ref(env, lease.owner);
    drop_ref(env, lease.device);
}
pub(super) fn reap_cancelled(env: &mut Env<'_>) {
    let cancelled: Vec<_> = env
        .ext
        .get::<State>()
        .dcs
        .iter()
        .filter(|l| {
            l.phase != Phase::Active
                && env
                    .sys
                    .contexts
                    .get(l.context as usize)
                    .is_none_or(|c| c.is_done())
        })
        .map(|l| l.owner)
        .collect();
    for owner in cancelled {
        retire(env, owner);
    }
}
pub(super) fn cancel_all(env: &mut Env<'_>) {
    while let Some(owner) = env.ext.get::<State>().dcs.last().map(|l| l.owner) {
        retire(env, owner);
    }
}

#[cfg(test)]
mod tests;
