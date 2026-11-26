//! D3D9 fixed-function calls and guest stream capture.
use super::*;
use crate::fixed::{self as core_fixed, Setting};
pub(in crate::d3d9) use crate::fixed::{State as Fixed, VertexLayout as Layout};
use adv_abi::types::D3DVIEWPORT9;
use adv_device::gpu::{DrawState, Image, Topology, Vertex};
pub(super) mod lighting;
pub(super) mod state_map;
pub(in crate::d3d9) use state_map::{
    apply_render, apply_sampler, apply_scissor, apply_stage, apply_viewport, sampler_value,
    stage_value, transform_slot,
};
use state_map::{to_scissor, to_viewport};

pub(super) type CapturedSpans = Vec<CapturedSpan>;

#[derive(Clone)]
pub(super) struct CapturedSpan {
    pub address: u32,
    pub length: usize,
    bytes: adv_device::gpu::VertexBytes,
    repack: Option<(usize, usize)>,
}

impl CapturedSpan {
    pub fn matches(&self, raw: &[u8]) -> bool {
        if let Some((stride, aligned)) = self.repack {
            raw.chunks(stride)
                .zip(self.bytes.chunks(aligned))
                .all(|(source, packed)| source == &packed[..source.len()])
        } else {
            raw == self.bytes.as_slice()
        }
    }
}

pub(super) struct CachedFixed {
    state: Fixed,
    pub(super) captures: std::cell::RefCell<Option<CapturedSpans>>,
}
impl CachedFixed {
    pub(super) fn new(width: u32, height: u32) -> Self {
        Self {
            state: state_map::defaults(width, height),
            captures: Default::default(),
        }
    }
    pub(super) fn capture_stream(
        &self,
        env: &Env<'_>,
        address: u32,
        length: u32,
        repack: Option<(usize, usize)>,
    ) -> Result<adv_device::gpu::VertexBytes, u32> {
        use std::borrow::Cow;
        let raw = if let Some(bytes) = env.cpu.shared_bytes(address, length as usize) {
            Cow::Borrowed(bytes)
        } else {
            let mut bytes = vec![0; length as usize];
            env.cpu.read(address, &mut bytes).map_err(|_| INVALID)?;
            Cow::Owned(bytes)
        };
        let bytes = if let Some((stride, aligned)) = repack {
            let mut bytes = vec![0; raw.len().div_ceil(stride) * aligned];
            for (source, packed) in raw.chunks(stride).zip(bytes.chunks_mut(aligned)) {
                packed[..source.len()].copy_from_slice(source);
            }
            bytes
        } else {
            raw.into_owned()
        };
        let key = xxhash_rust::xxh3::xxh3_128(&bytes);
        let bytes = adv_device::gpu::VertexBytes::captured(bytes, key);
        if let Some(captures) = self.captures.borrow_mut().as_mut() {
            captures.push(CapturedSpan {
                address,
                length: length as usize,
                bytes: bytes.clone(),
                repack,
            });
        }
        Ok(bytes)
    }
    pub(super) fn auto_depth(&mut self, enabled: bool) {
        self.state.apply(Setting::Render(
            core_fixed::Render::DepthEnable,
            u32::from(enabled),
        ));
    }
    pub(super) fn clear_fvf(&mut self) {
        self.state.apply(Setting::Fvf(0));
    }
    pub(super) fn state_mut(&mut self) -> &mut Fixed {
        &mut self.state
    }
}
impl std::ops::Deref for CachedFixed {
    type Target = Fixed;
    fn deref(&self) -> &Fixed {
        &self.state
    }
}
impl std::ops::DerefMut for CachedFixed {
    fn deref_mut(&mut self) -> &mut Fixed {
        self.state.invalidate_key();
        &mut self.state
    }
}

