//! Direct3D 7 state selectors and their neutral fixed-function values.
use crate::fixed::{
    self, LightKind, LightValue, Render, Sampler, Setting, Stage, State, Transform, Viewport,
};
use adv_abi::types::D3DVIEWPORT7;
use adv_device::surface::Rect;

use super::super::INVALID;

const UNSUPPORTED: u32 = 0x80004001;

pub(in crate::ddraw::d3d) fn defaults(width: u32, height: u32, depth: bool) -> State {
    let mut state = fixed::defaults(width, height);
    for (slot, value) in [
        (Render::DepthEnable, u32::from(depth)),
        (Render::TexturePerspective, 1),
        (Render::Antialias, 0),
        (Render::LinePattern, 0),
        (Render::StippledAlpha, 0),
        (Render::EdgeAntialias, 0),
        (Render::StippleEnable, 0),
        (Render::ColorKeyEnable, 0),
        (Render::ZBias, 0),
    ] {
        state.apply(Setting::Render(slot, value));
    }
    for stage in &mut state.stage_slots {
        stage.insert(Stage::TransformFlags as i32, 0);
    }
    state
}

pub(in crate::ddraw::d3d) fn render(slot: u32, value: u32) -> Result<Setting, u32> {
    let valid = match slot {
        2 => value <= 2,
        7 => value <= 2,
        8 | 9 | 22 => (1..=3).contains(&value),
        4 | 14..=16 | 26..=29 | 33 | 39..=41 | 48 | 52 | 136 | 137 | 141..=143 => true,
        10 | 24 | 34 | 36..=38 | 47 | 57..=60 => true,
        19 | 20 => (1..=13).contains(&value),
        23 | 25 | 53..=56 => (1..=8).contains(&value),
        35 | 140 => value <= 3,
        128..=135 => value <= 7,
        139 => true,
        145..=148 => value <= 2,
        151 => value <= 3,
        152 => value <= 63,
        _ => return Err(UNSUPPORTED),
    };
    if !valid {
        return Err(INVALID);
    }
    Ok(Setting::Render(
        Render::from_raw(slot as i32).unwrap(),
        value,
    ))
}

pub(in crate::ddraw::d3d) fn render_value(state: &State, slot: u32) -> Result<u32, u32> {
    render(slot, state.render.get(&(slot as i32)).copied().unwrap_or(0))?;
    Ok(state.render[&(slot as i32)])
}

fn texture_sampler(slot: u32) -> Option<Sampler> {
    Some(match slot {
        12 | 13 => Sampler::AddressU,
        14 => Sampler::AddressV,
        15 => Sampler::BorderColor,
        16 => Sampler::MagFilter,
        17 => Sampler::MinFilter,
        18 => Sampler::MipFilter,
        19 => Sampler::MipLodBias,
        20 => Sampler::MaxMipLevel,
        21 => Sampler::MaxAnisotropy,
        _ => return None,
    })
}

pub(in crate::ddraw::d3d) fn stage(
    stage: usize,
    slot: u32,
    value: u32,
) -> Result<[Option<Setting>; 2], u32> {
    if stage >= 8 {
        return Err(INVALID);
    }
    if let Some(sampler) = texture_sampler(slot) {
        let value = match sampler {
            Sampler::AddressU | Sampler::AddressV if !(1..=4).contains(&value) => {
                return Err(INVALID);
            }
            Sampler::MagFilter if !matches!(value, 1 | 2 | 5) => return Err(UNSUPPORTED),
            Sampler::MinFilter if !(1..=3).contains(&value) => return Err(INVALID),
            Sampler::MipFilter => value.checked_sub(1).filter(|v| *v <= 2).ok_or(INVALID)?,
            Sampler::MagFilter if value == 5 => 3,
            Sampler::AddressU
            | Sampler::AddressV
            | Sampler::AddressW
            | Sampler::BorderColor
            | Sampler::MagFilter
            | Sampler::MinFilter
            | Sampler::MipLodBias
            | Sampler::MaxMipLevel
            | Sampler::MaxAnisotropy
            | Sampler::SrgbTexture
            | Sampler::ElementIndex
            | Sampler::DisplacementOffset => value,
        };
        let set = |sampler| {
            if stage == 0 {
                Setting::Sampler(sampler, value)
            } else {
                Setting::SamplerRegister(stage, sampler, value)
            }
        };
        return Ok([
            Some(set(sampler)),
            (slot == 12).then(|| set(Sampler::AddressV)),
        ]);
    }
    let valid = match slot {
        1 | 4 => (1..=24).contains(&value),
        2 | 3 | 5 | 6 => value & !0x3f == 0 && value & 0xf <= 4,
        7..=10 | 22 | 23 => true,
        11 => value >> 16 <= 3 && value & 0xffff < 8,
        24 => matches!(value, 0..=4 | 257..=260),
        _ => return Err(INVALID),
    };
    if !valid {
        return Err(INVALID);
    }
    let slot = Stage::from_raw(slot as i32).unwrap();
    let setting = if stage == 0 && !bump(slot) {
        Setting::Stage(slot, value)
    } else {
        Setting::StageRegister(stage, slot, value)
    };
    Ok([Some(setting), None])
}

