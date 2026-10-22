//! D3DX image transfers reuse D3D9 allocation, ownership and upload/readback lanes.
//! Unpublished textures and staging surfaces are retired on cancellation.
use super::*;
use std::rc::{Rc, Weak};

#[derive(Default)]
struct Loading {
    pending: Vec<(u32, Weak<()>)>,
}

pub(crate) fn valid_device(env: &Env<'_>, pointer: u32) -> bool {
    env.ext.try_get::<State>().is_some() && device(env, pointer).is_some()
}

pub(crate) fn texture_caps(env: &Env<'_>, pointer: u32) -> Result<adv_abi::types::D3DCAPS9, u32> {
    if !valid_device(env, pointer) {
        return Err(INVALID);
    }
    super::capabilities::caps(env)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_empty(
    env: &mut Env<'_>,
    device: u32,
    width: u32,
    height: u32,
    levels: u32,
    usage: u32,
    format: u32,
    pool: i32,
    output: GuestPtr<u32>,
) -> Outcome {
    resources::create_texture(
        env,
        GuestPtr::new(device),
        width,
        height,
        levels,
        usage,
        format,
        pool,
        output,
        GuestPtr::default(),
    )
}

pub(crate) fn release_texture(env: &mut Env<'_>, pointer: u32) {
    release(env, pointer);
}

/// Returns the real texture pointer on success, an HRESULT on failure. The
/// caller publishes this pointer only after the initial shared upload completes.
#[cfg(test)]
pub(crate) fn create(
    env: &mut Env<'_>,
    device: u32,
    width: u32,
    height: u32,
    format: u32,
    pool: i32,
    pixels: Vec<u8>,
) -> Outcome {
    create_mipmaps(env, device, width, height, format, pool, vec![pixels])
}

pub(crate) fn create_mipmaps(
    env: &mut Env<'_>,
    device: u32,
    width: u32,
    height: u32,
    format: u32,
    pool: i32,
    pixels: Vec<Vec<u8>>,
) -> Outcome {
    let Some(raw): Option<Vec<_>> = pixels
        .into_iter()
        .map(|pixels| super::format::readback(format, pixels))
        .collect()
    else {
        return Outcome::Return(INVALID);
    };
    if raw.is_empty() {
        return Outcome::Return(INVALID);
    }
    let Some(slot) = alloc(env, 4) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let outcome = resources::create_texture(
        env,
        GuestPtr::new(device),
        width,
        height,
        raw.len() as u32,
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
    let resource = resources::resource(env, pointer).unwrap();
    let storage = resource.storage;
    let levels = resource.levels.clone();
    for (level, bytes) in levels.into_iter().zip(raw) {
        let target = storage + level.offset;
        if super::format::length(format, level.desc.Width, level.desc.Height)
            != Some(bytes.len() as u32)
            || adv_abi::guest::check_range(env.cpu, target, bytes.len() as u32, Prot::WRITE)
                .is_err()
            || env.cpu.write(target, &bytes).is_err()
        {
            release(env, pointer);
            return Outcome::Return(INVALID);
        }
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

/// Produce an owned CPU surface using the existing asynchronous readback lane.
/// The same unpublished-resource guard retires a cancelled temporary surface.
pub(crate) fn readable_surface(env: &mut Env<'_>, source: u32) -> Outcome {
    if env.ext.try_get::<State>().is_none() {
        return Outcome::Return(INVALID);
    }
    if let Some(outcome) = readback::settle(env, source, move |env| readable_surface(env, source)) {
        return outcome;
    }
    let Some(resource) = resources::resource(env, source) else {
        return Outcome::Return(INVALID);
    };
    if resource.kind != resources::Kind::Surface || resources::locked(env, resource) {
        return Outcome::Return(INVALID);
    }
    let desc = resource.desc;
    if desc.Pool != 0 {
        return Outcome::Return(if add_ref(env, source).is_some() {
            source
        } else {
            OUT_OF_MEMORY
        });
    }
    if desc.Usage & 1 == 0 || !matches!(desc.Format, 21 | 22) || desc.MultiSampleType != 0 {
        adv_abi::warn_unavailable(
            env,
            "d3dx9_43.dll!D3DXSaveSurfaceToFileInMemory",
            "GPU export requires a non-multisampled A8R8G8B8/X8R8G8B8 render target",
        );
        return Outcome::Return(INVALID);
    }
    let Some(Payload::Device(device)) = com(env).get_identity(&env.sys.objects, resource.device)
    else {
        return Outcome::Return(INVALID);
    };
    let device = device.pointer;
    let Some(slot) = alloc(env, 4) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let created = readback::create_offscreen(
        env,
        GuestPtr::new(device),
        desc.Width,
        desc.Height,
        desc.Format,
        2,
        GuestPtr::new(slot),
        GuestPtr::new(0),
    );
    let pointer = GuestPtr::<u32>::new(slot).read(env.cpu).unwrap_or(0);
    free(env, slot);
    if !matches!(created, Outcome::Return(0)) || pointer == 0 {
        return created;
    }
    let guard = Rc::new(());
    env.ext
        .get_or_default::<Loading>()
        .pending
        .push((pointer, Rc::downgrade(&guard)));
    let outcome = readback::transfer(
        env,
        GuestPtr::new(device),
        GuestPtr::new(source),
        GuestPtr::new(pointer),
    );
    finish(env, pointer, guard, outcome)
}