#[derive(Clone)]
pub(super) struct VertexBounds {
    pub transforms: Option<[[f32; 16]; 3]>,
    pub point_scale: Option<[f32; 3]>,
    pub count: u32,
    pub fixed_pixel: bool,
}
#[derive(Clone)]
pub(super) struct Draw {
    pub target: Target,
    pub image: Option<Image>,
    pub vertices: Arc<Vec<Vertex>>,
    pub texcoords: Option<(usize, Arc<Vec<[f32; 4]>>)>,
    pub state: DrawState,
    pub programmable: Option<Arc<adv_device::gpu::ProgrammableDraw>>,
    pub recording_key: u128,
    pub program_key: u128,
    pub fixed_vertex: Option<VertexBounds>,
}
#[derive(Debug)]
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
pub(super) fn register(registry: &mut Registry) {
    lighting::register(registry);
    for api in [
        super::unchanged::unless_unchanged!(set_fvf_ABI, super::unchanged::binding),
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
        multiply_transform_ABI,
        with_private_releases!(draw_up_ABI),
    ] {
        registry.register(api);
    }
}
pub(super) fn update_fixed(env: &mut Env<'_>, pointer: u32, setting: Result<Setting, u32>) -> u32 {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)
    else {
        return INVALID;
    };
    match setting {
        Ok(setting) => {
            apply_fixed(d, setting, false);
            0
        }
        Err(code) => code,
    }
}

fn apply_fixed(d: &mut Device, setting: Setting, force: bool) {
    if let Some(recording) = &mut d.recording {
        recording.fixed.record(setting);
    } else {
        let mut hot = d.hot.borrow_mut();
        let change = hot.fixed.state_mut().apply(setting);
        if change.changed || force {
            hot.touch(change.slot.class());
        }
    }
}

