//! Texture dirtiness lives in the shared mip metadata as each level's bounding
//! rectangle; only that rectangle is uploaded, after every lock is released.
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
    let mut region = None;
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
        region = Some(Rect {
            x: rectangle.left as u32,
            y: rectangle.top as u32,
            w: (rectangle.right - rectangle.left) as u32,
            h: (rectangle.bottom - rectangle.top) as u32,
        });
    }
    // An upload can complete before its ReleaseDC continuation runs. Keep
    // dirtiness, but do not start another upload while the DC lease is live.
    let deferred = dc::leased(env, r);
    let Some(Payload::Resource(r)) = com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
    else {
        return Outcome::Return(INVALID);
    };
    // AddDirtyRect names a level-zero region; D3D9 scales it to every level.
    for (index, level) in r.levels.iter_mut().enumerate() {
        match region {
            Some(rect) => level.mark_scaled(rect, index as u32),
            None => level.mark_all(),
        }
    }
    if deferred {
        Outcome::Return(0)
    } else {
        flush(env, this.addr())
    }
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
    let pointer = r.pointer;
    let restored = residency::ensure(env, pointer);
    if restored != 0 {
        return Outcome::Return(restored);
    }
    let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
        return Outcome::Return(INVALID);
    };
    let Some((index, selected, rectangle)) = r
        .levels
        .iter()
        .enumerate()
        .find_map(|(index, level)| Some((index as u32, *level, level.dirty?)))
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
    // Read only the dirty rows, from the first dirty texel of the first row
    // to the last of the final one.
    let format = selected.desc.Format;
    let (Some(pitch), Some(row), Some(bytes)) = (
        format::pitch(format, selected.desc.Width),
        format::pitch(format, rectangle.w),
        format::bytes(format),
    ) else {
        return Outcome::Return(INVALID);
    };
    let size = (rectangle.h - 1) * pitch + row;
    let Some(address) = r
        .storage
        .checked_add(selected.offset + rectangle.y * pitch + rectangle.x * bytes)
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
    let Some((pixels, pitch)) = format::upload(format, pixels, pitch, rectangle.w, rectangle.h)
    else {
        return Outcome::Return(INVALID);
    };
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        // Clear before submission so a later AddDirtyRect cannot be erased by
        // this upload's completion. Its original bytes already belong to the op.
        r.levels[index as usize].dirty = None;
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
            pixels: Arc::new(pixels),
            pitch,
            rectangle,
        },
    );
    uploaded(env, id, index, rectangle, outcome)
}

fn uploaded(env: &mut Env<'_>, id: Id, index: u32, rectangle: Rect, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Return(0) => next(env, id),
        Outcome::Return(error) => {
            if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id)
            {
                r.levels[index as usize].mark(rectangle);
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
                uploaded(env, id, index, rectangle, outcome)
            }),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests;
