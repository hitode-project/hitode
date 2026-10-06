//! Texture dirtiness lives in the shared mip metadata. Regions may expand to
//! complete levels; CPU storage is uploaded only after every lock is released.
use super::*;
use adv_device::surface::Rect;
use resources::{Kind, Lock};

pub(super) fn register(registry: &mut Registry) {
    registry.register(add_ABI);
}

#[win32(d3d9, name = "IDirect3DTexture9::AddDirtyRect", level = implement)]
fn add(env: &mut Env<'_>, this: GuestPtr<()>, rectangle: GuestPtr<RECT>) -> Outcome {
    let Some(r) = resources::resource(env, this.addr())
        .filter(|r| r.kind == Kind::Texture && matches!(r.desc.Pool, 1 | 2))
    else {
        return Outcome::Return(INVALID);
    };
    if !rectangle.is_null() {
        let Ok(rectangle) = rectangle.read(env.cpu) else {
            return Outcome::Return(INVALID);
        };
        if rectangle.left < 0
            || rectangle.top < 0
            || rectangle.right <= rectangle.left
            || rectangle.bottom <= rectangle.top
            || rectangle.right as u32 > r.desc.Width
            || rectangle.bottom as u32 > r.desc.Height
        {
            return Outcome::Return(INVALID);
        }
    }
    let Some(Payload::Resource(r)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return Outcome::Return(INVALID);
    };
    // Only level zero is specified by AddDirtyRect; expanding all its scaled
    // mip regions to full levels is a safe, bounded synchronization policy.
    for level in &mut r.levels {
        level.dirty = true;
    }
    flush(env, this.addr())
}

pub(super) fn flush(env: &mut Env<'_>, pointer: u32) -> Outcome {
    let Some(id) = com(env).identity::<Payload>(&env.sys.objects, pointer) else {
        return Outcome::Return(INVALID);
    };
    next(env, id)
}

fn next(env: &mut Env<'_>, id: Id) -> Outcome {
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return Outcome::Return(INVALID);
    };
    if r.kind != Kind::Texture {
        return Outcome::Return(INVALID);
    }
    // System-memory textures keep dirty metadata for a future explicit copy;
    // they have no GPU image and cannot be sampled by the existing device.
    if r.desc.Pool >= 2 || r.levels.iter().any(|level| level.lock.is_some()) {
        return Outcome::Return(0);
    }
    let Some((index, selected)) = r
        .levels
        .iter()
        .enumerate()
        .find(|(_, level)| level.dirty)
        .map(|(index, level)| (index as u32, *level))
    else {
        return Outcome::Return(0);
    };
    let state = env.ext.get::<State>();
    if state.gpu.is_none() {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DTexture9::AddDirtyRect",
            "dirty texture synchronization requires the installed GPU backend",
        );
        return Outcome::Return(UNAVAILABLE);
    }
    let Some(target) = selected
        .surface
        .and_then(|surface| (state.provider.target)(env.ext, surface))
    else {
        return Outcome::Return(INVALID);
    };
    let pitch = selected.desc.Width * 4;
    let size = pitch * selected.desc.Height;
    let Some(address) = r
        .storage
        .checked_add(selected.offset)
        .filter(|_| r.storage != 0)
    else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, address, size, Prot::READ).is_err() {
        return Outcome::Return(INVALID);
    }
    let mut pixels = vec![0; size as usize];
    if env.cpu.read(address, &mut pixels).is_err() {
        return Outcome::Return(INVALID);
    }
    if r.desc.Format == 22 {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    let rectangle = Rect {
        x: 0,
        y: 0,
        w: selected.desc.Width,
        h: selected.desc.Height,
    };
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        // Clear before submission so a later AddDirtyRect cannot be erased by
        // this upload's completion. Its original bytes already belong to the op.
        r.levels[index as usize].dirty = false;
        r.levels[index as usize].lock = Some(Lock {
            rectangle,
            flags: 0,
            ready: false,
        });
    }
    let outcome = dispatch(
        env,
        id,
        Operation::Upload {
            resource: id,
            level: Some(index),
            target,
            pixels,
            pitch,
            rectangle,
        },
    );
    uploaded(env, id, index, outcome)
}

fn uploaded(env: &mut Env<'_>, id: Id, index: u32, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Return(0) => next(env, id),
        Outcome::Return(error) => {
            if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id)
            {
                r.levels[index as usize].dirty = true;
                r.levels[index as usize].lock = None;
            }
            Outcome::Return(error)
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
                uploaded(env, id, index, outcome)
            }),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests;
