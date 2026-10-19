//! Managed textures retain their one guest CPU allocation while the shared
//! surface store owns disposable GPU mip views. No shader/pipeline state or
//! second resource owner is introduced here.
use super::*;
use resources::Kind;

const API: &str = "d3d9.dll!IDirect3DDevice9::EvictManagedResources";

pub(super) fn register(registry: &mut Registry) {
    registry.register(evict_ABI);
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, API, reason);
    UNAVAILABLE
}

/// Update the parent and every already published borrowed mip interface as a
/// single metadata operation. Those interfaces still share CPU storage/locks.
fn surfaces(env: &mut Env<'_>, id: Id, views: &[Option<u32>], dirty: bool) {
    let aliases = {
        let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
            return;
        };
        r.surface = views.first().copied().flatten();
        let mut aliases = Vec::new();
        for (level, &surface) in r.levels.iter_mut().zip(views) {
            level.surface = surface;
            if dirty {
                level.mark_all();
            }
            if let Some(alias) = level.interface {
                aliases.push((alias, surface));
            }
        }
        aliases
    };
    for (id, surface) in aliases {
        if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
            r.surface = surface;
        }
    }
}

#[win32(d3d9, name = "IDirect3DDevice9::EvictManagedResources", level = implement)]
fn evict(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let device_id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let bound: Vec<_> = d
        .texture
        .into_iter()
        .chain(d.shaders.textures.iter().copied().flatten())
        .collect();
    let recording = d.recording.is_some();
    let identities: Vec<_> = env
        .sys
        .objects
        .hosts_in::<adv_os::com::ComObject<Payload>>(com(env).namespace())
        .map(|(id, _)| id)
        .collect();
    let mut textures = Vec::new();
    // Admission covers the entire device before removing a single GPU view.
    for id in identities {
        let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
            continue;
        };
        if r.device != device_id || r.desc.Pool != 1 || r.container.is_some() {
            continue;
        }
        if r.surface.is_none() && r.levels.iter().all(|level| level.surface.is_none()) {
            continue;
        }
        if r.kind != Kind::Texture || r.storage == 0 || r.levels.is_empty() || !r.owns_surface {
            return unavailable(
                env,
                "managed GPU resource has no restorable shared texture/CPU owner",
            );
        }
        if bound.contains(&id) {
            return unavailable(
                env,
                "eviction of currently bound managed textures is unavailable",
            );
        }
        if resources::locked(env, r) {
            return unavailable(
                env,
                "managed texture lock, DC lease or texture transfer prevents eviction",
            );
        }
        for level in &r.levels {
            let Some(bytes) =
                format::length(level.desc.Format, level.desc.Width, level.desc.Height)
            else {
                return INVALID;
            };
            let Some(address) = r.storage.checked_add(level.offset) else {
                return INVALID;
            };
            if adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ).is_err() {
                return INVALID;
            }
        }
        textures.push((id, r.levels.clone()));
    }
    // CPU-only vertex/index storage and already cold textures have no GPU
    // copies to evict. They do not require an invented residency allocation.
    if textures.is_empty() {
        return 0;
    }
    if recording || shaders::stateblocks::has_outstanding(env, device_id) {
        return unavailable(
            env,
            "managed eviction with recorded/captured state blocks is unavailable",
        );
    }
    if !env.ext.get::<State>().pending.is_empty() || !env.ext.get::<State>().transfers.is_empty() {
        return unavailable(
            env,
            "pending GPU command or readback prevents managed eviction",
        );
    }
    let provider = env.ext.get::<State>().provider;
    // Temporary strong targets keep the original GPU copies alive through the
    // batch. An unexpected provider rejection can restore removed views before
    // these targets are dropped; normal admission failures make no changes.
    let mut saved = Vec::new();
    for (id, levels) in &textures {
        for (index, level) in levels.iter().enumerate() {
            if let Some(surface) = level.surface {
                let Some(target) = (provider.target)(env.ext, surface) else {
                    return INVALID;
                };
                saved.push((*id, index, surface, target));
            }
        }
    }
    for (removed, (_, _, surface, _)) in saved.iter().enumerate() {
        if let Err(error) = (provider.remove)(env.ext, env.cpu, *surface) {
            // Runtime removal fails only for an absent identity, preflighted
            // above. Retain truthful metadata even for a rejecting provider.
            let mut rollback_failed = false;
            for (id, index, _, target) in &saved[..removed] {
                let replacement = (provider.insert)(env.ext, target.clone()).ok();
                rollback_failed |= replacement.is_none();
                if let Some(Payload::Resource(r)) =
                    com(env).get_identity_mut(&mut env.sys.objects, *id)
                {
                    r.levels[*index].surface = replacement;
                }
            }
            for (id, _) in &textures {
                let views: Vec<_> = match com(env).get_identity(&env.sys.objects, *id) {
                    Some(Payload::Resource(r)) => {
                        r.levels.iter().map(|level| level.surface).collect()
                    }
                    _ => continue,
                };
                surfaces(env, *id, &views, rollback_failed);
            }
            return unavailable(
                env,
                &format!(
                    "shared GPU view retirement failed (rollback_failed={rollback_failed}): {error}"
                ),
            );
        }
    }
    for (id, levels) in textures {
        surfaces(env, id, &vec![None; levels.len()], true);
    }
    // Dropping saved retires the last owned GPU views. Submitted wgpu commands
    // keep their own backend lifetime; no queued image is explicitly destroyed.
    0
}

