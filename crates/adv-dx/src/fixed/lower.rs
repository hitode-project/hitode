//! Lower fixed-function state into raster settings and shader inputs.
use super::{Render, Sampler, Stage, State, StateMap, VertexProgram};
use adv_device::gpu::{
    Address, Blend, BlendOp, Compare, Cull, DrawState, Factor, Filter, Stencil, StencilFace,
    StencilOp, TextureOp, Topology, Viewport,
};
use adv_device::surface::Rect;

#[derive(Clone, Copy)]
pub(crate) struct Rules {
    pub color_key_mask: Option<u32>,
    pub uncapped_op: Option<(u32, u32)>,
    pub integer_zbias: bool,
    pub round_points: bool,
    /// None uses the state's line endpoint setting.
    pub line_last_pixel: Option<bool>,
}

impl Rules {
    pub(crate) fn texture_op(self, op: u32) -> u32 {
        match self.uncapped_op {
            Some((from, to)) if op == from => to,
            _ => op,
        }
    }
}

pub(crate) struct Layout {
    pub topology: Topology,
    /// Validated vertex variant, selected before the front captures streams.
    pub vertex: Option<VertexProgram>,
    pub extent: (u32, u32),
    pub depth: bool,
    pub stencil: bool,
    /// Bound fixed texture's mip count and residency LOD.
    pub texture: Option<(u32, u32)>,
    pub stage_zero_bound: bool,
}

pub(crate) struct Lowered {
    pub state: DrawState,
    pub program: ProgramKey,
    pub stage_zero: [u32; 4],
    /// Bias, slope, color-key mask, and raster-rule flags.
    pub raster_parameters: [u32; 4],
}
pub(crate) struct ProgramKey {
    pub features: u32,
    pub vertex: Option<VertexProgram>,
}

#[derive(Debug)]
pub(crate) enum LowerError {
    InvalidValue,
    EmptyScissor,
    TextureOperation,
    TextureArgument,
    TooManyLights,
}

fn factor(value: u32) -> Option<Factor> {
    Some(match value {
        1 => Factor::Zero,
        2 => Factor::One,
        3 => Factor::SourceColor,
        4 => Factor::OneMinusSourceColor,
        5 => Factor::SourceAlpha,
        6 => Factor::OneMinusSourceAlpha,
        7 => Factor::DestinationAlpha,
        8 => Factor::OneMinusDestinationAlpha,
        9 => Factor::DestinationColor,
        10 => Factor::OneMinusDestinationColor,
        11 => Factor::SourceAlphaSaturated,
        12 => Factor::SourceAlpha,
        13 => Factor::OneMinusSourceAlpha,
        14 => Factor::Constant,
        15 => Factor::OneMinusConstant,
        _ => return None,
    })
}

fn operation(value: u32) -> Option<BlendOp> {
    Some(match value {
        1 => BlendOp::Add,
        2 => BlendOp::Subtract,
        3 => BlendOp::ReverseSubtract,
        4 => BlendOp::Minimum,
        5 => BlendOp::Maximum,
        _ => return None,
    })
}

fn compare(value: u32) -> Option<Compare> {
    Some(match value {
        1 => Compare::Never,
        2 => Compare::Less,
        3 => Compare::Equal,
        4 => Compare::LessEqual,
        5 => Compare::Greater,
        6 => Compare::NotEqual,
        7 => Compare::GreaterEqual,
        8 => Compare::Always,
        _ => return None,
    })
}

fn stencil_op(value: u32) -> Option<StencilOp> {
    Some(match value {
        1 => StencilOp::Keep,
        2 => StencilOp::Zero,
        3 => StencilOp::Replace,
        4 => StencilOp::IncrementClamp,
        5 => StencilOp::DecrementClamp,
        6 => StencilOp::Invert,
        7 => StencilOp::IncrementWrap,
        8 => StencilOp::DecrementWrap,
        _ => return None,
    })
}

fn address(value: u32) -> Option<Address> {
    Some(match value {
        1 => Address::Repeat,
        2 => Address::Mirror,
        3 => Address::Clamp,
        4 => Address::Border,
        _ => return None,
    })
}

fn rgba(argb: u32) -> u32 {
    (argb & 0xff00_ff00) | ((argb >> 16) & 0xff) | ((argb & 0xff) << 16)
}

