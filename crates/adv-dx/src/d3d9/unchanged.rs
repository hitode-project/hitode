//! Setters and state block transfers that change nothing return before any
//! ABI work, exactly where their handlers would.
use super::*;

/// `api` behind a check that completes, with D3D_OK, a call `unchanged`
/// finds changes nothing; any other call runs `api` itself.
macro_rules! unless_unchanged {
    ($api:expr, $unchanged:path) => {{
        const API: adv_abi::Api = $api;
        fn handler(env: &mut Env<'_>) -> Outcome {
            if $unchanged(env, API.name) {
                return Outcome::Return(0);
            }
            (API.handler)(env)
        }
        adv_abi::Api { handler, ..API }
    }};
}
pub(crate) use unless_unchanged;

/// Whether the Capture or Apply `name` of the block transfers nothing.
pub(super) fn state_block(env: &mut Env<'_>, name: &str) -> bool {
    env.cpu
        .arg(0)
        .is_ok_and(|this| shaders::stateblocks::unchanged(env, this, name.ends_with("Capture")))
}

/// Whether the setter `name` rebinds what is already bound.
pub(super) fn binding(env: &mut Env<'_>, name: &str) -> bool {
    rebinds(env, name).unwrap_or(false)
}

fn rebinds(env: &Env<'_>, name: &str) -> Option<bool> {
    let arg = |n| env.cpu.arg(n).ok();
    let this = arg(0)?;
    let d = device(env, this)?;
    // Recording the current value must still capture membership.
    if d.recording.is_some() {
        return Some(false);
    }
    let (old, pointer) = match name {
        "IDirect3DDevice9::SetFVF" => (d.buffers.declaration, 0),
        "IDirect3DDevice9::SetVertexDeclaration" => (d.buffers.declaration, arg(1)?),
        "IDirect3DDevice9::SetVertexShader" => (d.shaders.vertex, arg(1)?),
        "IDirect3DDevice9::SetPixelShader" => (d.shaders.pixel, arg(1)?),
        "IDirect3DDevice9::SetIndices" => (d.buffers.indices, arg(1)?),
        "IDirect3DDevice9::SetStreamSource" => {
            (d.buffers.streams.get(arg(1)? as usize)?.buffer, arg(2)?)
        }
        "IDirect3DDevice9::SetTexture" => {
            let slot = programmable::sampler_slot(arg(1)?)?;
            let pointer = arg(2)?;
            // Binding a managed texture uploads its dirty or cold levels.
            if resources::resource(env, pointer).is_some_and(|r| {
                r.kind == resources::Kind::Texture
                    && r.desc.Pool == 1
                    && (residency::cold(env, pointer) || r.levels.iter().any(|l| l.dirty.is_some()))
            }) {
                return Some(false);
            }
            let old = if slot == 0 {
                d.texture
            } else {
                d.shaders.textures[slot]
            };
            (old, pointer)
        }
        _ => unreachable!("registered binding check"),
    };
    let new = match pointer {
        0 => None,
        // Invalid incoming objects cannot replace any binding.
        pointer => Some(com(env).identity::<Payload>(&env.sys.objects, pointer)?),
    };
    if old != new {
        return Some(false);
    }
    Some(match name {
        // FVF 0 is never accepted; any other bound FVF was validated.
        "IDirect3DDevice9::SetFVF" => d.fixed.fvf != 0 && arg(1) == Some(d.fixed.fvf),
        "IDirect3DDevice9::SetVertexDeclaration" => d.fixed.fvf == 0,
        "IDirect3DDevice9::SetStreamSource" => {
            let stream = &d.buffers.streams[arg(1)? as usize];
            new.is_none() || (arg(3) == Some(stream.offset) && arg(4) == Some(stream.stride))
        }
        _ => true,
    })
}

#[cfg(test)]
mod tests;
