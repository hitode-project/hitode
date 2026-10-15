//! Admission runs before ABI diagnostics, draw preparation or binding changes.
use super::*;

pub(super) fn draw(env: &mut Env<'_>) -> Option<u64> {
    let binding = env.sys.thunks.get(env.thunk).f;
    for word in 0..u32::from(binding.conv.argc()) {
        env.cpu.arg(word).ok()?;
    }
    let this = env.cpu.arg(0).ok()?;
    let name = env.sys.thunks.get(env.thunk).name.as_str();
    let (count_word, up) = match name {
        "IDirect3DDevice9::Clear" => (None, None),
        "IDirect3DDevice9::DrawPrimitive" => (Some(3), None),
        "IDirect3DDevice9::DrawIndexedPrimitive" => (Some(6), None),
        "IDirect3DDevice9::DrawPrimitiveUP" => (Some(2), Some(false)),
        "IDirect3DDevice9::DrawIndexedPrimitiveUP" => (Some(4), Some(true)),
        _ => unreachable!("registered draw leaf"),
    };
    let d = device(env, this)?;
    // UP clears these bindings, potentially freeing guest allocations.
    if let Some(indexed) = up
        && (d.buffers.streams[0].buffer.is_some() || (indexed && d.buffers.indices.is_some()))
    {
        return None;
    }
    let bytes = if let Some(word) = count_word {
        let count = env.cpu.arg(word).ok()?;
        let length = if count == 0 {
            0
        } else {
            fixed::primitive_length(env.cpu.arg(1).ok()? as i32, count).unwrap_or(0) as usize
        };
        // Fixed input expands points/fans and may retain both neutral and
        // programmable texture coordinates. Raw programmable streams have a
        // combined 16 MiB limit; their indices can expand a fan to triangles.
        length * 6 * (2 * (std::mem::size_of::<adv_device::gpu::Vertex>() + 8 * 16) + 4)
            + 16 * 1024 * 1024
    } else {
        0
    };
    if !recording::admits_leaf(env, this, bytes) {
        return None;
    }
    match (binding.handler)(env) {
        Outcome::Return(value) => {
            Some((u64::from(env.cpu.reg(adv_cpu::backend::Reg::Edx)) << 32) | u64::from(value))
        }
        _ => unreachable!("admitted draw cannot flush or wait"),
    }
}

fn invoke(env: &mut Env<'_>) -> Option<u64> {
    match (env.sys.thunks.get(env.thunk).f.handler)(env) {
        Outcome::Return(value) => {
            Some((u64::from(env.cpu.reg(adv_cpu::backend::Reg::Edx)) << 32) | u64::from(value))
        }
        _ => unreachable!("admitted state call cannot wait"),
    }
}
fn readable_arguments(env: &Env<'_>) -> Option<()> {
    for word in 0..u32::from(env.sys.thunks.get(env.thunk).f.conv.argc()) {
        env.cpu.arg(word).ok()?;
    }
    Some(())
}
pub(super) fn state_block(env: &mut Env<'_>) -> Option<u64> {
    readable_arguments(env)?;
    let name = env.sys.thunks.get(env.thunk).name.as_str();
    if !shaders::stateblocks::admits_leaf(env, env.cpu.arg(0).ok()?, name.ends_with("Capture")) {
        return None;
    }
    invoke(env)
}
pub(super) fn binding(env: &mut Env<'_>) -> Option<u64> {
    readable_arguments(env)?;
    let this = env.cpu.arg(0).ok()?;
    let d = device(env, this)?;
    let (shaders, buffers, texture) = d
        .recording
        .as_ref()
        .map_or((&d.shaders, &d.buffers, d.texture), |s| s.binding_state());
    let name = env.sys.thunks.get(env.thunk).name.as_str();
    let (old, pointer) = match name {
        "IDirect3DDevice9::SetFVF" => (buffers.declaration, 0),
        "IDirect3DDevice9::SetVertexDeclaration" => (buffers.declaration, env.cpu.arg(1).ok()?),
        "IDirect3DDevice9::SetVertexShader" => (shaders.vertex, env.cpu.arg(1).ok()?),
        "IDirect3DDevice9::SetPixelShader" => (shaders.pixel, env.cpu.arg(1).ok()?),
        "IDirect3DDevice9::SetIndices" => (buffers.indices, env.cpu.arg(1).ok()?),
        "IDirect3DDevice9::SetStreamSource" => {
            let Some(stream) = buffers.streams.get(env.cpu.arg(1).ok()? as usize) else {
                return invoke(env);
            };
            (stream.buffer, env.cpu.arg(2).ok()?)
        }
        "IDirect3DDevice9::SetTexture" => {
            let Some(slot) = programmable::sampler_slot(env.cpu.arg(1).ok()?) else {
                return invoke(env);
            };
            let pointer = env.cpu.arg(2).ok()?;
            if resources::resource(env, pointer).is_some_and(|r| {
                r.kind == resources::Kind::Texture
                    && r.desc.Pool == 1
                    && (residency::cold(env, pointer) || r.levels.iter().any(|l| l.dirty))
            }) {
                return None;
            }
            (
                if slot == 0 {
                    texture
                } else {
                    shaders.textures[slot]
                },
                pointer,
            )
        }
        _ => unreachable!("registered binding leaf"),
    };
    let new = if pointer == 0 {
        None
    } else {
        // Invalid incoming objects cannot replace any binding.
        let Some(id) = com(env).identity::<Payload>(&env.sys.objects, pointer) else {
            return invoke(env);
        };
        Some(id)
    };
    if old != new
        && old.is_some_and(|id| {
            com(env)
                .references(&env.sys.objects, id)
                .is_none_or(|n| n == 0)
        })
    {
        return None;
    }
    invoke(env)
}

#[cfg(test)]
mod binding_tests;
#[cfg(all(test, target_arch = "aarch64"))]
mod tests;