fn bump(slot: Stage) -> bool {
    matches!(
        slot,
        Stage::Bump00
            | Stage::Bump01
            | Stage::Bump10
            | Stage::Bump11
            | Stage::BumpScale
            | Stage::BumpOffset
    )
}

pub(in crate::ddraw::d3d) fn stage_value(
    state: &State,
    stage: usize,
    slot: u32,
) -> Result<u32, u32> {
    if stage >= 8 {
        return Err(INVALID);
    }
    if let Some(sampler) = texture_sampler(slot) {
        let registers = if stage == 0 {
            &state.sampler
        } else {
            &state.sampler_slots[stage]
        };
        let value = registers[&sampler];
        return Ok(match sampler {
            Sampler::MipFilter => value + 1,
            Sampler::MagFilter if value == 3 => 5,
            Sampler::AddressU
            | Sampler::AddressV
            | Sampler::AddressW
            | Sampler::BorderColor
            | Sampler::MagFilter
            | Sampler::MinFilter
            | Sampler::MipLodBias
            | Sampler::MaxMipLevel
            | Sampler::MaxAnisotropy
            | Sampler::SrgbTexture
            | Sampler::ElementIndex
            | Sampler::DisplacementOffset => value,
        });
    }
    let slot = Stage::from_raw(slot as i32).ok_or(INVALID)?;
    let registers = if stage == 0 && !bump(slot) {
        &state.stage
    } else {
        &state.stage_slots[stage]
    };
    Ok(registers[&slot])
}

pub(in crate::ddraw::d3d) fn transform(slot: u32) -> Result<Transform, u32> {
    Ok(match slot {
        1 => Transform::World,
        2 => Transform::View,
        3 => Transform::Projection,
        4 => Transform::World1,
        5 => Transform::World2,
        6 => Transform::World3,
        16 => Transform::Texture0,
        17 => Transform::Texture1,
        18 => Transform::Texture2,
        19 => Transform::Texture3,
        20 => Transform::Texture4,
        21 => Transform::Texture5,
        22 => Transform::Texture6,
        23 => Transform::Texture7,
        _ => return Err(INVALID),
    })
}

pub(in crate::ddraw::d3d) fn viewport(
    v: D3DVIEWPORT7,
    target: (u32, u32),
) -> Result<Viewport, u32> {
    let D3DVIEWPORT7 {
        dwX: x,
        dwY: y,
        dwWidth: w,
        dwHeight: h,
        dvMinZ: min_depth,
        dvMaxZ: max_depth,
    } = v;
    if w == 0
        || h == 0
        || x.checked_add(w).is_none_or(|right| right > target.0)
        || y.checked_add(h).is_none_or(|bottom| bottom > target.1)
        || !(0. ..=1.).contains(&min_depth)
        || !(min_depth..=1.).contains(&max_depth)
    {
        return Err(INVALID);
    }
    Ok(Viewport {
        rect: Rect { x, y, w, h },
        min_depth,
        max_depth,
    })
}

pub(in crate::ddraw::d3d) fn viewport_desc(v: Viewport) -> D3DVIEWPORT7 {
    D3DVIEWPORT7 {
        dwX: v.rect.x,
        dwY: v.rect.y,
        dwWidth: v.rect.w,
        dwHeight: v.rect.h,
        dvMinZ: v.min_depth,
        dvMaxZ: v.max_depth,
    }
}

pub(in crate::ddraw::d3d) fn light(v: [u32; 26]) -> Result<LightValue, u32> {
    let kind = match v[0] {
        1 => LightKind::Point,
        2 => LightKind::Spot,
        3 => LightKind::Directional,
        _ => return Err(INVALID),
    };
    Ok(LightValue {
        kind,
        components: v[1..].try_into().unwrap(),
    })
}
