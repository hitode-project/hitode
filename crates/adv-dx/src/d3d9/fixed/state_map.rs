//! D3D9 selectors, defaults and refusals for shared fixed-function state.
use super::*;
use crate::fixed::{LightKind, LightValue, Material, Render, Sampler, Stage, Transform, Viewport};
use adv_device::surface::Rect;

pub(in crate::d3d9) const RULES: core_fixed::Rules = core_fixed::Rules {
    color_key_mask: None,
    uncapped_op: None,
    integer_zbias: false,
    round_points: false,
    line_last_pixel: None,
};

pub(in crate::d3d9) fn defaults(width: u32, height: u32) -> Fixed {
    core_fixed::defaults(width, height)
}

pub(super) fn render(state: i32, value: u32) -> Result<Setting, u32> {
    let valid = match state {
        8 => match value {
            3 => true,
            1 | 2 => return Err(UNAVAILABLE),
            _ => false,
        },
        9 => match value {
            1 | 2 => true,
            3 => return Err(UNAVAILABLE),
            _ => false,
        },
        7 | 14 | 15 | 16 | 27 | 29 | 52 | 137 | 141 | 142 | 143 | 48 | 157 | 161 | 174 | 176
        | 194 | 185 | 206 => value <= 1,
        26 => match value {
            0 => true,
            1 => return Err(UNAVAILABLE),
            _ => false,
        },
        28 | 156 => value <= 1,
        34 | 60 | 139 | 162 | 193 => true,
        165 => value <= 1,
        170 => f32::from_bits(value).is_finite(),
        158..=160 | 175 | 195 => f32::from_bits(value).is_finite(),
        172 | 173 => (1..=3).contains(&value),
        190..=192 => value <= 15,
        35 | 140 => value <= 3,
        36..=38 => f32::from_bits(value).is_finite(),
        154 | 155 | 166 => f32::from_bits(value).is_finite() && f32::from_bits(value) > 0.0,
        167 => match value {
            0 => true,
            1 => return Err(UNAVAILABLE),
            _ => false,
        },
        136 => value <= 1,
        145..=148 => value <= 2,
        151 => match value {
            0 => true,
            1..=3 | 255 | 256 => return Err(UNAVAILABLE),
            _ => false,
        },
        152 => value & !63 == 0,
        19 | 20 | 207 | 208 => (1..=15).contains(&value),
        22 => (1..=3).contains(&value),
        23 | 25 | 56 | 189 => (1..=8).contains(&value),
        53..=55 | 186..=188 => (1..=8).contains(&value),
        24 | 57..=59 => true,
        168 => value <= 15,
        128..=135 | 198..=205 => match value {
            0 => true,
            1..=15 => return Err(UNAVAILABLE),
            _ => false,
        },
        171 | 209 => (1..=5).contains(&value),
        _ => return Err(UNAVAILABLE),
    };
    if !valid {
        return Err(INVALID);
    }
    Ok(Setting::Render(Render::from_raw(state).unwrap(), value))
}

pub(super) fn stage(stage: u32, kind: i32, value: u32) -> Result<Setting, u32> {
    if stage >= 8 {
        return Err(INVALID);
    }
    if matches!(kind, 7..=10 | 22 | 23) {
        return Ok(Setting::StageRegister(
            stage as usize,
            Stage::from_raw(kind).unwrap(),
            value,
        ));
    }
    if stage != 0 {
        // Stage one combines like stage zero; later stages stay disabled.
        // Arguments are stored for Get and state blocks and checked at draw.
        let valid = match kind {
            1 | 4 => match value {
                1 => true,
                2..=7 | 10 if stage == 1 => true,
                2..=26 => return Err(UNAVAILABLE),
                _ => false,
            },
            2 | 3 | 5 | 6 => value & !0x3f == 0 && value & 0xf <= 6,
            11 => match (value & 0xffff, value >> 16) {
                (0..=7, 0) => true,
                (0..=7, 1..=4) => return Err(UNAVAILABLE),
                _ => false,
            },
            _ => return Err(UNAVAILABLE),
        };
        if !valid {
            return Err(INVALID);
        }
        return Ok(Setting::StageRegister(
            stage as usize,
            Stage::from_raw(kind).unwrap(),
            value,
        ));
    }
    // Legal SDK operations and arguments this stage does not combine are
    // unavailable rather than invalid, so callers see them reported.
    let valid = match kind {
        1 | 4 => match value {
            1..=7 | 10 => true,
            8..=26 => return Err(UNAVAILABLE),
            _ => false,
        },
        2 | 3 | 5 | 6 => match (value & !0x3f, value & 0xf) {
            (0, 0..=3) => true,
            (0, 4..=6) => return Err(UNAVAILABLE),
            _ => false,
        },
        11 => {
            // Up to eight FVF coordinate sets and the SDK's four automatic
            // generation modes are legal; this decoder supplies set zero only.
            if value & 0xffff > 7 || value >> 16 > 4 {
                false
            } else if value != 0 {
                return Err(UNAVAILABLE);
            } else {
                true
            }
        }
        24 => match value {
            0 => true,
            1..=4 | 256..=260 => return Err(UNAVAILABLE),
            _ => false,
        },
        _ => return Err(UNAVAILABLE),
    };
    if !valid {
        return Err(INVALID);
    }
    Ok(Setting::Stage(Stage::from_raw(kind).unwrap(), value))
}