fn stage_enabled(stage: &StateMap, has_texture: bool, rules: Rules) -> bool {
    rules.texture_op(stage[&Stage::ColorOp]) != 1
        && (has_texture || stage[&Stage::ColorArg1] & 0xf != 2)
}

pub(crate) fn second_stage(
    f: &State,
    has_texture: [bool; 2],
    rules: Rules,
) -> Result<Option<[u32; 4]>, LowerError> {
    let s = &f.stage_slots[1];
    if !stage_enabled(&f.stage, has_texture[0], rules) || !stage_enabled(s, has_texture[1], rules) {
        return Ok(None);
    }
    // Diffuse, current, texture and texture factor, including argument modifiers.
    if [
        Stage::ColorArg1,
        Stage::ColorArg2,
        Stage::AlphaArg1,
        Stage::AlphaArg2,
    ]
    .iter()
    .any(|k| s[k] & 0xf > 3)
    {
        return Err(LowerError::TextureArgument);
    }
    Ok(Some([
        rules.texture_op(s[&Stage::ColorOp]),
        s[&Stage::ColorArg1] | s[&Stage::ColorArg2] << 8,
        rules.texture_op(s[&Stage::AlphaOp])
            | s[&Stage::AlphaArg1] << 8
            | s[&Stage::AlphaArg2] << 16,
        s[&Stage::CoordinateIndex],
    ]))
}

pub(crate) fn compact_stage_ops(f: &State, rules: Rules) -> Option<(TextureOp, TextureOp)> {
    Some((
        combine(
            rules.texture_op(f.stage[&Stage::ColorOp]),
            f.stage[&Stage::ColorArg1],
            f.stage[&Stage::ColorArg2],
        )?,
        combine(
            rules.texture_op(f.stage[&Stage::AlphaOp]),
            f.stage[&Stage::AlphaArg1],
            f.stage[&Stage::AlphaArg2],
        )?,
    ))
}

fn combine(op: u32, arg1: u32, arg2: u32) -> Option<TextureOp> {
    let arg = |v| match v {
        0 | 1 | 3..=0x3f => Some(TextureOp::Color),
        2 => Some(TextureOp::Texture),
        _ => None,
    };
    Some(match op {
        1 => TextureOp::Color,
        2 => arg(arg1)?,
        3 => arg(arg2)?,
        4..=7 if (arg1 == 2) != (arg2 == 2) => match op {
            4 => TextureOp::Multiply,
            5 => TextureOp::Multiply2x,
            6 => TextureOp::Multiply4x,
            _ => TextureOp::Add,
        },
        10 if arg1 <= 1 && arg2 == 2 => TextureOp::Subtract,
        _ if arg1 > 2 || arg2 > 2 => TextureOp::Color,
        _ => return None,
    })
}

pub(crate) const STAGE_ARGUMENTS: u32 = 1 << 3;

pub(crate) fn features(f: &State, transformed: Option<bool>, rules: Rules) -> u32 {
    let stage_arguments = [
        Stage::ColorArg1,
        Stage::ColorArg2,
        Stage::AlphaArg1,
        Stage::AlphaArg2,
    ]
    .iter()
    .any(|k| {
        f.stage[k] > 2
            || (rules.texture_op(f.stage_slots[1][&Stage::ColorOp]) != 1 && f.stage_slots[1][k] > 2)
    }) || compact_stage_ops(f, rules).is_none();
    u32::from(transformed == Some(false) && f.render[&Render::Lighting] != 0)
        | (u32::from(f.render[&Render::SpecularEnable] != 0) << 1)
        | (u32::from(f.render[&Render::ShadeMode] == 1) << 2)
        | (u32::from(stage_arguments) * STAGE_ARGUMENTS)
        | (u32::from(f.render[&Render::SrgbWrite] != 0) << 4)
        | (u32::from(f.render[&Render::FogEnable] != 0) << 5)
        | (u32::from(
            f.render[&Render::PointScale] != 0
                || f.render[&Render::PointSprite] != 0
                || f.render[&Render::PointSize] != 1f32.to_bits(),
        ) << 6)
        | (u32::from(
            f.render[&Render::SlopeDepthBias] != 0
                || f.render[&Render::DepthBias] != 0
                || rules.integer_zbias && f.render[&Render::ZBias] != 0,
        ) << 7)
        | (u32::from(transformed == Some(false) && f.render[&Render::PointScale] != 0) << 8)
}
impl State {
    pub(crate) fn program_sampler(
        &self,
        slot: usize,
    ) -> Result<adv_device::gpu::Sampler, LowerError> {
        let sampler = if slot == 0 {
            &self.sampler
        } else {
            self.sampler_slots
                .get(slot)
                .ok_or(LowerError::InvalidValue)?
        };
        Ok(adv_device::gpu::Sampler {
            u: address(sampler[&Sampler::AddressU]).ok_or(LowerError::InvalidValue)?,
            v: address(sampler[&Sampler::AddressV]).ok_or(LowerError::InvalidValue)?,
            min: if sampler[&Sampler::MinFilter] == 2 {
                Filter::Linear
            } else {
                Filter::Nearest
            },
            mag: if sampler[&Sampler::MagFilter] == 2 {
                Filter::Linear
            } else {
                Filter::Nearest
            },
            mip: match sampler[&Sampler::MipFilter] {
                0 => None,
                1 => Some(Filter::Nearest),
                _ => Some(Filter::Linear),
            },
            first_level: sampler[&Sampler::MaxMipLevel],
            border_color: rgba(sampler[&Sampler::BorderColor]),
        })
    }
}

