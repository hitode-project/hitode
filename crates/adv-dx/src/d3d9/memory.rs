//! Texture-memory estimates, without presenting CPU arena space as host VRAM.
use super::*;

pub(super) fn register(registry: &mut Registry) {
    registry.register(available_ABI);
}

#[win32(d3d9,name="IDirect3DDevice9::GetAvailableTextureMem",level=safe_fail)]
fn available(env: &mut Env<'_>, this: GuestPtr<()>) -> u32 {
    let valid = env.ext.try_get::<State>().is_some() && device(env, this.addr()).is_some();
    adv_abi::warn_unavailable(
        env,
        "d3d9.dll!IDirect3DDevice9::GetAvailableTextureMem",
        if valid {
            // The GPU has transfer/command limits but no texture-memory budget.
            // SurfaceStore's growable guest CPU arena is a separate allocator.
            // This UINT API returns an approximate, whole-MB estimate, not an
            // allocation guarantee. Until a virtual texture budget is enforced,
            // zero is the conservative estimate; no host VRAM value is invented.
            "texture-memory budget is unavailable; conservative estimate is 0 bytes"
        } else {
            "invalid device interface; texture-memory estimate is 0 bytes"
        },
    );
    0
}

#[cfg(test)]
mod tests;