pub(super) fn sampler(sampler: u32, kind: i32, value: u32) -> Result<Setting, u32> {
    let Some(slot) = super::programmable::sampler_slot(sampler) else {
        return Err(INVALID);
    };
    let valid = match kind {
        1..=3 => (1..=4).contains(&value),
        5 | 6 => matches!(value, 1 | 2),
        7 => value <= 2,
        4 | 9 => true,
        // These neutral values require no additional sampling operation. Keep
        // exact DWORDs (including signed zero); nonneutral requests stay explicit.
        8 if f32::from_bits(value) == 0. => true,
        10 if value == 1 => true,
        11..=13 if value == 0 => true,
        _ => return Err(UNAVAILABLE),
    };
    if !valid {
        return Err(INVALID);
    }
    let kind = Sampler::from_raw(kind).unwrap();
    Ok(if slot == 0 {
        Setting::Sampler(kind, value)
    } else {
        Setting::SamplerRegister(slot, kind, value)
    })
}

pub(super) fn viewport(v: D3DVIEWPORT9, target: (u32, u32)) -> Result<Setting, u32> {
    if v.Width == 0
        || v.Height == 0
        || v.X.checked_add(v.Width).is_none_or(|x| x > target.0)
        || v.Y.checked_add(v.Height).is_none_or(|y| y > target.1)
        || !v.MinZ.is_finite()
        || !v.MaxZ.is_finite()
        || v.MinZ < 0.
        || v.MaxZ > 1.
        || v.MinZ > v.MaxZ
    {
        return Err(INVALID);
    }
    Ok(Setting::Viewport(from_viewport(v)))
}

pub(super) fn scissor(rect: RECT) -> Result<Setting, u32> {
    if rect.left < 0 || rect.top < 0 || rect.right <= rect.left || rect.bottom <= rect.top {
        return Err(INVALID);
    }
    Ok(Setting::Scissor(from_scissor(rect)))
}

fn from_viewport(v: D3DVIEWPORT9) -> Viewport {
    Viewport {
        rect: Rect {
            x: v.X,
            y: v.Y,
            w: v.Width,
            h: v.Height,
        },
        min_depth: v.MinZ,
        max_depth: v.MaxZ,
    }
}
pub(in crate::d3d9) fn to_viewport(v: Viewport) -> D3DVIEWPORT9 {
    D3DVIEWPORT9 {
        X: v.rect.x,
        Y: v.rect.y,
        Width: v.rect.w,
        Height: v.rect.h,
        MinZ: v.min_depth,
        MaxZ: v.max_depth,
    }
}
fn from_scissor(r: RECT) -> Rect {
    Rect {
        x: r.left as u32,
        y: r.top as u32,
        w: (r.right - r.left) as u32,
        h: (r.bottom - r.top) as u32,
    }
}
pub(in crate::d3d9) fn to_scissor(r: Rect) -> RECT {
    RECT {
        left: r.x as i32,
        top: r.y as i32,
        right: (r.x + r.w) as i32,
        bottom: (r.y + r.h) as i32,
    }
}

pub(in crate::d3d9) fn apply_render(
    f: &mut CachedFixed,
    state: i32,
    value: u32,
) -> Result<bool, u32> {
    Ok(f.state_mut().apply(render(state, value)?).changed)
}

pub(in crate::d3d9) fn apply_stage(
    f: &mut CachedFixed,
    stage: u32,
    kind: i32,
    value: u32,
) -> Result<bool, u32> {
    Ok(f.state_mut()
        .apply(self::stage(stage, kind, value)?)
        .changed)
}

