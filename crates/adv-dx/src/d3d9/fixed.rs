//! Translate documented D3D9 fixed-function state into neutral drawing commands.
use super::*;
use adv_abi::types::D3DVIEWPORT9;
use adv_device::gpu::{
    Address, Blend, BlendOp, Compare, Cull, DrawState, Factor, Filter, Image, Stencil, StencilFace,
    StencilOp, TextureOp, Topology, Vertex, Viewport,
};
use adv_device::surface::Rect;
mod input;

#[derive(Clone)]
pub(super) struct Fixed {
    pub(super) fvf: u32,
    pub(super) render: BTreeMap<i32, u32>,
    pub(super) stage: BTreeMap<i32, u32>,
    pub(super) stage_slots: [BTreeMap<i32, u32>; 8],
    pub(super) sampler: BTreeMap<i32, u32>,
    pub(super) sampler_slots: [BTreeMap<i32, u32>; 20],
    pub(super) viewport: D3DVIEWPORT9,
    pub(super) scissor: RECT,
    pub(super) transforms: [[f32; 16]; 3],
    pub(super) clip_planes: [[f32; 4]; 6],
}
/// Byte offsets and source streams of the FVF-equivalent fixed inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub transformed: bool,
    pub position: u32,
    pub color: Option<u32>,
    pub texcoords: [Option<(u16, u32, u8)>; 8],
    /// POSITION/POSITIONT and COLOR0 source stream numbers.
    pub streams: [u16; 2],
    /// Largest per-stream extent, also the vertex size for interleaved inputs.
    pub size: u32,
}
impl Layout {
    pub(super) fn from_fvf(fvf: u32) -> Option<Self> {
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
        let color = field(fvf & 0x40 != 0, 4);
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
            texcoords,
            streams: [0; 2],
            size,
        })
    }
}
impl Layout {
    pub(super) fn coordinate_count(self) -> usize {
        self.texcoords
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |n| n + 1)
    }
    fn extended_coordinates(self) -> bool {
        self.coordinate_count() > 1
            || self.texcoords[0].is_some_and(|(_, _, dimensions)| dimensions != 2)
    }
}
pub(super) struct Draw {
    pub target: Target,
    pub image: Option<Image>,
    pub vertices: Vec<Vertex>,
    pub texcoords: Option<(usize, Vec<[f32; 4]>)>,
    pub state: DrawState,
    pub programmable: Option<adv_device::gpu::ProgrammableDraw>,
    pub recording_key: u128,
    pub program_key: u128,
}
pub(super) struct Rejected {
    pub code: u32,
    pub reason: &'static str,
}
impl From<u32> for Rejected {
    fn from(code: u32) -> Self {
        Self {
            code,
            reason: "invalid or unsupported fixed-function state value",
        }
    }
}
fn rejected(code: u32, reason: &'static str) -> Rejected {
    Rejected { code, reason }
}
impl Fixed {
    pub(super) fn program_sampler(&self, slot: usize) -> Result<adv_device::gpu::Sampler, u32> {
        let s = if slot == 0 {
            &self.sampler
        } else {
            self.sampler_slots.get(slot).ok_or(INVALID)?
        };
        Ok(adv_device::gpu::Sampler {
            u: address(s[&1]).ok_or(INVALID)?,
            v: address(s[&2]).ok_or(INVALID)?,
            min: if s[&6] == 2 {
                Filter::Linear
            } else {
                Filter::Nearest
            },
            mag: if s[&5] == 2 {
                Filter::Linear
            } else {
                Filter::Nearest
            },
            mip: match s[&7] {
                0 => None,
                1 => Some(Filter::Nearest),
                _ => Some(Filter::Linear),
            },
            first_level: s[&9],
            border_color: rgba(s[&4]),
        })
    }
    pub(super) fn viewport_rect(&self) -> Rect {
        Rect {
            x: self.viewport.X,
            y: self.viewport.Y,
            w: self.viewport.Width,
            h: self.viewport.Height,
        }
    }
    pub(super) fn clear_rect(&self) -> Rect {
        let viewport = self.viewport_rect();
        if self.render[&174] == 0 {
            return viewport;
        }
        let left = viewport.x.max(self.scissor.left as u32);
        let top = viewport.y.max(self.scissor.top as u32);
        let right = (viewport.x + viewport.w).min(self.scissor.right as u32);
        let bottom = (viewport.y + viewport.h).min(self.scissor.bottom as u32);
        if left >= right || top >= bottom {
            return Rect {
                x: viewport.x,
                y: viewport.y,
                w: 0,
                h: 0,
            };
        }
        Rect {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }
    pub(super) fn clear_fvf(&mut self) {
        self.fvf = 0;
    }
    pub(super) fn auto_depth(&mut self, enabled: bool) {
        self.render.insert(7, u32::from(enabled));
    }
    pub(super) fn target_changed(&mut self, width: u32, height: u32) {
        self.scissor = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        self.viewport = D3DVIEWPORT9 {
            X: 0,
            Y: 0,
            Width: width,
            Height: height,
            MinZ: 0.,
            MaxZ: 1.,
        };
    }
    pub fn new(width: u32, height: u32) -> Self {
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
        let identity = [
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
        ];
        Self {
            fvf: 0,
            render: BTreeMap::from([
                (7, 0),
                (8, 3), // FILLMODE: only solid rasterization is implemented.
                (9, 2), // SHADEMODE: the shared shader interpolates vertex colors.
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
                (154, 1f32.to_bits()),
                (155, 1f32.to_bits()),
                (156, 0),
                (166, 64f32.to_bits()),
                (29, 0), // SPECULARENABLE: supported FVFs supply zero specular RGB.
                (26, 0), // DITHERENABLE: the renderer does not dither.
                (52, 0),
                (53, 1),
                (54, 1),
                (55, 1),
                (56, 8),
                (57, 0),
                (58, u32::MAX),
                (59, u32::MAX),
                (128, 0), // WRAP0: texture coordinate wrapping is disabled.
                (136, 1), // CLIPPING: GPU primitive clipping remains enabled.
                (137, 1),
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
            ]),
            stage_slots: std::array::from_fn(|stage| {
                let mut states =
                    BTreeMap::from([(7, 0), (8, 0), (9, 0), (10, 0), (22, 0), (23, 0)]);
                if stage != 0 {
                    states.extend([(1, 1), (2, 2), (3, 1), (4, 1), (5, 2), (6, 1)]);
                }
                states
            }),
            stage: BTreeMap::from([
                (1, 4),
                (2, 2),
                (3, 1),
                (4, 2),
                (5, 2),
                (6, 1),
                (11, 0), // TEXCOORDINDEX: pass through the first FVF coordinate set.
                (24, 0), // TEXTURETRANSFORMFLAGS: no texture coordinate transform.
            ]),
            sampler: sampler.clone(),
            sampler_slots: std::array::from_fn(|_| sampler.clone()),
            viewport: D3DVIEWPORT9 {
                X: 0,
                Y: 0,
                Width: width,
                Height: height,
                MinZ: 0.,
                MaxZ: 1.,
            },
            scissor: RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            },
            transforms: [identity; 3],
            clip_planes: [[0.; 4]; 6],
        }
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        set_fvf_ABI,
        get_fvf_ABI,
        set_render_ABI,
        get_render_ABI,
        set_stage_ABI,
        get_stage_ABI,
        set_sampler_ABI,
        get_sampler_ABI,
        set_viewport_ABI,
        get_viewport_ABI,
        set_scissor_ABI,
        get_scissor_ABI,
        set_transform_ABI,
        get_transform_ABI,
        adv_abi::Api {
            env_leaf: Some(super::leaf_calls::draw),
            ..draw_up_ABI
        },
    ] {
        registry.register(api);
    }
}
fn mutable<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Fixed> {
    match com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)? {
        Payload::Device(d) => Some(if let Some(recording) = &mut d.recording {
            &mut recording.fixed
        } else {
            &mut d.fixed
        }),
        _ => None,
    }
}
fn current<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Fixed> {
    Some(&device(env, pointer)?.fixed)
}
fn valid_fvf(fvf: u32) -> bool {
    // SPECULARENABLE is observable state, but these prelit/lighting-off inputs
    // have no specular color. Keep SPECULAR (0x80) unavailable until that color
    // is propagated through the neutral vertex and post-texture RGB stage.
    let count = (fvf >> 8) & 15;
    let sizes = fvf >> 16;
    matches!(fvf & 0x0e, 2 | 4)
        && fvf & !(0x0e | 0x40 | 0xf00 | 0xffff0000) == 0
        && count <= 8
        && (count == 8 || sizes >> (count * 2) == 0)
}
#[win32(d3d9,name="IDirect3DDevice9::SetFVF",level=state)]
pub(super) fn set_fvf(env: &mut Env<'_>, this: GuestPtr<()>, fvf: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if !valid_fvf(fvf) {
        return UNAVAILABLE;
    }
    f.fvf = fvf;
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Fvf,
    );
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if let Some(code) = super::shaders::stateblocks::record_declaration(env, device, None) {
        if code == 0 {
            mutable(env, this.addr()).unwrap().fvf = fvf;
            super::shaders::stateblocks::mark_fixed(
                env,
                this.addr(),
                super::shaders::stateblocks::FixedKey::Fvf,
            );
        }
        return code;
    }
    if device_declaration(env, this.addr()).is_some() {
        // Unbinding a declaration clears the FVF it replaced; restore ours.
        let code = super::declaration::bind(env, device, None);
        if code != 0 {
            return code;
        }
        mutable(env, this.addr()).unwrap().fvf = fvf;
    }
    0
}
fn device_declaration(env: &Env<'_>, pointer: u32) -> Option<Id> {
    device(env, pointer)?.buffers.declaration
}
fn layout(env: &Env<'_>, pointer: u32) -> Result<Layout, Rejected> {
    let d = device(env, pointer).ok_or_else(|| rejected(INVALID, "invalid device interface"))?;
    match d.buffers.declaration {
        Some(id) => {
            super::declaration::layout(env, id).map_err(|reason| rejected(UNAVAILABLE, reason))
        }
        None => Layout::from_fvf(d.fixed.fvf)
            .ok_or_else(|| rejected(INVALID, "no supported FVF is set")),
    }
}
#[win32(d3d9,name="IDirect3DDevice9::GetFVF",level=implement,leaf)]
pub(super) fn get_fvf(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    let fvf = f.fvf;
    if output.write(env.cpu, fvf).is_ok() {
        0
    } else {
        INVALID
    }
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
#[win32(d3d9,name="IDirect3DDevice9::SetRenderState",level=state,leaf)]
fn set_render(env: &mut Env<'_>, this: GuestPtr<()>, state: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let valid = match state {
        8 => match value {
            3 => true,
            1 | 2 => return UNAVAILABLE,
            _ => false,
        },
        9 => match value {
            2 => true, // GOURAUD is the existing neutral renderer's shading mode.
            1 | 3 => return UNAVAILABLE,
            _ => false,
        },
        7 | 14 | 15 | 16 | 27 | 29 | 52 | 137 | 141 | 161 | 174 | 185 | 206 => value <= 1,
        26 => match value {
            0 => true,
            1 => return UNAVAILABLE,
            _ => false,
        },
        28 | 156 => value <= 1,
        34 => true,
        35 | 140 => value <= 3,
        36..=38 => f32::from_bits(value).is_finite(),
        154 | 155 | 166 => f32::from_bits(value).is_finite() && f32::from_bits(value) > 0.0,
        167 => match value {
            0 => true,
            1 => return UNAVAILABLE,
            _ => false,
        },
        136 => match value {
            1 => true,
            0 => return UNAVAILABLE,
            _ => false,
        },
        // Material sources only affect lighting. Supported prelit and
        // lighting-off draws use vertex colors; lit draws still fail explicitly.
        145..=148 => value <= 2,
        151 => match value {
            0 => true,
            1..=3 | 255 | 256 => return UNAVAILABLE,
            _ => false,
        },
        152 => value & !63 == 0,
        19 | 20 | 207 | 208 => factor(value).is_some(),
        22 => (1..=3).contains(&value),
        23 | 25 | 56 | 189 => compare(value).is_some(),
        53..=55 | 186..=188 => stencil_op(value).is_some(),
        24 | 57..=59 => true,
        168 => value <= 15,
        128 => match value {
            0 => true,
            1..=15 => return UNAVAILABLE,
            _ => false,
        },
        171 | 209 => operation(value).is_some(),
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    f.render.insert(state, value);
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Render(state),
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetRenderState",level=implement,leaf)]
fn get_render(env: &mut Env<'_>, this: GuestPtr<()>, state: i32, output: GuestPtr<u32>) -> u32 {
    let Some(&value) = current(env, this.addr()).and_then(|f| f.render.get(&state)) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetTextureStageState",level=state,leaf)]
fn set_stage(env: &mut Env<'_>, this: GuestPtr<()>, stage: u32, kind: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if stage >= 8 {
        return INVALID;
    }
    if matches!(kind, 7..=10 | 22 | 23) {
        f.stage_slots[stage as usize].insert(kind, value);
        super::shaders::stateblocks::mark_fixed(
            env,
            this.addr(),
            super::shaders::stateblocks::FixedKey::StageSlot(stage as usize, kind),
        );
        return 0;
    }
    if stage != 0 {
        // The fixed renderer stops at disabled stages. Store their arguments
        // for Get/state blocks, but reject enabling an unimplemented combiner.
        let valid = match kind {
            1 | 4 => match value {
                1 => true,
                2..=26 => return UNAVAILABLE,
                _ => false,
            },
            2 | 3 | 5 | 6 => value & !0x3f == 0 && value & 0xf <= 6,
            _ => return UNAVAILABLE,
        };
        if !valid {
            return INVALID;
        }
        f.stage_slots[stage as usize].insert(kind, value);
        super::shaders::stateblocks::mark_fixed(
            env,
            this.addr(),
            super::shaders::stateblocks::FixedKey::StageSlot(stage as usize, kind),
        );
        return 0;
    }
    let valid = match kind {
        1 | 4 => matches!(value, 1 | 2 | 3 | 4 | 7 | 10),
        2 | 3 | 5 | 6 => value <= 2,
        11 => {
            // Up to eight FVF coordinate sets and the SDK's four automatic
            // generation modes are legal; this decoder supplies set zero only.
            if value & 0xffff > 7 || value >> 16 > 4 {
                false
            } else if value != 0 {
                return UNAVAILABLE;
            } else {
                true
            }
        }
        24 => match value {
            0 => true,
            1..=4 | 256..=260 => return UNAVAILABLE,
            _ => false,
        },
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    f.stage.insert(kind, value);
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Stage(kind),
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetTextureStageState",level=implement,leaf)]
fn get_stage(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    if stage >= 8 {
        return INVALID;
    }
    let states = if stage != 0 || matches!(kind, 7..=10 | 22 | 23) {
        &f.stage_slots[stage as usize]
    } else {
        &f.stage
    };
    let Some(&value) = states.get(&kind) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
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
/// Neutral packed RGBA keeps red in the low byte; D3DCOLOR is packed ARGB.
fn rgba(argb: u32) -> u32 {
    (argb & 0xff00_ff00) | ((argb >> 16) & 0xff) | ((argb & 0xff) << 16)
}
#[win32(d3d9,name="IDirect3DDevice9::SetSamplerState",level=state,leaf)]
fn set_sampler(env: &mut Env<'_>, this: GuestPtr<()>, sampler: u32, kind: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let Some(slot) = super::programmable::sampler_slot(sampler) else {
        return INVALID;
    };
    let valid = match kind {
        1 | 2 | 3 => address(value).is_some(),
        5 | 6 => matches!(value, 1 | 2),
        7 => value <= 2,
        4 | 9 => true,
        // These neutral values require no additional sampling operation. Keep
        // exact DWORDs (including signed zero); nonneutral requests stay explicit.
        8 if f32::from_bits(value) == 0. => true,
        10 if value == 1 => true,
        11..=13 if value == 0 => true,
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    if slot == 0 {
        f.sampler.insert(kind, value);
    } else {
        f.sampler_slots[slot].insert(kind, value);
    }
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        if slot == 0 {
            super::shaders::stateblocks::FixedKey::Sampler(kind)
        } else {
            super::shaders::stateblocks::FixedKey::SamplerSlot(slot, kind)
        },
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetSamplerState",level=implement,leaf)]
fn get_sampler(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    sampler: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    let Some(slot) = super::programmable::sampler_slot(sampler) else {
        return INVALID;
    };
    let Some(&value) = current(env, this.addr()).and_then(|f| {
        if slot == 0 {
            f.sampler.get(&kind)
        } else {
            f.sampler_slots[slot].get(&kind)
        }
    }) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetViewport",level=state,leaf)]
fn set_viewport(env: &mut Env<'_>, this: GuestPtr<()>, viewport: GuestPtr<D3DVIEWPORT9>) -> u32 {
    let Ok(v) = viewport.read(env.cpu) else {
        return INVALID;
    };
    let Ok(target) = super::targets::draw_target(env, this.addr()) else {
        return INVALID;
    };
    if v.Width == 0
        || v.Height == 0
        || v.X
            .checked_add(v.Width)
            .is_none_or(|x| x > target.image.width())
        || v.Y
            .checked_add(v.Height)
            .is_none_or(|y| y > target.image.height())
        || !v.MinZ.is_finite()
        || !v.MaxZ.is_finite()
        || v.MinZ < 0.
        || v.MaxZ > 1.
        || v.MinZ > v.MaxZ
    {
        return INVALID;
    }
    mutable(env, this.addr()).unwrap().viewport = v;
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Viewport,
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetViewport",level=implement,leaf)]
fn get_viewport(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DVIEWPORT9>) -> u32 {
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    let v = f.viewport;
    if output.write(env.cpu, v).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetScissorRect",level=state,leaf)]
fn set_scissor(env: &mut Env<'_>, this: GuestPtr<()>, rect: GuestPtr<RECT>) -> u32 {
    let Ok(rect) = rect.read(env.cpu) else {
        return INVALID;
    };
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if rect.left < 0 || rect.top < 0 || rect.right <= rect.left || rect.bottom <= rect.top {
        return INVALID;
    }
    f.scissor = rect;
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Scissor,
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetScissorRect",level=implement,leaf)]
fn get_scissor(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<RECT>) -> u32 {
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    let v = f.scissor;
    if output.write(env.cpu, v).is_ok() {
        0
    } else {
        INVALID
    }
}
fn transform_slot(kind: i32) -> Option<usize> {
    Some(match kind {
        256 => 0,
        2 => 1,
        3 => 2,
        _ => return None,
    })
}
#[win32(d3d9,name="IDirect3DDevice9::SetTransform",level=state,leaf)]
fn set_transform(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    matrix: GuestPtr<[f32; 16]>,
) -> u32 {
    let Some(slot) = transform_slot(kind) else {
        return UNAVAILABLE;
    };
    let Ok(matrix) = matrix.read(env.cpu) else {
        return INVALID;
    };
    if matrix.iter().any(|v| !v.is_finite()) {
        return INVALID;
    }
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    f.transforms[slot] = matrix;
    super::shaders::stateblocks::mark_fixed(
        env,
        this.addr(),
        super::shaders::stateblocks::FixedKey::Transform(slot),
    );
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetTransform",level=implement,leaf)]
fn get_transform(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    output: GuestPtr<[f32; 16]>,
) -> u32 {
    let Some(slot) = transform_slot(kind) else {
        return INVALID;
    };
    let Some(f) = current(env, this.addr()) else {
        return INVALID;
    };
    let matrix = f.transforms[slot];
    if output.write(env.cpu, matrix).is_ok() {
        0
    } else {
        INVALID
    }
}
fn combine(op: u32, arg1: u32, arg2: u32) -> Option<TextureOp> {
    let arg = |v| match v {
        0 | 1 => Some(TextureOp::Color),
        2 => Some(TextureOp::Texture),
        _ => None,
    };
    Some(match op {
        1 => TextureOp::Color,
        2 => arg(arg1)?,
        3 => arg(arg2)?,
        4 | 7 if (arg1 == 2) != (arg2 == 2) => {
            if op == 4 {
                TextureOp::Multiply
            } else {
                TextureOp::Add
            }
        }
        10 if arg1 <= 1 && arg2 == 2 => TextureOp::Subtract,
        _ => return None,
    })
}
fn row(vector: [f32; 4], matrix: &[f32; 16]) -> [f32; 4] {
    std::array::from_fn(|column| (0..4).map(|i| vector[i] * matrix[i * 4 + column]).sum())
}
/// Draw-bound buffers supply every declared input stream. UP callers still
/// supply only their explicit stream-zero pointer through prepare_checked.
pub(super) fn prepare_bound(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    start: u32,
    indices: Option<&[u32]>,
) -> Result<Draw, u32> {
    if log::log_enabled!(log::Level::Debug)
        && let Some(id) = device(env, pointer).and_then(|d| d.buffers.declaration)
    {
        log::debug!(
            target: "adv_dx::d3d9::fixed::input",
            "bound declaration {}",
            super::declaration::describe(env, id)
        );
    }
    prepare_input_checked(
        env,
        pointer,
        kind,
        count,
        input::Input::Bound { start },
        indices,
    )
    .map_err(|error| {
        if error.code == UNAVAILABLE {
            warn_rejected(
                env,
                if indices.is_some() {
                    "IDirect3DDevice9::DrawIndexedPrimitive"
                } else {
                    "IDirect3DDevice9::DrawPrimitive"
                },
                pointer,
                kind,
                GuestPtr::new(0),
                0,
                error.code,
                error.reason,
            );
        }
        error.code
    })
}
pub(super) fn primitive_length(kind: i32, count: u32) -> Result<u32, u32> {
    if count == 0 || count > 131_072 {
        return Err(INVALID);
    }
    match kind {
        1 => Some(count),
        2 => count.checked_mul(2),
        3 => count.checked_add(1),
        4 => count.checked_mul(3),
        5 | 6 => count.checked_add(2),
        _ => None,
    }
    .ok_or(INVALID)
}
pub(super) fn empty_draw(env: &Env<'_>, pointer: u32, kind: i32) -> Result<(), u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    if !d.scene || d.needs_reset || !matches!(kind, 1..=6) {
        return Err(INVALID);
    }
    // Zero batches do not dereference data, transform vertices or enqueue GPU
    // work. Their UP binding side effects remain the caller's responsibility.
    Ok(())
}
pub(super) fn prepare(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    data: GuestPtr<u8>,
    stride: u32,
    indices: Option<&[u32]>,
) -> Result<Draw, u32> {
    prepare_checked(env, pointer, kind, count, data, stride, indices).map_err(|error| {
        // Unsupported (not invalid) inputs name the missing capability.
        if error.code == UNAVAILABLE {
            let api = if indices.is_some() {
                "IDirect3DDevice9::DrawIndexedPrimitive"
            } else {
                "IDirect3DDevice9::DrawPrimitive"
            };
            warn_rejected(
                env,
                api,
                pointer,
                kind,
                data,
                stride,
                error.code,
                error.reason,
            );
        }
        error.code
    })
}
fn prepare_checked(
    env: &Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    data: GuestPtr<u8>,
    stride: u32,
    indices: Option<&[u32]>,
) -> Result<Draw, Rejected> {
    prepare_input_checked(
        env,
        pointer,
        kind,
        count,
        input::Input::Up {
            data: data.addr(),
            stride,
        },
        indices,
    )
}
fn prepare_input_checked(
    env: &Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    source: input::Input,
    indices: Option<&[u32]>,
) -> Result<Draw, Rejected> {
    let d = device(env, pointer).ok_or_else(|| rejected(INVALID, "invalid device interface"))?;
    let f = &d.fixed;
    if !d.scene {
        return Err(rejected(INVALID, "draw outside BeginScene/EndScene"));
    }
    if d.shaders.vertex.is_none()
        && d.shaders.pixel.is_none()
        && (f.render[&28] != 0
            || f.render[&152] != 0
            || (kind == 1 && (f.render[&154] != 1f32.to_bits() || f.render[&156] != 0)))
    {
        return Err(rejected(
            UNAVAILABLE,
            "fog, clip planes and sized points require the programmable draw path",
        ));
    }
    let layout = layout(env, pointer)?;
    if !layout.transformed && f.render[&137] != 0 {
        return Err(rejected(
            UNAVAILABLE,
            "untransformed vertices require unsupported lighting",
        ));
    }
    let length = primitive_length(kind, count)
        .map_err(|code| rejected(code, "invalid or excessive primitive count"))?;
    if indices.is_some_and(|v| v.len() != length as usize) {
        return Err(rejected(
            INVALID,
            "index count does not match primitive count",
        ));
    }
    let first = indices.and_then(|v| v.iter().min().copied()).unwrap_or(0);
    let last = indices
        .and_then(|v| v.iter().max().copied())
        .unwrap_or(length - 1);
    let mut input = input::Reader::new(env, pointer, layout, source, first, last)?;
    let mut input_key = super::recording::hash(indices);
    let mut vertices = Vec::with_capacity(length as usize);
    let coordinate_count = layout.coordinate_count();
    let mut texcoords = if layout.extended_coordinates() {
        let output_length = if kind == 6 {
            count.checked_mul(3).ok_or(INVALID)?
        } else {
            length
        };
        if output_length
            .checked_mul(32 + coordinate_count as u32 * 16)
            .is_none_or(|bytes| bytes > 16 * 1024 * 1024)
        {
            return Err(rejected(
                INVALID,
                "expanded texture-coordinate stream exceeds the draw budget",
            ));
        }
        Some(Vec::with_capacity(length as usize * coordinate_count))
    } else {
        None
    };
    for ordinal in 0..length {
        let index = indices.map_or(ordinal, |v| v[ordinal as usize]);
        input.read(env, index)?;
        input_key = super::recording::hash((input_key, input.key()));
        let word = |stream: u16, at: u32| input.word(stream, at);
        let float = |stream, at| f32::from_le_bytes(word(stream, at));
        let at = layout.position;
        let ps = layout.streams[0];
        let mut p = [float(ps, at), float(ps, at + 4), float(ps, at + 8), 1.];
        if layout.transformed {
            let rhw = float(ps, at + 12);
            if rhw == 0. || !rhw.is_finite() {
                return Err(rejected(INVALID, "vertex RHW is zero or not finite"));
            }
            let w = 1. / rhw;
            p = [
                (((p[0] + 0.5 - f.viewport.X as f32) / f.viewport.Width as f32) * 2. - 1.) * w,
                (1. - ((p[1] + 0.5 - f.viewport.Y as f32) / f.viewport.Height as f32) * 2.) * w,
                p[2] * w,
                w,
            ];
        } else {
            for matrix in &f.transforms {
                p = row(p, matrix);
            }
            // D3D9 integer pixel centres map to the host's half-integer centres.
            p[0] += p[3] / f.viewport.Width as f32;
            p[1] -= p[3] / f.viewport.Height as f32;
        }
        if p.iter().any(|v| !v.is_finite()) {
            return Err(rejected(
                INVALID,
                "transformed vertex position is not finite",
            ));
        }
        let mut color = [1.; 4];
        if let Some(at) = layout.color {
            let argb = u32::from_le_bytes(word(layout.streams[1], at));
            color = [
                ((argb >> 16) & 255) as f32 / 255.,
                ((argb >> 8) & 255) as f32 / 255.,
                (argb & 255) as f32 / 255.,
                (argb >> 24) as f32 / 255.,
            ];
        }
        let mut coordinates = [[0., 0., 0., 1.]; 8];
        for (n, coordinate) in layout.texcoords.iter().enumerate() {
            if let Some((stream, offset, dimensions)) = *coordinate {
                for component in 0..dimensions as usize {
                    coordinates[n][component] = float(stream, offset + component as u32 * 4);
                }
                if coordinates[n].iter().any(|v| !v.is_finite()) {
                    return Err(rejected(
                        INVALID,
                        "vertex texture coordinates are not finite",
                    ));
                }
            }
        }
        let uv = [coordinates[0][0], coordinates[0][1]];
        if let Some(values) = &mut texcoords {
            values.extend_from_slice(&coordinates[..coordinate_count]);
        }
        vertices.push(Vertex {
            position: p,
            color,
            uv,
        });
    }
    if kind == 6 {
        let mut fan = Vec::with_capacity(count as usize * 3);
        for i in 1..vertices.len() - 1 {
            fan.extend_from_slice(&[vertices[0], vertices[i], vertices[i + 1]]);
        }
        vertices = fan;
        if let Some(values) = &mut texcoords {
            let mut fan = Vec::with_capacity(count as usize * 3 * coordinate_count);
            for n in 1..length as usize - 1 {
                for index in [0, n, n + 1] {
                    fan.extend_from_slice(
                        &values[index * coordinate_count..(index + 1) * coordinate_count],
                    );
                }
            }
            *values = fan;
        }
    }
    let (target, image, state) = prepare_state(env, pointer, kind)?;
    Ok(Draw {
        target,
        image,
        vertices,
        texcoords: texcoords.map(|values| (coordinate_count, values)),
        state,
        programmable: None,
        recording_key: super::recording::hash((
            super::recording::draw_key(env, pointer),
            input_key,
        )),
        program_key: 0,
    })
}
pub(super) fn prepare_state(
    env: &Env<'_>,
    pointer: u32,
    kind: i32,
) -> Result<(Target, Option<Image>, DrawState), Rejected> {
    let d = device(env, pointer).ok_or(INVALID)?;
    let f = &d.fixed;
    let topology = match kind {
        1 => Topology::Points,
        2 => Topology::Lines,
        3 => Topology::LineStrip,
        4 | 6 => Topology::Triangles,
        5 => Topology::TriangleStrip,
        _ => return Err(INVALID.into()),
    };
    let provider = env.ext.get::<State>().provider;
    let target = super::targets::draw_target(env, pointer)
        .map_err(|code| rejected(code, "render target is missing, locked or unavailable"))?;
    let target = if f.render[&7] != 0 || f.render[&52] != 0 {
        super::depth::attach(env, pointer, target)
            .map_err(|code| rejected(code, "depth attachment is invalid or incompatible"))?
    } else {
        target
    };
    let (image, lod) = if d.shaders.pixel.is_some() {
        (None, 0)
    } else if let Some(id) = d.texture {
        let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
            return Err(rejected(
                INVALID,
                "bound texture no longer has a resource owner",
            ));
        };
        if resources::locked(env, r) {
            return Err(rejected(INVALID, "bound texture is locked"));
        }
        (
            Some(
                (provider.target)(
                    env.ext,
                    r.surface.ok_or_else(|| {
                        rejected(INVALID, "bound texture has no sampleable surface")
                    })?,
                )
                .ok_or_else(|| rejected(INVALID, "bound texture surface is unavailable"))?
                .image,
            ),
            r.lod,
        )
    } else {
        (None, 0)
    };
    let mut state = DrawState::new(target.image.width(), target.image.height());
    state.last_pixel = f.render[&16] != 0;
    state.topology = topology;
    state.cull = match f.render[&22] {
        1 => Cull::None,
        2 => Cull::Clockwise,
        _ => Cull::CounterClockwise,
    };
    if target.depth().is_some() {
        if f.render[&7] != 0 {
            state.depth = Some(compare(f.render[&23]).ok_or(INVALID)?);
            state.depth_write = f.render[&14] != 0;
        }
        // Native D3D9 ignores stencil state when the bound depth format has
        // no stencil component (D16/D24X8), even STENCILFUNC=NEVER. Keep the
        // requested state observable and preserve the independent depth test.
        if f.render[&52] != 0 && super::depth::has_stencil(env, pointer) {
            let front = StencilFace {
                compare: compare(f.render[&56]).ok_or(INVALID)?,
                fail: stencil_op(f.render[&53]).ok_or(INVALID)?,
                depth_fail: stencil_op(f.render[&54]).ok_or(INVALID)?,
                pass: stencil_op(f.render[&55]).ok_or(INVALID)?,
            };
            let back = if f.render[&185] != 0 {
                StencilFace {
                    compare: compare(f.render[&189]).ok_or(INVALID)?,
                    fail: stencil_op(f.render[&186]).ok_or(INVALID)?,
                    depth_fail: stencil_op(f.render[&187]).ok_or(INVALID)?,
                    pass: stencil_op(f.render[&188]).ok_or(INVALID)?,
                }
            } else {
                front
            };
            // D3D's clockwise screen-space face maps to the GPU back face
            // after our Y projection. CCW override therefore selects GPU front.
            state.stencil = Some(Stencil {
                front: back,
                back: front,
                read_mask: f.render[&58],
                write_mask: f.render[&59],
            });
            state.stencil_reference = f.render[&57];
        }
    }
    if f.render[&27] != 0 {
        state.blend = Some(Blend {
            source: factor(f.render[&19]).ok_or(INVALID)?,
            destination: factor(f.render[&20]).ok_or(INVALID)?,
            operation: operation(f.render[&171]).ok_or(INVALID)?,
        });
        if f.render[&206] != 0 {
            state.alpha_blend = Some(Blend {
                source: factor(f.render[&207]).ok_or(INVALID)?,
                destination: factor(f.render[&208]).ok_or(INVALID)?,
                operation: operation(f.render[&209]).ok_or(INVALID)?,
            });
        }
    }
    state.color_write = f.render[&168] as u8;
    state.alpha_test = if f.render[&15] != 0 {
        compare(f.render[&25]).ok_or(INVALID)?
    } else {
        Compare::Always
    };
    state.alpha_reference = (f.render[&24] & 255) as f32 / 255.;
    state.viewport = Viewport {
        x: f.viewport.X as f32,
        y: f.viewport.Y as f32,
        width: f.viewport.Width as f32,
        height: f.viewport.Height as f32,
        minimum_depth: f.viewport.MinZ,
        maximum_depth: f.viewport.MaxZ,
    };
    if f.render[&174] != 0 {
        let r = f.scissor;
        let right = (r.right as u32).min(target.image.width());
        let bottom = (r.bottom as u32).min(target.image.height());
        if right <= r.left as u32 || bottom <= r.top as u32 {
            return Err(rejected(
                INVALID,
                "scissor rectangle is empty outside render target",
            ));
        }
        state.scissor = Some(Rect {
            x: r.left as u32,
            y: r.top as u32,
            w: right - r.left as u32,
            h: bottom - r.top as u32,
        });
    }
    state.sampler.u = address(f.sampler[&1]).ok_or(INVALID)?;
    state.sampler.v = address(f.sampler[&2]).ok_or(INVALID)?;
    state.sampler.border_color = rgba(f.sampler[&4]);
    state.sampler.min = if f.sampler[&6] == 2 {
        Filter::Linear
    } else {
        Filter::Nearest
    };
    state.sampler.mag = if f.sampler[&5] == 2 {
        Filter::Linear
    } else {
        Filter::Nearest
    };
    state.sampler.mip = match f.sampler[&7] {
        0 => None,
        1 => Some(Filter::Nearest),
        _ => Some(Filter::Linear),
    };
    // Native D3D ignores MAXMIPLEVEL when MIPFILTER is NONE, but managed
    // texture residency LOD still selects its most detailed permitted level.
    let first = if state.sampler.mip.is_none() {
        lod
    } else {
        lod.max(f.sampler[&9])
    };
    state.sampler.first_level = first.min(image.as_ref().map_or(1, Image::mip_levels) - 1);
    if image.is_some() && d.shaders.pixel.is_none() {
        state.texture_op = combine(f.stage[&1], f.stage[&2], f.stage[&3]).ok_or(UNAVAILABLE)?;
        state.alpha_texture_op =
            combine(f.stage[&4], f.stage[&5], f.stage[&6]).ok_or(UNAVAILABLE)?;
    }
    Ok((target, image, state))
}

#[win32(d3d9,name="IDirect3DDevice9::DrawPrimitiveUP",level=implement)]
fn draw_up(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    count: u32,
    data: GuestPtr<u8>,
    stride: u32,
) -> Outcome {
    if count == 0 {
        if let Err(error) = empty_draw(env, this.addr(), kind) {
            return Outcome::Return(error);
        }
        super::buffers::clear_up_bindings(env, this.addr(), false);
        return Outcome::Return(0);
    }
    let result = if super::programmable::active(env, this.addr()) {
        super::programmable::prepare(env, this.addr(), kind, count, Some((data, stride)), None, 0)
            .map_err(|code| rejected(code, "programmable draw rejected"))
    } else {
        prepare_checked(env, this.addr(), kind, count, data, stride, None)
    };
    let draw = match result {
        Ok(d) => d,
        Err(error) => {
            warn_draw_rejected(
                env,
                this.addr(),
                kind,
                data,
                stride,
                error.code,
                error.reason,
            );
            return Outcome::Return(error.code);
        }
    };
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    super::buffers::clear_up_bindings(env, this.addr(), false);
    let outcome = dispatch(env, id, Operation::Draw(draw));
    if let Outcome::Return(error) = &outcome
        && *error != 0
    {
        warn_draw_rejected(
            env,
            this.addr(),
            kind,
            data,
            stride,
            *error,
            "GPU command rejected after fixed-function input validation",
        );
    }
    outcome
}

fn warn_draw_rejected(
    env: &mut Env<'_>,
    pointer: u32,
    kind: i32,
    data: GuestPtr<u8>,
    stride: u32,
    code: u32,
    reason: &str,
) {
    let api = "IDirect3DDevice9::DrawPrimitiveUP";
    warn_rejected(env, api, pointer, kind, data, stride, code, reason);
}
fn warn_rejected(
    env: &mut Env<'_>,
    api: &str,
    pointer: u32,
    kind: i32,
    data: GuestPtr<u8>,
    stride: u32,
    code: u32,
    reason: &str,
) {
    let state = device(env, pointer).map_or_else(
        || "device unavailable".to_owned(),
        |d| {
            let v = d.fixed.viewport;
            let declaration = d
                .buffers
                .declaration
                .map_or_else(|| "none".into(), |id| super::declaration::describe(env, id));
            format!(
                "FVF={:#x}, declaration={declaration}, scene={}, viewport=({},{} {}x{},{}..{}), z_enable={}, stencil_enable={}, stencil_attachment={}",
                d.fixed.fvf, d.scene, v.X, v.Y, v.Width, v.Height, v.MinZ, v.MaxZ,
                d.fixed.render[&7], d.fixed.render[&52], super::depth::has_stencil(env, pointer),
            )
        },
    );
    // Do not include primitive count or vertex contents in the warning key:
    // repeated failed batches share one bounded diagnostic and a repeat count.
    adv_abi::warn_unavailable(
        env,
        api,
        &format!(
            "{reason}; HRESULT={code:#x}, primitive={kind}, data={:#x}, stride={stride}; {state}",
            data.addr(),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("fixed/stencil_tests.rs");
    include!("fixed/state_tests.rs");
    include!("fixed/disabled_stage_tests.rs");

    #[test]
    // spec: GFX-INPUT-5
    fn programmable_raster_states_do_not_silently_change_the_fixed_path_contract() {
        let mut m = super::super::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (device, owner, _, _) = super::super::tests::setup(&mut env);
        let this = GuestPtr::new(device);
        let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner)
        else {
            panic!("test device");
        };
        d.scene = true;
        for (state, value, kind) in [
            (28, 1, 5),
            (152, 1, 5),
            (154, 4f32.to_bits(), 1),
            (156, 1, 1),
        ] {
            let original = current(&env, device).unwrap().render[&state];
            assert_eq!(set_render(&mut env, this, state, value), 0);
            // Rejection happens before vertex access or queued GPU work.
            let error = prepare_checked(&env, device, kind, 1, GuestPtr::new(0), 16, None)
                .err()
                .unwrap();
            assert_eq!(error.code, UNAVAILABLE);
            assert!(error.reason.contains("programmable draw path"));
            assert_eq!(set_render(&mut env, this, state, original), 0);
        }
        assert!(env.ext.get::<State>().pending.is_empty());
    }

    #[test]
    fn no_specular_fvf_profile_exposes_bool_state_without_accepting_specular_inputs() {
        let mut m = super::super::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (device, _, _, _) = super::super::tests::setup(&mut env);
        let this = GuestPtr::new(device);
        let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
        assert_eq!(get_render(&mut env, this, 29, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(set_fvf(&mut env, this, 0x44), 0);
        for value in [1, 0, 1] {
            assert_eq!(set_render(&mut env, this, 29, value), 0);
            assert_eq!(get_render(&mut env, this, 29, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), value);
        }
        assert_eq!(set_render(&mut env, this, 29, 2), INVALID);
        assert_eq!(get_render(&mut env, this, 29, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 1);
        assert_eq!(set_fvf(&mut env, this, 0xc4), UNAVAILABLE);
        assert_eq!(get_fvf(&mut env, this, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0x44);
        assert!(env.ext.get::<State>().pending.is_empty());
        assert!(env.ext.get::<State>().transfers.is_empty());
    }

    #[test]
    fn gouraud_default_roundtrip_and_rejected_modes_preserve_state_and_output() {
        let mut m = super::super::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (device, _, _, _) = super::super::tests::setup(&mut env);
        let this = GuestPtr::new(device);
        let page = env.sys.vm.alloc(env.cpu, 8192, Prot::RW).unwrap();
        let output = GuestPtr::<u32>::new(page);
        assert_eq!(get_render(&mut env, this, 9, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 2);
        assert_eq!(set_render(&mut env, this, 9, 2), 0);
        for (value, error) in [
            (1, UNAVAILABLE),
            (3, UNAVAILABLE),
            (0, INVALID),
            (u32::MAX, INVALID),
        ] {
            assert_eq!(set_render(&mut env, this, 9, value), error);
            assert_eq!(get_render(&mut env, this, 9, output), 0);
            assert_eq!(output.read(env.cpu).unwrap(), 2);
        }
        let crossing = GuestPtr::<u32>::new(page + 4094);
        crossing.write(env.cpu, 0x12345678).unwrap();
        env.cpu.protect(page + 4096, 4096, Prot::READ).unwrap();
        assert_eq!(get_render(&mut env, this, 9, crossing), INVALID);
        assert_eq!(crossing.read(env.cpu).unwrap(), 0x12345678);
        assert_eq!(get_render(&mut env, this, 9, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 2);
        assert!(env.ext.get::<State>().pending.is_empty());
        assert!(env.ext.get::<State>().transfers.is_empty());
    }
}
