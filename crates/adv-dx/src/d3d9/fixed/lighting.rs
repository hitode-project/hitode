use super::*;

pub(super) fn register(registry: &mut Registry) {
    for api in [
        set_light_ABI,
        get_light_ABI,
        light_enable_ABI,
        get_light_enable_ABI,
        set_material_ABI,
        get_material_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetLight",level=implement)]
pub(super) fn set_light(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    light: GuestPtr<[u32; 26]>,
) -> u32 {
    let Ok(value) = light.read(env.cpu) else {
        return INVALID;
    };
    let value = match state_map::light(value) {
        Ok(value) => value,
        Err(code) => {
            if code == UNAVAILABLE {
                adv_abi::warn_unavailable(
                    env,
                    "IDirect3DDevice9::SetLight",
                    "unsupported light type",
                );
            }
            return code;
        }
    };
    super::update_fixed(env, this.addr(), Ok(Setting::Light(index, value)))
}
#[win32(d3d9,name="IDirect3DDevice9::GetLight",level=implement)]
pub(super) fn get_light(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: GuestPtr<[u32; 26]>,
) -> u32 {
    let Some(value) =
        current(env, this.addr()).and_then(|f| f.lights.get(&index).map(|l| l.value.words()))
    else {
        return INVALID;
    };
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DDevice9::LightEnable",level=implement)]
pub(super) fn light_enable(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, enable: i32) -> u32 {
    super::update_fixed(
        env,
        this.addr(),
        Ok(Setting::LightEnable(index, enable != 0)),
    )
}
#[win32(d3d9,name="IDirect3DDevice9::GetLightEnable",level=implement)]
pub(super) fn get_light_enable(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: GuestPtr<i32>,
) -> u32 {
    let Some(enabled) =
        current(env, this.addr()).and_then(|f| f.lights.get(&index).map(|l| l.enabled))
    else {
        return INVALID;
    };
    // Native D3D9 returns 128 for an enabled light.
    let value = if enabled { 128 } else { 0 };
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DDevice9::SetMaterial",level=implement)]
pub(super) fn set_material(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    material: GuestPtr<[f32; 17]>,
) -> u32 {
    let Ok(value) = material.read(env.cpu) else {
        return INVALID;
    };
    super::update_fixed(
        env,
        this.addr(),
        Ok(Setting::Material(state_map::material(value))),
    )
}
#[win32(d3d9,name="IDirect3DDevice9::GetMaterial",level=implement)]
pub(super) fn get_material(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<[f32; 17]>,
) -> u32 {
    let Some(value) = current(env, this.addr()).map(|f| state_map::material_words(f.material))
    else {
        return INVALID;
    };
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