fn current<'a>(env: &'a Env<'_>, pointer: u32) -> Option<std::cell::Ref<'a, Fixed>> {
    Some(std::cell::Ref::map(
        device(env, pointer)?.hot.borrow(),
        |h| &*h.fixed,
    ))
}
#[win32(d3d9,name="IDirect3DDevice9::SetFVF",level=state)]
pub(super) fn set_fvf(env: &mut Env<'_>, this: GuestPtr<()>, fvf: u32) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    // Any FVF is state; draws reject one without a fixed-function layout.
    // Zero is accepted and leaves the current binding unchanged.
    if fvf == 0 {
        return 0;
    }
    let needs_unbind = {
        let hot = d.hot.borrow();
        if d.recording.is_none() && hot.buffers.declaration.is_none() && hot.fixed.fvf == fvf {
            return 0;
        }
        hot.buffers.declaration.is_some()
    };
    let set_raw = |env: &mut Env<'_>, force: bool| {
        if let Some(Payload::Device(d)) =
            com(env).get_mut::<Payload>(&mut env.sys.objects, this.addr())
        {
            apply_fixed(d, Setting::Fvf(fvf), force);
        }
    };
    set_raw(env, false);
    let device = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    if let Some(code) = super::shaders::stateblocks::record_declaration(env, device, None) {
        if code == 0 {
            set_raw(env, true);
        }
        return code;
    }
    if needs_unbind {
        // Unbinding a declaration clears the FVF it replaced; restore ours.
        let code = super::declaration::bind(env, device, None);
        if code != 0 {
            return code;
        }
        set_raw(env, true);
    }
    0
}
pub(super) fn layout(env: &Env<'_>, pointer: u32) -> Result<Layout, Rejected> {
    let d = device(env, pointer).ok_or_else(|| rejected(INVALID, "invalid device interface"))?;
    let hot = d.hot.borrow();
    match hot.buffers.declaration {
        Some(id) => {
            super::declaration::layout(env, id).map_err(|reason| rejected(UNAVAILABLE, reason))
        }
        None => Layout::from_fvf(hot.fixed.fvf)
            .ok_or_else(|| rejected(INVALID, "no supported FVF is set")),
    }
}
#[win32(d3d9,name="IDirect3DDevice9::GetFVF",level=implement)]
pub(super) fn get_fvf(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    let Some(fvf) = current(env, this.addr()).map(|f| f.fvf) else {
        return INVALID;
    };
    if output.write(env.cpu, fvf).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetRenderState",level=state)]
pub(super) fn set_render(env: &mut Env<'_>, this: GuestPtr<()>, state: i32, value: u32) -> u32 {
    update_fixed(env, this.addr(), state_map::render(state, value))
}
#[win32(d3d9,name="IDirect3DDevice9::GetRenderState",level=implement)]
fn get_render(env: &mut Env<'_>, this: GuestPtr<()>, state: i32, output: GuestPtr<u32>) -> u32 {
    let Some(value) = current(env, this.addr()).and_then(|f| f.render.get(&state).copied()) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetTextureStageState",level=state)]
pub(super) fn set_stage(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    kind: i32,
    value: u32,
) -> u32 {
    update_fixed(env, this.addr(), state_map::stage(stage, kind, value))
}
#[win32(d3d9,name="IDirect3DDevice9::GetTextureStageState",level=implement)]
fn get_stage(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    stage: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    let Some(value) = current(env, this.addr()).and_then(|f| stage_value(&f, stage, kind)) else {
        return INVALID;
    };
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::SetSamplerState",level=state)]
pub(super) fn set_sampler(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    sampler: u32,
    kind: i32,
    value: u32,
) -> u32 {
    update_fixed(env, this.addr(), state_map::sampler(sampler, kind, value))
}
#[win32(d3d9,name="IDirect3DDevice9::GetSamplerState",level=implement)]
fn get_sampler(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    sampler: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> u32 {
    let Some(value) = current(env, this.addr()).and_then(|f| sampler_value(&f, sampler, kind))
    else {
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
    let size = (target.image.width(), target.image.height());
    update_fixed(env, this.addr(), state_map::viewport(v, size))
}
#[win32(d3d9,name="IDirect3DDevice9::GetViewport",level=implement)]
fn get_viewport(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DVIEWPORT9>) -> u32 {
    let Some(v) = current(env, this.addr()).map(|f| to_viewport(f.viewport)) else {
        return INVALID;
    };
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
    update_fixed(env, this.addr(), state_map::scissor(rect))
}
#[win32(d3d9,name="IDirect3DDevice9::GetScissorRect",level=implement)]
fn get_scissor(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<RECT>) -> u32 {
    let Some(v) = current(env, this.addr()).map(|f| to_scissor(f.scissor)) else {
        return INVALID;
    };
    if output.write(env.cpu, v).is_ok() {
        0
    } else {
        INVALID
    }
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
    update_fixed(env, this.addr(), Ok(Setting::Transform(slot, matrix)))
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
    let Some(matrix) = current(env, this.addr()).map(|f| f.transforms[slot as usize]) else {
        return INVALID;
    };
    if output.write(env.cpu, matrix).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9,name="IDirect3DDevice9::MultiplyTransform",level=implement)]
fn multiply_transform(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    kind: i32,
    matrix: GuestPtr<[f32; 16]>,
) -> u32 {
    let Some(slot) = transform_slot(kind) else {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::MultiplyTransform",
            "unsupported transform slot",
        );
        return UNAVAILABLE;
    };
    let Ok(matrix) = matrix.read(env.cpu) else {
        return INVALID;
    };
    if matrix.iter().any(|v| !v.is_finite()) {
        return INVALID;
    }
    update_fixed(
        env,
        this.addr(),
        Ok(Setting::MultiplyTransform(slot, matrix)),
    )
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
    programmable::prepare(env, pointer, kind, count, None, indices, start)
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
    if !d.hot.borrow().scene || d.needs_reset || !matches!(kind, 1..=6) {
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
    programmable::prepare(env, pointer, kind, count, Some((data, stride)), indices, 0)
}
pub(super) fn prepare_lines(
    draw: &mut Draw,
    streams: &[adv_device::gpu::VertexStream],
    indices: Option<&adv_device::gpu::IndexBytes>,
    count: u32,
) -> Result<(), u32> {
    let bounds = draw.fixed_vertex.take().unwrap();
    let mut vertices = Vec::with_capacity(count as usize);
    let has_color = streams
        .iter()
        .any(|s| s.attributes.iter().any(|a| a.location == 1));
    for ordinal in 0..count {
        let n = indices.map_or(ordinal, |i| i[ordinal as usize]);
        let v = recording::damage::stream_input(streams, n)
            .ok_or(INVALID)?
            .v;
        let mut position = v[0];
        let viewport = draw.state.viewport;
        if let Some(transforms) = &bounds.transforms {
            for matrix in transforms {
                position = core_fixed::tnl::row(position, matrix);
            }
            position[0] += position[3] / viewport.width;
            position[1] -= position[3] / viewport.height;
        } else {
            position = core_fixed::tnl::pretransformed(position, viewport).ok_or(INVALID)?;
        }
        vertices.push(Vertex {
            position,
            color: if has_color { v[1] } else { [1.; 4] },
            uv: [v[2][0], v[2][1]],
        });
    }
    draw.vertices = vertices.into();
    Ok(())
}

pub(super) fn prepare_state(
    env: &Env<'_>,
    pointer: u32,
    topology: Topology,
    vertex: Option<core_fixed::VertexProgram>,
) -> Result<(Target, Option<Image>, core_fixed::Lowered), Rejected> {
    let d = device(env, pointer).ok_or(INVALID)?;
    let hot = d.hot.borrow();
    let f = &hot.fixed;
    let provider = env.ext.get::<State>().provider;
    let target = super::targets::draw_target(env, pointer)
        .map_err(|code| rejected(code, "render target is missing, locked or unavailable"))?;
    let target = if f.render[&7] != 0 || f.render[&52] != 0 {
        super::depth::attach(env, pointer, target)
            .map_err(|code| rejected(code, "depth attachment is invalid or incompatible"))?
    } else {
        target
    };
    let (image, lod) = if hot.shaders.pixel.is_some() {
        (None, 0)
    } else if let Some(id) = hot.texture {
        let Some(Payload::Resource(r)) = com(env).get_identity(&env.sys.objects, id) else {
            return Err(rejected(
                INVALID,
                "bound texture no longer has a resource owner",
            ));
        };
        if resources::sampling_blocked(env, r) {
            return Err(rejected(
                INVALID,
                "bound texture has a DC lease or transfer",
            ));
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
    let layout = core_fixed::Layout {
        topology,
        vertex,
        extent: (target.image.width(), target.image.height()),
        depth: target.depth().is_some(),
        stencil: target.depth().is_some()
            && f.render[&core_fixed::Render::StencilEnable] != 0
            && super::depth::has_stencil(env, pointer),
        texture: image.as_ref().map(|image| (image.mip_levels(), lod)),
        stage_zero_bound: hot.texture.is_some(),
    };
    let lowered = core_fixed::lower(f, &layout, state_map::RULES)?;
    Ok((target, image, lowered))
}

pub(super) fn topology(kind: i32) -> Result<Topology, u32> {
    Ok(match kind {
        1 => Topology::Points,
        2 => Topology::Lines,
        3 => Topology::LineStrip,
        4 | 6 => Topology::Triangles,
        5 => Topology::TriangleStrip,
        _ => return Err(INVALID),
    })
}

impl From<core_fixed::LowerError> for Rejected {
    fn from(error: core_fixed::LowerError) -> Self {
        use core_fixed::LowerError::*;
        match error {
            InvalidValue => INVALID.into(),
            EmptyScissor => rejected(INVALID, "scissor rectangle is empty outside render target"),
            TextureOperation => UNAVAILABLE.into(),
            TextureArgument => rejected(
                UNAVAILABLE,
                "texture stage 1 argument source is unavailable",
            ),
            TooManyLights => rejected(
                UNAVAILABLE,
                "more than MaxActiveLights=8 lights are enabled",
            ),
        }
    }
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
    let outcome = recording::up(env, this.addr(), kind, count, data.addr(), stride, |env| {
        prepare(env, this.addr(), kind, count, data, stride, None)
    });
    if let Outcome::Return(error) = &outcome
        && *error != 0
    {
        let reason = if let Some(device) = device(env, this.addr()) {
            if !device.hot.borrow().scene {
                "draw outside BeginScene/EndScene"
            } else if *error == UNAVAILABLE
                && layout(env, this.addr()).is_ok_and(|l| {
                    l.streams[0] != 0
                        || (l.color.is_some() && l.streams[1] != 0)
                        || l.normal.is_some_and(|(stream, _)| stream != 0)
                        || l.specular.is_some_and(|(stream, _)| stream != 0)
                        || l.texcoords
                            .iter()
                            .flatten()
                            .any(|(stream, _, _)| *stream != 0)
                })
            {
                "UP fixed-function declarations require all inputs on stream 0"
            } else {
                "draw input or state rejected"
            }
        } else {
            "invalid device interface"
        };
        warn_draw_rejected(env, this.addr(), kind, data, stride, *error, reason);
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
            let hot = d.hot.borrow();
            let v = to_viewport(hot.fixed.viewport);
            let declaration = hot
                .buffers
                .declaration
                .map_or_else(|| "none".into(), |id| super::declaration::describe(env, id));
            format!(
                "FVF={:#x}, declaration={declaration}, scene={}, viewport=({},{} {}x{},{}..{}), z_enable={}, stencil_enable={}, stencil_attachment={}",
                hot.fixed.fvf, hot.scene, v.X, v.Y, v.Width, v.Height, v.MinZ, v.MaxZ,
                hot.fixed.render[&7], hot.fixed.render[&52], super::depth::has_stencil(env, pointer),
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

pub(super) fn apply_transform(
    f: &mut CachedFixed,
    slot: core_fixed::Transform,
    matrix: [f32; 16],
) -> bool {
    f.state_mut()
        .apply(Setting::Transform(slot, matrix))
        .changed
}

pub(super) fn multiply_transform_cached(
    f: &mut CachedFixed,
    slot: core_fixed::Transform,
    matrix: [f32; 16],
) -> bool {
    f.state_mut()
        .apply(Setting::MultiplyTransform(slot, matrix))
        .changed
}

pub(super) fn second_stage(f: &Fixed, textures: [bool; 2]) -> Result<Option<[u32; 4]>, Rejected> {
    core_fixed::second_stage(f, textures, state_map::RULES).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("fixed/stencil_tests.rs");
    include!("fixed/state_tests.rs");
    include!("fixed/disabled_stage_tests.rs");

    #[test]
    // spec: GFX-INPUT-5
    fn fixed_completion_user_clip_planes_stay_explicit() {
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
        d.hot.borrow_mut().scene = true;
        let original = current(&env, device).unwrap().render[&152];
        assert_eq!(set_render(&mut env, this, 152, 1), 0);
        // Rejection happens before vertex access or queued GPU work.
        assert_eq!(
            prepare(&mut env, device, 5, 1, GuestPtr::new(0), 16, None).err(),
            Some(UNAVAILABLE)
        );
        assert_eq!(set_render(&mut env, this, 152, original), 0);
        assert!(env.ext.get::<State>().pending.is_empty());
    }

    #[test]
    // spec: GFX-FIXED-1
    fn fixed_completion_specular_fvf_and_bool_roundtrip() {
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
        assert_eq!(set_fvf(&mut env, this, 0xc4), 0);
        assert_eq!(get_fvf(&mut env, this, output), 0);
        assert_eq!(output.read(env.cpu).unwrap(), 0xc4);
        assert!(env.ext.get::<State>().pending.is_empty());
        assert!(env.ext.get::<State>().transfers.is_empty());
    }

    #[test]
    // spec: GFX-FIXED-2
    fn fixed_completion_shading_roundtrip_and_invalid_modes() {
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
        for (value, error) in [(3, UNAVAILABLE), (0, INVALID), (u32::MAX, INVALID)] {
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