pub(in crate::d3d9) fn apply_sampler(
    f: &mut CachedFixed,
    sampler: u32,
    kind: i32,
    value: u32,
) -> Result<bool, u32> {
    Ok(f.state_mut()
        .apply(self::sampler(sampler, kind, value)?)
        .changed)
}

pub(in crate::d3d9) fn apply_viewport(
    f: &mut CachedFixed,
    v: D3DVIEWPORT9,
    target: (u32, u32),
) -> Result<bool, u32> {
    Ok(f.state_mut().apply(viewport(v, target)?).changed)
}

pub(in crate::d3d9) fn apply_scissor(f: &mut CachedFixed, rect: RECT) -> Result<bool, u32> {
    Ok(f.state_mut().apply(scissor(rect)?).changed)
}

pub(in crate::d3d9) fn stage_value(f: &Fixed, stage: u32, kind: i32) -> Option<u32> {
    if stage >= 8 {
        return None;
    }
    let states = if stage != 0 || matches!(kind, 7..=10 | 22 | 23) {
        &f.stage_slots[stage as usize]
    } else {
        &f.stage
    };
    states.get(&kind).copied()
}

pub(in crate::d3d9) fn sampler_value(f: &Fixed, sampler: u32, kind: i32) -> Option<u32> {
    let slot = super::programmable::sampler_slot(sampler)?;
    if slot == 0 {
        f.sampler.get(&kind)
    } else {
        f.sampler_slots[slot].get(&kind)
    }
    .copied()
}

pub(in crate::d3d9) fn transform_slot(kind: i32) -> Option<Transform> {
    Some(match kind {
        256 => Transform::World,
        2 => Transform::View,
        3 => Transform::Projection,
        _ => return None,
    })
}

#[cfg(test)]
pub(super) use core_fixed::defaults::default_light;
pub(super) use core_fixed::material;

pub(super) fn material_words(material: Material) -> [f32; 17] {
    let mut words = [material.power; 17];
    words[..16].copy_from_slice(material.colors.as_flattened());
    words
}

pub(super) fn light(words: [u32; 26]) -> Result<LightValue, u32> {
    let kind = match words[0] {
        1 => LightKind::Point,
        2 => LightKind::Spot,
        3 => LightKind::Directional,
        _ => return Err(UNAVAILABLE),
    };
    let floats = words.map(f32::from_bits);
    if floats[19] < 0.
        || floats[21..24].iter().any(|v| *v < 0.)
        || (kind == LightKind::Spot
            && (floats[24] < 0. || floats[24] > floats[25] || floats[25] > std::f32::consts::PI))
    {
        return Err(INVALID);
    }
    Ok(LightValue {
        kind,
        components: words[1..].try_into().unwrap(),
    })
}

impl Layout {
    pub(in crate::d3d9) fn from_fvf(fvf: u32) -> Option<Self> {
        if !valid_fvf(fvf) {
            return None;
        }
        let transformed = fvf & 0x0e == 4;
        let mut size = if transformed { 16 } else { 12 };
        let mut field = |present: bool, bytes: u32| {
            present.then(|| {
                size += bytes;
                size - bytes
            })
        };
        let normal = field(fvf & 0x10 != 0, 12).map(|at| (0, at));
        let color = field(fvf & 0x40 != 0, 4);
        let specular = field(fvf & 0x80 != 0, 4).map(|at| (0, at));
        let mut texcoords = [None; 8];
        for (n, coordinate) in texcoords
            .iter_mut()
            .enumerate()
            .take(((fvf >> 8) & 15) as usize)
        {
            let dimensions = [2, 3, 4, 1][((fvf >> (16 + n * 2)) & 3) as usize];
            let offset = size;
            size += u32::from(dimensions) * 4;
            *coordinate = Some((0, offset, dimensions));
        }
        Some(Self {
            transformed,
            position: 0,
            color,
            normal,
            specular,
            texcoords,
            streams: [0; 2],
            size,
        })
    }
}
fn valid_fvf(fvf: u32) -> bool {
    let count = (fvf >> 8) & 15;
    let sizes = fvf >> 16;
    matches!(fvf & 0x0e, 2 | 4)
        && fvf & !(0x0e | 0x10 | 0x40 | 0x80 | 0xf00 | 0xffff0000) == 0
        && !(fvf & 0x0e == 4 && fvf & 0x10 != 0)
        && count <= 8
        && (count == 8 || sizes >> (count * 2) == 0)
}