pub(crate) fn lower(f: &State, layout: &Layout, rules: Rules) -> Result<Lowered, LowerError> {
    let features = features(
        f,
        layout.vertex.as_ref().map(VertexProgram::transformed),
        rules,
    );
    let mut state = DrawState::new(layout.extent.0, layout.extent.1);
    state.last_pixel = rules
        .line_last_pixel
        .unwrap_or(f.render[&Render::LastPixel] != 0);
    state.topology = layout.topology;
    state.cull = match f.render[&Render::CullMode] {
        1 => Cull::None,
        2 => Cull::Clockwise,
        _ => Cull::CounterClockwise,
    };
    if layout.depth {
        if f.render[&Render::DepthEnable] != 0 {
            state.depth =
                Some(compare(f.render[&Render::DepthCompare]).ok_or(LowerError::InvalidValue)?);
            state.depth_write = f.render[&Render::DepthWrite] != 0;
        }
        // Native D3D9 ignores stencil state when the bound depth format has
        // no stencil component (D16/D24X8), even STENCILFUNC=NEVER. Keep the
        // requested state observable and preserve the independent depth test.
        if f.render[&Render::StencilEnable] != 0 && layout.stencil {
            let front = StencilFace {
                compare: compare(f.render[&Render::StencilCompare])
                    .ok_or(LowerError::InvalidValue)?,
                fail: stencil_op(f.render[&Render::StencilFail]).ok_or(LowerError::InvalidValue)?,
                depth_fail: stencil_op(f.render[&Render::StencilDepthFail])
                    .ok_or(LowerError::InvalidValue)?,
                pass: stencil_op(f.render[&Render::StencilPass]).ok_or(LowerError::InvalidValue)?,
            };
            let back = if f.render[&Render::TwoSidedStencil] != 0 {
                StencilFace {
                    compare: compare(f.render[&Render::BackStencilCompare])
                        .ok_or(LowerError::InvalidValue)?,
                    fail: stencil_op(f.render[&Render::BackStencilFail])
                        .ok_or(LowerError::InvalidValue)?,
                    depth_fail: stencil_op(f.render[&Render::BackStencilDepthFail])
                        .ok_or(LowerError::InvalidValue)?,
                    pass: stencil_op(f.render[&Render::BackStencilPass])
                        .ok_or(LowerError::InvalidValue)?,
                }
            } else {
                front
            };
            // D3D's clockwise screen-space face maps to the GPU back face
            // after our Y projection. CCW override therefore selects GPU front.
            state.stencil = Some(Stencil {
                front: back,
                back: front,
                read_mask: f.render[&Render::StencilReadMask],
                write_mask: f.render[&Render::StencilWriteMask],
            });
            state.stencil_reference = f.render[&Render::StencilReference];
        }
    }
    if f.render[&Render::BlendEnable] != 0 {
        state.blend = Some(Blend {
            source: factor(f.render[&Render::SourceBlend]).ok_or(LowerError::InvalidValue)?,
            destination: factor(f.render[&Render::DestinationBlend])
                .ok_or(LowerError::InvalidValue)?,
            operation: operation(f.render[&Render::BlendOperation])
                .ok_or(LowerError::InvalidValue)?,
        });
        match f.render[&Render::SourceBlend] {
            12 => state.blend.as_mut().unwrap().destination = Factor::OneMinusSourceAlpha,
            13 => state.blend.as_mut().unwrap().destination = Factor::SourceAlpha,
            _ => {}
        }
        if [
            Render::SourceBlend,
            Render::DestinationBlend,
            Render::SourceBlendAlpha,
            Render::DestinationBlendAlpha,
        ]
        .iter()
        .any(|k| matches!(f.render[k], 14 | 15))
        {
            state.blend_constant = Some(rgba(f.render[&Render::BlendFactor]));
        }
        if f.render[&Render::SeparateAlphaBlend] != 0 {
            state.alpha_blend = Some(Blend {
                source: factor(f.render[&Render::SourceBlendAlpha])
                    .ok_or(LowerError::InvalidValue)?,
                destination: factor(f.render[&Render::DestinationBlendAlpha])
                    .ok_or(LowerError::InvalidValue)?,
                operation: operation(f.render[&Render::BlendOperationAlpha])
                    .ok_or(LowerError::InvalidValue)?,
            });
        }
    }
    state.color_write = f.render[&Render::ColorWrite] as u8;
    state.alpha_test = if f.render[&Render::AlphaTest] != 0 {
        compare(f.render[&Render::AlphaCompare]).ok_or(LowerError::InvalidValue)?
    } else {
        Compare::Always
    };
    state.alpha_reference = (f.render[&Render::AlphaReference] & 255) as f32 / 255.;
    state.viewport = Viewport {
        x: f.viewport.rect.x as f32,
        y: f.viewport.rect.y as f32,
        width: f.viewport.rect.w as f32,
        height: f.viewport.rect.h as f32,
        minimum_depth: f.viewport.min_depth,
        maximum_depth: f.viewport.max_depth,
    };
    if f.render[&Render::ScissorEnable] != 0 {
        let r = f.scissor;
        let right = (r.x + r.w).min(layout.extent.0);
        let bottom = (r.y + r.h).min(layout.extent.1);
        if right <= r.x || bottom <= r.y {
            return Err(LowerError::EmptyScissor);
        }
        state.scissor = Some(Rect {
            x: r.x,
            y: r.y,
            w: right - r.x,
            h: bottom - r.y,
        });
    }
    state.sampler = f.program_sampler(0)?;
    let (mip_levels, lod) = layout.texture.unwrap_or((1, 0));
    // Native D3D ignores MAXMIPLEVEL when MIPFILTER is NONE, but managed
    // texture residency LOD still selects its most detailed permitted level.
    let first = if state.sampler.mip.is_none() {
        lod
    } else {
        lod.max(f.sampler[&Sampler::MaxMipLevel])
    };
    state.sampler.first_level = first.min(mip_levels - 1);
    if layout.texture.is_some() && features & STAGE_ARGUMENTS == 0 {
        (state.texture_op, state.alpha_texture_op) =
            compact_stage_ops(f, rules).ok_or(LowerError::TextureOperation)?;
    }
    Ok(Lowered {
        state,
        program: ProgramKey {
            features,
            vertex: layout.vertex,
        },
        stage_zero: [
            if stage_enabled(&f.stage, layout.stage_zero_bound, rules) {
                rules.texture_op(f.stage[&Stage::ColorOp])
            } else {
                1
            },
            f.stage[&Stage::ColorArg1] | f.stage[&Stage::ColorArg2] << 8,
            rules.texture_op(f.stage[&Stage::AlphaOp]),
            f.stage[&Stage::AlphaArg1] | f.stage[&Stage::AlphaArg2] << 8,
        ],
        raster_parameters: [
            if rules.integer_zbias {
                (-(f.render[&Render::ZBias] as i32 as f32) / 65535.).to_bits()
            } else {
                f.render[&Render::DepthBias]
            },
            f.render[&Render::SlopeDepthBias],
            rules.color_key_mask.unwrap_or(0),
            u32::from(rules.integer_zbias) | (u32::from(rules.round_points) << 1),
        ],
    })
}
