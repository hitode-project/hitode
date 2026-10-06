//! Cursor requests fail explicitly until the presenter can carry cursor images.
use super::*;

pub(super) fn register(registry: &mut Registry) {
    registry.register(properties_ABI);
}

#[win32(d3d9,name="IDirect3DDevice9::SetCursorProperties",level=safe_fail)]
fn properties(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    hotspot_x: u32,
    hotspot_y: u32,
    bitmap: GuestPtr<()>,
) -> u32 {
    if env.ext.try_get::<State>().is_none() || device(env, this.addr()).is_none() {
        return INVALID;
    }
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let Some(resource) = resources::resource(env, bitmap.addr()) else {
        return INVALID;
    };
    let (width, height) = (resource.desc.Width, resource.desc.Height);
    let (display_width, display_height, _) = (env.ext.get::<State>().provider.display)(env.ext);
    if resource.kind != resources::Kind::Surface
        || resource.device != owner
        || resource.desc.Format != 21 // D3DFMT_A8R8G8B8
        || resource.depth.is_some()
        || !width.is_power_of_two()
        || !height.is_power_of_two()
        || width >= display_width
        || height >= display_height
        || hotspot_x >= width
        || hotspot_y >= height
        || resources::locked(env, resource)
    {
        return INVALID;
    }
    // SetCursorProperties must copy the surface into a displayed cursor. The
    // current provider has neither a host cursor channel nor software cursor
    // composition. Keeping bytes privately would not fulfill that contract.
    // Refuse without retaining the surface or changing USER32 cursor state.
    adv_abi::warn_unavailable(
        env,
        "d3d9.dll!IDirect3DDevice9::SetCursorProperties",
        "custom cursor presentation is unavailable",
    );
    UNAVAILABLE
}

#[cfg(test)]
mod tests;
