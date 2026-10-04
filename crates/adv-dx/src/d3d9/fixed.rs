//! Translate documented D3D9 fixed-function state into neutral drawing commands.
use super::*;
use adv_abi::types::D3DVIEWPORT9;
use adv_device::gpu::{
    Address, Blend, BlendOp, Compare, Cull, DrawState, Factor, Filter, Image, Stencil, StencilFace,
    StencilOp, TextureOp, Topology, Vertex, Viewport,
};
use adv_device::surface::Rect;

pub(super) struct Fixed {
    fvf: u32,
    render: BTreeMap<i32, u32>,
    stage: BTreeMap<i32, u32>,
    sampler: BTreeMap<i32, u32>,
    viewport: D3DVIEWPORT9,
    scissor: RECT,
    transforms: [[f32; 16]; 3],
}
pub(super) struct Draw {
    pub target: Target,
    pub image: Option<Image>,
    pub vertices: Vec<Vertex>,
    pub state: DrawState,
}
impl Fixed {
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
        let identity = [
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
        ];
        Self {
            fvf: 0,
            render: BTreeMap::from([
                (7, 0),
                (9, 2), // SHADEMODE: the shared shader interpolates vertex colors.
                (14, 1),
                (15, 0),
                (19, 2),
                (20, 1),
                (22, 3),
                (23, 4),
                (24, 0),
                (25, 8),
                (27, 0),
                (29, 0), // SPECULARENABLE: supported FVFs supply zero specular RGB.
                (52, 0),
                (53, 1),
                (54, 1),
                (55, 1),
                (56, 8),
                (57, 0),
                (58, u32::MAX),
                (59, u32::MAX),
                (137, 1),
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
            stage: BTreeMap::from([(1, 4), (2, 2), (3, 1), (4, 2), (5, 2), (6, 1)]),
            sampler: BTreeMap::from([(1, 1), (2, 1), (5, 1), (6, 1), (7, 0), (9, 0)]),
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
        draw_up_ABI,
    ] {
        registry.register(api);
    }
}
fn mutable<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Fixed> {
    match com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)? {
        Payload::Device(d) => Some(&mut d.fixed),
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
    matches!(fvf & 0x0e, 2 | 4) && fvf & !(0x0e | 0x40 | 0x100) == 0
}
#[win32(d3d9,name="IDirect3DDevice9::SetFVF",level=state)]
fn set_fvf(env: &mut Env<'_>, this: GuestPtr<()>, fvf: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if !valid_fvf(fvf) {
        return UNAVAILABLE;
    }
    f.fvf = fvf;
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetFVF",level=implement)]
fn get_fvf(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
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
#[win32(d3d9,name="IDirect3DDevice9::SetRenderState",level=state)]
fn set_render(env: &mut Env<'_>, this: GuestPtr<()>, state: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    let valid = match state {
        9 => match value {
            2 => true, // GOURAUD is the existing neutral renderer's shading mode.
            1 | 3 => return UNAVAILABLE,
            _ => false,
        },
        7 | 14 | 15 | 27 | 29 | 52 | 137 | 174 | 185 | 206 => value <= 1,
        19 | 20 | 207 | 208 => factor(value).is_some(),
        22 => (1..=3).contains(&value),
        23 | 25 | 56 | 189 => compare(value).is_some(),
        53..=55 | 186..=188 => stencil_op(value).is_some(),
        24 | 57..=59 => true,
        168 => value <= 15,
        171 | 209 => operation(value).is_some(),
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    f.render.insert(state, value);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetRenderState",level=implement)]
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
#[win32(d3d9,name="IDirect3DDevice9::SetTextureStageState",level=state)]
fn set_stage(env: &mut Env<'_>, this: GuestPtr<()>, stage: u32, kind: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if stage == 1 && kind == 1 && value == 1 {
        return 0;
    }
    if stage != 0 {
        return UNAVAILABLE;
    }
    let valid = match kind {
        1 | 4 => matches!(value, 1 | 2 | 3 | 4 | 7 | 10),
        2 | 3 | 5 | 6 => value <= 2,
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    f.stage.insert(kind, value);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetTextureStageState",level=implement)]
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
    let value = if stage == 1 && kind == 1 {
        1
    } else if stage == 0 {
        match f.stage.get(&kind) {
            Some(&v) => v,
            None => return INVALID,
        }
    } else {
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
        _ => return None,
    })
}
#[win32(d3d9,name="IDirect3DDevice9::SetSamplerState",level=state)]
fn set_sampler(env: &mut Env<'_>, this: GuestPtr<()>, sampler: u32, kind: i32, value: u32) -> u32 {
    let Some(f) = mutable(env, this.addr()) else {
        return INVALID;
    };
    if sampler != 0 {
        return UNAVAILABLE;
    }
    let valid = match kind {
        1 | 2 => address(value).is_some(),
        5 | 6 => matches!(value, 1 | 2),
        7 => value <= 2,
        9 => true,
        _ => return UNAVAILABLE,
    };
    if !valid {
        return INVALID;
    }
    f.sampler.insert(kind, value);
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetSamplerState",level=implement)]
fn get_sampler(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    sampler: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    if sampler != 0 {
        return INVALID;
    }
    let Some(&value) = current(env, this.addr()).and_then(|f| f.sampler.get(&kind)) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetViewport",level=state)]
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
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetViewport",level=implement)]
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
#[win32(d3d9,name="IDirect3DDevice9::SetScissorRect",level=state)]
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
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetScissorRect",level=implement)]
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
#[win32(d3d9,name="IDirect3DDevice9::SetTransform",level=state)]
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
    0
}
#[win32(d3d9,name="IDirect3DDevice9::GetTransform",level=implement)]
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
pub(super) fn vertex_size(env: &Env<'_>, pointer: u32) -> Result<u32, u32> {
    let f = current(env, pointer).ok_or(INVALID)?;
    if !valid_fvf(f.fvf) {
        return Err(INVALID);
    }
    let position = if f.fvf & 0x0e == 4 { 4 } else { 3 };
    Ok((position + u32::from(f.fvf & 0x40 != 0) + 2 * u32::from(f.fvf & 0x100 != 0)) * 4)
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
    env: &Env<'_>,
    pointer: u32,
    kind: i32,
    count: u32,
    data: GuestPtr<u8>,
    stride: u32,
    indices: Option<&[u32]>,
) -> Result<Draw, u32> {
    let d = device(env, pointer).ok_or(INVALID)?;
    let f = &d.fixed;
    if !d.scene || !valid_fvf(f.fvf) {
        return Err(INVALID);
    }
    let position = if f.fvf & 0x0e == 4 { 4 } else { 3 };
    if position == 3 && f.render[&137] != 0 {
        return Err(UNAVAILABLE);
    }
    let minimum = vertex_size(env, pointer)?;
    if stride < minimum || stride > 2048 || data.is_null() {
        return Err(INVALID);
    }
    let topology = match kind {
        1 => Topology::Points,
        2 => Topology::Lines,
        3 => Topology::LineStrip,
        4 | 6 => Topology::Triangles,
        5 => Topology::TriangleStrip,
        _ => return Err(INVALID),
    };
    let length = primitive_length(kind, count)?;
    if indices.is_some_and(|v| v.len() != length as usize) {
        return Err(INVALID);
    }
    let first = indices.and_then(|v| v.iter().min().copied()).unwrap_or(0);
    let last = indices
        .and_then(|v| v.iter().max().copied())
        .unwrap_or(length - 1);
    let offset = first.checked_mul(stride).ok_or(INVALID)?;
    let bytes = (last - first)
        .checked_mul(stride)
        .and_then(|n| n.checked_add(minimum))
        .ok_or(INVALID)?;
    let span = data.addr().checked_add(offset).ok_or(INVALID)?;
    adv_abi::guest::check_range(env.cpu, span, bytes, Prot::READ).map_err(|_| INVALID)?;
    let mut vertices = Vec::with_capacity(length as usize);
    for ordinal in 0..length {
        let index = indices.map_or(ordinal, |v| v[ordinal as usize]);
        let mut raw = [0; 28];
        env.cpu
            .read(data.addr() + index * stride, &mut raw[..minimum as usize])
            .map_err(|_| INVALID)?;
        let float =
            |word: usize| f32::from_le_bytes(raw[word * 4..word * 4 + 4].try_into().unwrap());
        let mut p = [float(0), float(1), float(2), 1.];
        if position == 4 {
            let rhw = float(3);
            if rhw == 0. || !rhw.is_finite() {
                return Err(INVALID);
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
        }
        if p.iter().any(|v| !v.is_finite()) {
            return Err(INVALID);
        }
        let mut at = position as usize;
        let mut color = [1.; 4];
        if f.fvf & 0x40 != 0 {
            let argb = u32::from_le_bytes(raw[at * 4..at * 4 + 4].try_into().unwrap());
            color = [
                ((argb >> 16) & 255) as f32 / 255.,
                ((argb >> 8) & 255) as f32 / 255.,
                (argb & 255) as f32 / 255.,
                (argb >> 24) as f32 / 255.,
            ];
            at += 1;
        }
        let uv = if f.fvf & 0x100 != 0 {
            [float(at), float(at + 1)]
        } else {
            [0.; 2]
        };
        if uv.iter().any(|v| !v.is_finite()) {
            return Err(INVALID);
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
    }
    let provider = env.ext.get::<State>().provider;
    let target = super::targets::draw_target(env, pointer)?;
    let target = if f.render[&7] != 0 || f.render[&52] != 0 {
        super::depth::attach(env, pointer, target)?
    } else {
        target
    };
    let (image, lod) = if let Some(id) = d.texture {
        let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
            return Err(INVALID);
        };
        if resources::locked(env, r) {
            return Err(INVALID);
        }
        (
            Some(
                (provider.target)(env.ext, r.surface.ok_or(INVALID)?)
                    .ok_or(INVALID)?
                    .image,
            ),
            r.lod,
        )
    } else {
        (None, 0)
    };
    let mut state = DrawState::new(target.image.width(), target.image.height());
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
        if f.render[&52] != 0 {
            if !super::depth::has_stencil(env, pointer) {
                return Err(INVALID);
            }
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
            return Err(INVALID);
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
    if image.is_some() {
        state.texture_op = combine(f.stage[&1], f.stage[&2], f.stage[&3]).ok_or(UNAVAILABLE)?;
        state.alpha_texture_op =
            combine(f.stage[&4], f.stage[&5], f.stage[&6]).ok_or(UNAVAILABLE)?;
    }
    Ok(Draw {
        target,
        image,
        vertices,
        state,
    })
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
    let draw = match prepare(env, this.addr(), kind, count, data, stride, None) {
        Ok(d) => d,
        Err(e) => return Outcome::Return(e),
    };
    let id = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    super::buffers::clear_up_bindings(env, this.addr(), false);
    dispatch(env, id, Operation::Draw(draw))
}

#[cfg(test)]
mod tests {
    use super::*;

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
