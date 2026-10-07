//! CPU image loaders use the same texture allocation, COM owner and upload path
//! as CreateTexture/UnlockRect. An unpublished texture is retired on cancellation.
use super::*;
use std::rc::{Rc, Weak};

#[derive(Default)]
struct Loading {
    pending: Vec<(u32, Weak<()>)>,
}

pub(crate) fn valid_device(env: &Env<'_>, pointer: u32) -> bool {
    env.ext.try_get::<State>().is_some() && device(env, pointer).is_some()
}

pub(crate) fn release_texture(env: &mut Env<'_>, pointer: u32) {
    release(env, pointer);
}

/// Returns the real texture pointer on success, an HRESULT on failure. The
/// caller publishes this pointer only after the initial shared upload completes.
pub(crate) fn create(
    env: &mut Env<'_>,
    device: u32,
    width: u32,
    height: u32,
    format: u32,
    pool: i32,
    pixels: Vec<u8>,
) -> Outcome {
    let Some(raw) = super::format::readback(format, pixels) else {
        return Outcome::Return(INVALID);
    };
    let Some(slot) = alloc(env, 4) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let outcome = resources::create_texture(
        env,
        GuestPtr::new(device),
        width,
        height,
        1,
        0,
        format,
        pool,
        GuestPtr::new(slot),
        GuestPtr::new(0),
    );
    let pointer = GuestPtr::<u32>::new(slot).read(env.cpu).unwrap_or(0);
    free(env, slot);
    match outcome {
        Outcome::Return(0) if pointer != 0 => {}
        other => return other,
    }
    let storage = resources::resource(env, pointer).unwrap().storage;
    if adv_abi::guest::check_range(env.cpu, storage, raw.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(storage, &raw).is_err()
    {
        release(env, pointer);
        return Outcome::Return(INVALID);
    }
    let guard = Rc::new(());
    env.ext
        .get_or_default::<Loading>()
        .pending
        .push((pointer, Rc::downgrade(&guard)));
    let outcome = dirty::flush(env, pointer);
    finish(env, pointer, guard, outcome)
}

fn finish(env: &mut Env<'_>, pointer: u32, guard: Rc<()>, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let result = then(env, resume);
                finish(env, pointer, guard, result)
            }),
        },
        other => {
            let pending = &mut env.ext.get_mut::<Loading>().pending;
            let Some(index) = pending.iter().position(|(p, _)| *p == pointer) else {
                // Process cancellation already retired this unpublished owner.
                // An abandoned upload must never publish a freed/reused address.
                return Outcome::Return(INVALID);
            };
            pending.swap_remove(index);
            drop(guard);
            if matches!(other, Outcome::Return(0)) {
                if resources::resource(env, pointer).is_some() {
                    Outcome::Return(pointer)
                } else {
                    Outcome::Return(INVALID)
                }
            } else {
                release(env, pointer);
                other
            }
        }
    }
}

pub(super) fn reap_cancelled(env: &mut Env<'_>) {
    let Some(loading) = env.ext.try_get_mut::<Loading>() else {
        return;
    };
    let mut retired = Vec::new();
    loading.pending.retain(|(pointer, guard)| {
        if guard.strong_count() == 0 {
            retired.push(*pointer);
            false
        } else {
            true
        }
    });
    for pointer in retired {
        release(env, pointer);
    }
}

pub(super) fn cancel_all(env: &mut Env<'_>) {
    if let Some(loading) = env.ext.try_get_mut::<Loading>() {
        let retired = std::mem::take(&mut loading.pending);
        for (pointer, _) in retired {
            release(env, pointer);
        }
    }
}

#[cfg(test)]
mod tests;
