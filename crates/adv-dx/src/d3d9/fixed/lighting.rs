use super::super::shaders::stateblocks::{FixedKey, mark_fixed};
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
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let created = !f.lights.contains_key(&index);
    let light = f.lights.entry(index).or_default();
    let changed = created || light.value != value;
    light.value = value;
    mark_fixed(env, this.addr(), FixedKey::Light(index), changed);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetLight",level=implement)]
pub(super) fn get_light(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: GuestPtr<[u32; 26]>,
) -> u32 {
    let Some(light) = current(env, this.addr()).and_then(|f| f.lights.get(&index)) else {
        return INVALID;
    };
    let value = light.value;
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DDevice9::LightEnable",level=implement)]
pub(super) fn light_enable(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, enable: i32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let created = !f.lights.contains_key(&index);
    let light = f.lights.entry(index).or_default();
    let changed = created || light.enabled != (enable != 0);
    light.enabled = enable != 0;
    mark_fixed(env, this.addr(), FixedKey::LightEnable(index), changed);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetLightEnable",level=implement)]
pub(super) fn get_light_enable(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    output: GuestPtr<i32>,
) -> u32 {
    let Some(light) = current(env, this.addr()).and_then(|f| f.lights.get(&index)) else {
        return INVALID;
    };
    // Native D3D9 returns 128 for an enabled light.
    let value = if light.enabled { 128 } else { 0 };
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
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let changed = f.material.map(f32::to_bits) != value.map(f32::to_bits);
    f.material = value;
    mark_fixed(env, this.addr(), FixedKey::Material, changed);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetMaterial",level=implement)]
pub(super) fn get_material(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<[f32; 17]>,
) -> u32 {
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    let value = f.material;
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
pub(super) fn active(f: &Fixed) -> Result<[Option<[f32; 26]>; 8], Rejected> {
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
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    if length == 0. {
        v
    } else {
        v.map(|x| x / length)
    }
}
pub(super) fn normal_matrix(f: &Fixed) -> [[f32; 3]; 3] {
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
pub(super) fn illuminate(
    f: &Fixed,
    lights: &[Option<[f32; 26]>; 8],
    eye: [f32; 4],
    normal: [f32; 3],
    matrix: &[[f32; 3]; 3],
    color1: Option<[f32; 4]>,
    color2: Option<[f32; 4]>,
) -> ([f32; 4], [f32; 4]) {
    let source = |state, offset| {
        if f.render[&141] != 0 {
            match f.render[&state] {
                1 if color1.is_some() => return color1.unwrap(),
                2 if color2.is_some() => return color2.unwrap(),
                _ => {}
            }
        }
        f.material[offset..offset + 4].try_into().unwrap()
    };
    let diffuse = source(145, 0);
    let specular = source(146, 8);
    let ambient = source(147, 4);
    let emissive = source(148, 12);
    let mut normal = std::array::from_fn(|c| (0..3).map(|r| normal[r] * matrix[r][c]).sum());
    if f.render[&143] != 0 {
        normal = normalize(normal);
    }
    let global = color(f.render[&139]);
    let mut result = std::array::from_fn(|n| emissive[n] + ambient[n] * global[n]);
    result[3] = diffuse[3];
    let mut highlight = [0.; 4];
    let viewer = if f.render[&142] != 0 {
        normalize([-eye[0], -eye[1], -eye[2]])
    } else {
        [0., 0., -1.]
    };
    for light in lights.iter().flatten() {
        let direction;
        let mut attenuation = 1.;
        if light[0] == 3. {
            let v = row([-light[16], -light[17], -light[18], 0.], &f.transforms[1]);
            direction = normalize([v[0], v[1], v[2]]);
        } else {
            let position = row([light[13], light[14], light[15], 1.], &f.transforms[1]);
            let delta = std::array::from_fn(|n| position[n] - eye[n]);
            let distance = dot(delta, delta).sqrt();
            if distance > light[19] {
                continue;
            }
            direction = normalize(delta);
            attenuation = 1. / (light[21] + light[22] * distance + light[23] * distance * distance);
            if light[0] == 2. {
                let v = row([light[16], light[17], light[18], 0.], &f.transforms[1]);
                let cosine = -dot(normalize([v[0], v[1], v[2]]), direction);
                let inner = (light[24] * 0.5).cos();
                let outer = (light[25] * 0.5).cos();
                attenuation *= if cosine > inner {
                    1.
                } else if cosine <= outer {
                    0.
                } else {
                    ((cosine - outer) / (inner - outer)).powf(light[20])
                };
            }
        }
        let ndotl = dot(normal, direction).max(0.);
        let power = if ndotl > 0. && f.render[&29] != 0 {
            dot(
                normal,
                normalize(std::array::from_fn(|n| viewer[n] + direction[n])),
            )
            .max(0.)
            .powf(f.material[16])
        } else {
            0.
        };
        for n in 0..3 {
            result[n] +=
                attenuation * (ambient[n] * light[9 + n] + diffuse[n] * light[1 + n] * ndotl);
            highlight[n] += attenuation * specular[n] * light[5 + n] * power;
        }
    }
    (
        result.map(|v| v.clamp(0., 1.)),
        highlight.map(|v| v.clamp(0., 1.)),
    )
}
pub(super) fn fog(f: &Fixed, transformed: bool, z: f32, eye: [f32; 4], supplied: f32) -> f32 {
    let table = f.render[&35];
    let mode = if table != 0 {
        table
    } else if transformed {
        0
    } else {
        f.render[&140]
    };
    let distance = if transformed {
        z
    } else if table == 0 && f.render[&48] != 0 {
        dot([eye[0], eye[1], eye[2]], [eye[0], eye[1], eye[2]]).sqrt()
    } else {
        eye[2].abs()
    };
    let density = f32::from_bits(f.render[&38]);
    match mode {
        1 => (-density * distance).exp(),
        2 => (-(density * distance).powi(2)).exp(),
        3 => ((f32::from_bits(f.render[&37]) - distance)
            / (f32::from_bits(f.render[&37]) - f32::from_bits(f.render[&36])))
        .clamp(0., 1.),
        _ => supplied,
    }
}
