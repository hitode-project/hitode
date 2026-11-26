//! D3D9 selectors, defaults and refusals for shared fixed-function state.
use super::*;
use crate::fixed::StateMap;
use crate::fixed::{
    LightKind, LightValue, Material, Registers, Render, Sampler, Stage, State, Transform, Viewport,
};
use adv_device::surface::Rect;

pub(in crate::d3d9) const RULES: core_fixed::Rules = core_fixed::Rules {
    color_key_mask: None,
    uncapped_op: None,
    integer_zbias: false,
    round_points: false,
    line_last_pixel: None,
};

pub(in crate::d3d9) fn defaults(width: u32, height: u32) -> Fixed {
    let sampler = BTreeMap::from([
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 0),
        (5, 1),
        (6, 1),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 1),
        (11, 0),
        (12, 0),
        (13, 0),
    ]);
    State::new(
        Registers {
            render: StateMap::from(BTreeMap::from([
                (7, 0),
                (8, 3), // FILLMODE: only solid rasterization is implemented.
                (9, 2), // SHADEMODE: GOURAUD.
                (14, 1),
                (15, 0),
                (16, 1), // LASTPIXEL: consumed by the shared line rasterizer.
                (19, 2),
                (20, 1),
                (22, 3),
                (23, 4),
                (24, 0),
                (25, 8),
                (27, 0),
                (28, 0),
                (34, 0),
                (35, 0),
                (36, 0f32.to_bits()),
                (37, 1f32.to_bits()),
                (38, 1f32.to_bits()),
                (140, 0),
                (48, 0),        // RANGEFOGENABLE.
                (60, u32::MAX), // TEXTUREFACTOR.
                (142, 1),       // LOCALVIEWER.
                (143, 0),       // NORMALIZENORMALS.
                (157, 0),
                (158, 1f32.to_bits()),
                (159, 0),
                (160, 0),
                (162, u32::MAX),
                (165, 0),
                (170, 0),
                (172, 3),
                (173, 1),
                (175, 0),
                (176, 0),
                (193, u32::MAX),
                (194, 0),
                (195, 0),
                (154, 1f32.to_bits()),
                (155, 1f32.to_bits()),
                (156, 0),
                (166, 64f32.to_bits()),
                (29, 0), // SPECULARENABLE.
                (26, 0), // DITHERENABLE: the renderer does not dither.
                (52, 0),
                (53, 1),
                (54, 1),
                (55, 1),
                (56, 8),
                (57, 0),
                (58, u32::MAX),
                (59, u32::MAX),
                (128, 0), // WRAP0–WRAP7: texture coordinate wrapping is disabled.
                (129, 0),
                (130, 0),
                (131, 0),
                (132, 0),
                (133, 0),
                (134, 0),
                (135, 0),
                (136, 1), // CLIPPING: GPU primitive clipping remains enabled.
                (137, 1),
                (139, 0), // AMBIENT.
                (141, 1), // COLORVERTEX: only relevant when lighting is enabled.
                (145, 1), // Material sources: COLOR1, COLOR2, MATERIAL, MATERIAL.
                (146, 2),
                (147, 0),
                (148, 0),
                (151, 0), // VERTEXBLEND: one world matrix without blending.
                (152, 0), // CLIPPLANEENABLE: no user clipping planes.
                (161, 1), // Inert on the supported single-sample render targets.
                (167, 0), // INDEXEDVERTEXBLENDENABLE: no matrix indices.
                (168, 15),
                (190, 15),
                (191, 15),
                (192, 15),
                (198, 0),
                (199, 0),
                (200, 0),
                (201, 0),
                (202, 0),
                (203, 0),
                (204, 0),
                (205, 0),
                (171, 1),
                (174, 0),
                (185, 0),
                (186, 1),
                (187, 1),
                (188, 1),
                (189, 8),
                (206, 0),
                (207, 2),
                (208, 1),
                (209, 1),
            ])),
            stage_slots: std::array::from_fn(|stage| {
                let mut states =
                    BTreeMap::from([(7, 0), (8, 0), (9, 0), (10, 0), (22, 0), (23, 0)]);
                if stage != 0 {
                    states.extend([(1, 1), (2, 2), (3, 1), (4, 1), (5, 2), (6, 1)]);
                    // TEXCOORDINDEX defaults to the stage's own coordinate set.
                    states.insert(11, stage as u32);
                }
                states.into()
            }),
            stage: StateMap::from(BTreeMap::from([
                (1, 4),
                (2, 2),
                (3, 1),
                (4, 2),
                (5, 2),
                (6, 1),
                (11, 0), // TEXCOORDINDEX: pass through the first FVF coordinate set.
                (24, 0), // TEXTURETRANSFORMFLAGS: no texture coordinate transform.
            ])),
            sampler: sampler.clone().into(),
            sampler_slots: std::array::from_fn(|_| sampler.clone().into()),
        },
        width,
        height,
        material([
            1., 1., 1., 1., 1., 1., 1., 1., 0., 0., 0., 0., 0., 0., 0., 0., 0.,
        ]),
        default_light(),
    )
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
    Ok(Setting::Render(render_slot(state), value))
}