pub(super) fn cold(env: &Env<'_>, pointer: u32) -> bool {
    resources::resource(env, pointer).is_some_and(|r| {
        r.kind == Kind::Texture
            && r.desc.Pool == 1
            && r.levels.iter().any(|level| level.surface.is_none())
    })
}

/// Allocate a complete new mip chain, publishing no partial chain on failure.
/// Actual CPU bytes are uploaded by the existing dirty/dispatch lifecycle.
pub(super) fn ensure(env: &mut Env<'_>, pointer: u32) -> u32 {
    if !cold(env, pointer) {
        return 0;
    }
    let r = resources::resource(env, pointer).unwrap();
    if r.storage == 0 || r.levels.is_empty() || r.levels.iter().any(|level| level.lock.is_some()) {
        return INVALID;
    }
    // A cold allocation may have had its guest protection changed since
    // eviction. Validate the complete CPU chain before allocating any GPU view.
    for level in &r.levels {
        let Some(bytes) = format::length(level.desc.Format, level.desc.Width, level.desc.Height)
        else {
            return INVALID;
        };
        let Some(address) = r.storage.checked_add(level.offset) else {
            return INVALID;
        };
        if adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ).is_err() {
            return INVALID;
        }
    }
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, pointer)
        .unwrap();
    let (width, height, format, count) = (
        r.desc.Width,
        r.desc.Height,
        r.desc.Format,
        r.levels.len() as u32,
    );
    let old: Vec<_> = r.levels.iter().filter_map(|level| level.surface).collect();
    let Some(gpu) = env.ext.get::<State>().gpu.as_ref() else {
        adv_abi::warn_unavailable(
            env,
            "d3d9.dll!IDirect3DDevice9::SetTexture",
            "managed texture restoration requires the installed GPU backend",
        );
        return UNAVAILABLE;
    };
    let target = match gpu.target_levels(width, height, count) {
        Ok(target) => target.with_alpha_mode(targets::alpha_mode(format)),
        Err(error) => {
            return match gpu_error(error) {
                Outcome::Return(error) => error,
                _ => UNAVAILABLE,
            };
        }
    };
    let provider = env.ext.get::<State>().provider;
    let mut views = Vec::new();
    for level in 0..count {
        let view = if level == 0 {
            target.clone()
        } else {
            target.mip_level(level).unwrap()
        };
        match (provider.insert)(env.ext, view) {
            Ok(surface) => views.push(Some(surface)),
            Err(_) => {
                for surface in views.into_iter().flatten() {
                    let _ = (provider.remove)(env.ext, env.cpu, surface);
                }
                return OUT_OF_MEMORY;
            }
        }
    }
    surfaces(env, id, &views, true);
    for surface in old {
        if let Err(error) = (provider.remove)(env.ext, env.cpu, surface) {
            log::warn!(
                "managed residency replaced a stale GPU view but retirement failed: {error}"
            );
        }
    }
    0
}

#[cfg(test)]
mod tests;
