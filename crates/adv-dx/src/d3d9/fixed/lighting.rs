use super::super::shaders::stateblocks::FixedKey;
use super::*;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(in crate::d3d9) struct Light {
    pub value: [u32; 26],
    pub enabled: bool,
}
impl Default for Light {
    fn default() -> Self {
        let mut value = [0; 26];
        value[0] = 3;
        value[1..4].fill(1f32.to_bits());
        value[18] = 1f32.to_bits();
        Self {
            value,
            enabled: false,
        }
    }
}
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
    if !(1..=3).contains(&value[0]) {
        adv_abi::warn_unavailable(env, "IDirect3DDevice9::SetLight", "unsupported light type");
        return UNAVAILABLE;
    }
    let floats = value.map(f32::from_bits);
    if floats[19] < 0.
        || floats[21..24].iter().any(|v| *v < 0.)
        || (value[0] == 2
            && (floats[24] < 0. || floats[24] > floats[25] || floats[25] > std::f32::consts::PI))
    {
        return INVALID;
    }
    if !super::mutate_fixed_raw(env, this.addr(), FixedKey::Light(index), |f| {
        let created = !f.lights.contains_key(&index);
        let light = f.lights.entry(index).or_default();
        let changed = created || light.value != value;
        light.value = value;
        changed
    }) {
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetLight",level=implement)]
pub(super) fn get_light(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: GuestPtr<[u32; 26]>,
) -> u32 {
    let Some(value) = current(env, this.addr()).and_then(|f| f.lights.get(&index).map(|l| l.value))
    else {
        return INVALID;
    };
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DDevice9::LightEnable",level=implement)]
pub(super) fn light_enable(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, enable: i32) -> u32 {
    if !super::mutate_fixed_raw(env, this.addr(), FixedKey::LightEnable(index), |f| {
        let created = !f.lights.contains_key(&index);
        let light = f.lights.entry(index).or_default();
        let changed = created || light.enabled != (enable != 0);
        light.enabled = enable != 0;
        changed
    }) {
        return INVALID;
    }
    0
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
    if !super::mutate_fixed_raw(env, this.addr(), FixedKey::Material, |f| {
        let changed = f.material.map(f32::to_bits) != value.map(f32::to_bits);
        f.material = value;
        changed
    }) {
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetMaterial",level=implement)]
pub(super) fn get_material(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<[f32; 17]>,
) -> u32 {
    let Some(value) = current(env, this.addr()).map(|f| f.material) else {
        return INVALID;
    };
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
pub(in crate::d3d9) fn active(f: &Fixed) -> Result<[Option<[f32; 26]>; 8], Rejected> {
    let mut lights = [None; 8];
    if f.render[&137] != 0 {
        for (n, light) in f.lights.values().filter(|l| l.enabled).enumerate() {
            let Some(slot) = lights.get_mut(n) else {
                return Err(rejected(
                    UNAVAILABLE,
                    "more than MaxActiveLights=8 lights are enabled",
                ));
            };
            let mut value = light.value.map(f32::from_bits);
            value[0] = light.value[0] as f32;
            *slot = Some(value);
        }
    }
    Ok(lights)
}
pub(in crate::d3d9) fn color(argb: u32) -> [f32; 4] {
    [
        ((argb >> 16) & 255) as f32 / 255.,
        ((argb >> 8) & 255) as f32 / 255.,
        (argb & 255) as f32 / 255.,
        (argb >> 24) as f32 / 255.,
    ]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|n| a[n] * b[n]).sum()
}
pub(in crate::d3d9) fn normal_matrix(f: &Fixed) -> [[f32; 3]; 3] {
    let m: [[f32; 3]; 3] = std::array::from_fn(|r| {
        let row = row(
            f.transforms[0][r * 4..r * 4 + 4].try_into().unwrap(),
            &f.transforms[1],
        );
        [row[0], row[1], row[2]]
    });
    let cofactor = std::array::from_fn::<_, 3, _>(|r| {
        std::array::from_fn::<_, 3, _>(|c| {
            let (a, b) = ((r + 1) % 3, (r + 2) % 3);
            let (x, y) = ((c + 1) % 3, (c + 2) % 3);
            m[a][x] * m[b][y] - m[a][y] * m[b][x]
        })
    });
    let det = dot(m[0], cofactor[0]);
    cofactor.map(|r| r.map(|v| v / det))
}

#[cfg(test)]
#[path = "reference.rs"]
mod reference;
#[cfg(test)]
pub(super) use reference::{fog, illuminate};