pub(super) fn stage(stage: u32, kind: i32, value: u32) -> Result<Setting, u32> {
    if stage >= 8 {
        return Err(INVALID);
    }
    if matches!(kind, 7..=10 | 22 | 23) {
        return Ok(Setting::StageRegister(
            stage as usize,
            stage_slot(kind),
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
            stage_slot(kind),
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
    Ok(Setting::Stage(stage_slot(kind), value))
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
    let kind = sampler_kind(kind);
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

pub(super) fn material(words: [f32; 17]) -> Material {
    Material {
        colors: std::array::from_fn(|i| words[i * 4..][..4].try_into().unwrap()),
        power: words[16],
    }
}

pub(super) fn material_words(material: Material) -> [f32; 17] {
    let mut words = [material.power; 17];
    words[..16].copy_from_slice(material.colors.as_flattened());
    words
}

pub(super) fn default_light() -> LightValue {
    let mut components = [0; 25];
    components[..3].fill(1f32.to_bits());
    components[17] = 1f32.to_bits();
    LightValue {
        kind: LightKind::Directional,
        components,
    }
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

fn render_slot(kind: i32) -> Render {
    match kind {
        7 => Render::DepthEnable,
        8 => Render::FillMode,
        9 => Render::ShadeMode,
        14 => Render::DepthWrite,
        15 => Render::AlphaTest,
        16 => Render::LastPixel,
        19 => Render::SourceBlend,
        20 => Render::DestinationBlend,
        22 => Render::CullMode,
        23 => Render::DepthCompare,
        24 => Render::AlphaReference,
        25 => Render::AlphaCompare,
        26 => Render::DitherEnable,
        27 => Render::BlendEnable,
        28 => Render::FogEnable,
        29 => Render::SpecularEnable,
        34 => Render::FogColor,
        35 => Render::TableFog,
        36 => Render::FogStart,
        37 => Render::FogEnd,
        38 => Render::FogDensity,
        48 => Render::RangeFog,
        52 => Render::StencilEnable,
        53 => Render::StencilFail,
        54 => Render::StencilDepthFail,
        55 => Render::StencilPass,
        56 => Render::StencilCompare,
        57 => Render::StencilReference,
        58 => Render::StencilReadMask,
        59 => Render::StencilWriteMask,
        60 => Render::TextureFactor,
        128 => Render::Wrap0,
        129 => Render::Wrap1,
        130 => Render::Wrap2,
        131 => Render::Wrap3,
        132 => Render::Wrap4,
        133 => Render::Wrap5,
        134 => Render::Wrap6,
        135 => Render::Wrap7,
        136 => Render::Clipping,
        137 => Render::Lighting,
        139 => Render::Ambient,
        140 => Render::VertexFog,
        141 => Render::ColorVertex,
        142 => Render::LocalViewer,
        143 => Render::NormalizeNormals,
        145 => Render::DiffuseSource,
        146 => Render::SpecularSource,
        147 => Render::AmbientSource,
        148 => Render::EmissiveSource,
        151 => Render::VertexBlend,
        152 => Render::ClipPlaneEnable,
        154 => Render::PointSize,
        155 => Render::PointSizeMin,
        156 => Render::PointSprite,
        157 => Render::PointScale,
        158 => Render::PointScaleA,
        159 => Render::PointScaleB,
        160 => Render::PointScaleC,
        161 => Render::MultisampleAntialias,
        162 => Render::MultisampleMask,
        165 => Render::DebugMonitorToken,
        166 => Render::PointSizeMax,
        167 => Render::IndexedVertexBlend,
        168 => Render::ColorWrite,
        170 => Render::TweenFactor,
        171 => Render::BlendOperation,
        172 => Render::PositionDegree,
        173 => Render::NormalDegree,
        174 => Render::ScissorEnable,
        175 => Render::SlopeDepthBias,
        176 => Render::AntialiasedLine,
        185 => Render::TwoSidedStencil,
        186 => Render::BackStencilFail,
        187 => Render::BackStencilDepthFail,
        188 => Render::BackStencilPass,
        189 => Render::BackStencilCompare,
        190 => Render::ColorWrite1,
        191 => Render::ColorWrite2,
        192 => Render::ColorWrite3,
        193 => Render::BlendFactor,
        194 => Render::SrgbWrite,
        195 => Render::DepthBias,
        198 => Render::Wrap8,
        199 => Render::Wrap9,
        200 => Render::Wrap10,
        201 => Render::Wrap11,
        202 => Render::Wrap12,
        203 => Render::Wrap13,
        204 => Render::Wrap14,
        205 => Render::Wrap15,
        206 => Render::SeparateAlphaBlend,
        207 => Render::SourceBlendAlpha,
        208 => Render::DestinationBlendAlpha,
        209 => Render::BlendOperationAlpha,
        _ => unreachable!(),
    }
}

fn stage_slot(kind: i32) -> Stage {
    match kind {
        1 => Stage::ColorOp,
        2 => Stage::ColorArg1,
        3 => Stage::ColorArg2,
        4 => Stage::AlphaOp,
        5 => Stage::AlphaArg1,
        6 => Stage::AlphaArg2,
        7 => Stage::Bump00,
        8 => Stage::Bump01,
        9 => Stage::Bump10,
        10 => Stage::Bump11,
        11 => Stage::CoordinateIndex,
        22 => Stage::BumpScale,
        23 => Stage::BumpOffset,
        24 => Stage::TransformFlags,
        _ => unreachable!(),
    }
}

fn sampler_kind(kind: i32) -> Sampler {
    match kind {
        1 => Sampler::AddressU,
        2 => Sampler::AddressV,
        3 => Sampler::AddressW,
        4 => Sampler::BorderColor,
        5 => Sampler::MagFilter,
        6 => Sampler::MinFilter,
        7 => Sampler::MipFilter,
        8 => Sampler::MipLodBias,
        9 => Sampler::MaxMipLevel,
        10 => Sampler::MaxAnisotropy,
        11 => Sampler::SrgbTexture,
        12 => Sampler::ElementIndex,
        13 => Sampler::DisplacementOffset,
        _ => unreachable!(),
    }
}
