//! Direct bodies for hot D3D9 COM state setters, getters, and unchanged state
//! blocks (A64-CALL-2). Bodies read and mutate shared `HotDevice` and
//! `HotStateBlock` storage via `Services::d3d9_state`, and return `Stop::Guest`
//! or `Stop::Slow` before any mutation whenever the full ABI path is required.
use super::shaders::ConstantKind;
use super::*;
use crate::shader::Stage;
use adv_abi::types::D3DVIEWPORT9;
use adv_cpu::body;
use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use rustc_hash::FxHashMap;
use std::cell::{RefCell, RefMut};
use std::rc::Rc;

#[derive(Default)]
pub(super) struct SharedD3D9 {
    devices: Vec<(u32, Rc<RefCell<HotDevice>>)>,
    state_blocks: FxHashMap<u32, Rc<RefCell<shaders::stateblocks::HotStateBlock>>>,
}

impl SharedD3D9 {
    pub(super) fn register_device(&mut self, pointer: u32, device: Rc<RefCell<HotDevice>>) {
        if let Some(entry) = self.devices.iter_mut().find(|(p, _)| *p == pointer) {
            entry.1 = device;
        } else {
            self.devices.push((pointer, device));
        }
    }
    pub(super) fn remove_device(&mut self, pointer: u32) {
        if let Some(pos) = self.devices.iter().position(|(p, _)| *p == pointer) {
            self.devices.swap_remove(pos);
        }
    }
    pub(super) fn register_state_block(
        &mut self,
        pointer: u32,
        block: Rc<RefCell<shaders::stateblocks::HotStateBlock>>,
    ) {
        self.state_blocks.insert(pointer, block);
    }
    pub(super) fn remove_state_block(&mut self, pointer: u32) {
        self.state_blocks.remove(&pointer);
    }
    fn device(&self, pointer: u32) -> Option<&Rc<RefCell<HotDevice>>> {
        self.devices
            .iter()
            .find(|(p, _)| *p == pointer)
            .map(|(_, d)| d)
    }
    fn state_block(
        &self,
        pointer: u32,
    ) -> Option<&Rc<RefCell<shaders::stateblocks::HotStateBlock>>> {
        self.state_blocks.get(&pointer)
    }
}

pub(super) struct HotDevice {
    pub(super) pointer: u32,
    pub(super) scene: bool,
    pub(super) fixed: fixed::CachedFixed,
    pub(super) shaders: shaders::Bindings,
    pub(super) buffers: buffers::Bindings,
    pub(super) texture: Option<Id>,
    pub(super) versions: [u64; 15],
    pub(super) target_version: u64,
    pub(super) fingerprints: [u128; 15],
    pub(super) draw_key: std::cell::Cell<Option<(recording::DrawStateIdentity, u128)>>,
    pub(super) target_size: Option<(u32, u32)>,
    pub(super) recording: bool,
    pub(super) stream_ptrs: [u32; 16],
    pub(super) stream_lens: [u32; 16],
    pub(super) indices_ptr: u32,
    pub(super) declaration_ptr: u32,
    pub(super) vertex_shader_ptr: u32,
    pub(super) pixel_shader_ptr: u32,
    pub(super) texture_ptrs: [u32; 20],
    pub(super) texture_clean: u32,
    /// Unavailable options already reported by the full handler.
    pub(super) reported: rustc_hash::FxHashSet<[u32; 4]>,
}

impl HotDevice {
    pub(super) fn new(fixed: fixed::CachedFixed) -> Self {
        let mut hot = Self {
            pointer: 0,
            scene: false,
            fixed,
            shaders: shaders::Bindings::default(),
            buffers: buffers::Bindings::default(),
            texture: None,
            versions: [0; 15],
            target_version: 0,
            fingerprints: [0; 15],
            draw_key: Default::default(),
            target_size: None,
            recording: false,
            stream_ptrs: [0; 16],
            stream_lens: [0; 16],
            indices_ptr: 0,
            declaration_ptr: 0,
            vertex_shader_ptr: 0,
            pixel_shader_ptr: 0,
            texture_ptrs: [0; 20],
            texture_clean: 0,
            reported: Default::default(),
        };
        hot.refresh_all();
        hot
    }
    pub(super) fn class_key(&self, class: usize) -> u128 {
        use recording::hash;
        let f = &self.fixed;
        match class {
            0..=6 => f.class_key(class),
            7 => hash((f.fvf, self.buffers.declaration)),
            8 => hash(self.shaders.vertex),
            9 => hash(self.shaders.pixel),
            10 => hash((
                self.buffers.streams.map(|s| (s.buffer, s.offset, s.stride)),
                self.buffers.frequencies,
            )),
            11 => hash(self.buffers.indices),
            12 => hash((self.texture, self.shaders.textures)),
            13 => self.shaders.vertex_constants.fingerprint(),
            14 => self.shaders.pixel_constants.fingerprint(),
            _ => unreachable!(),
        }
    }
    pub(super) fn touch(&mut self, class: usize) {
        self.versions[class] += 1;
        self.fingerprints[class] = self.class_key(class);
    }
    pub(super) fn refresh_all(&mut self) {
        self.fingerprints = std::array::from_fn(|class| self.class_key(class));
    }
    pub(super) fn refresh(&mut self, dirty: u32) {
        for class in 0..self.fingerprints.len() {
            if dirty & (1 << class) != 0 {
                self.fingerprints[class] = self.class_key(class);
            }
        }
    }
    pub(super) fn clear_texture_clean(&mut self, pointer: u32) {
        for (slot, &bound) in self.texture_ptrs.iter().enumerate() {
            if bound == pointer {
                self.texture_clean &= !(1 << slot);
            }
        }
    }
    pub(super) fn set_texture_slot(&mut self, slot: usize, pointer: u32, clean: bool) {
        self.texture_ptrs[slot] = pointer;
        if pointer != 0 && clean {
            self.texture_clean |= 1 << slot;
        } else {
            self.texture_clean &= !(1 << slot);
        }
    }
}

pub(super) fn body_services(
    _sys: &adv_machine::Sys,
    ext: &adv_machine::Ext,
    services: &mut adv_cpu::body::Services,
) {
    if let Some(state) = ext.try_get::<State>() {
        services.d3d9_state = Rc::as_ptr(&state.shared).cast();
    }
}

fn shared<M: Mem>(mem: &M) -> Result<&RefCell<SharedD3D9>, Stop> {
    let ptr = mem.services().d3d9_state.cast::<RefCell<SharedD3D9>>();
    if ptr.is_null() {
        return Err(Stop::Guest);
    }
    // SAFETY: `d3d9::install` hooks `Services::d3d9_state` to `State::shared`,
    // whose `Rc` outlives every machine execution turn.
    Ok(unsafe { &*ptr })
}

fn with_device<M: Mem, R>(
    mem: &M,
    pointer: u32,
    f: impl FnOnce(&HotDevice) -> Result<R, Stop>,
) -> Result<R, Stop> {
    let shared = shared(mem)?.borrow();
    let cell = shared.device(pointer).ok_or(Stop::Guest)?;
    let hot = cell.borrow();
    f(&hot)
}

fn with_device_mut<M: Mem, R>(
    mem: &M,
    pointer: u32,
    f: impl FnOnce(&mut HotDevice) -> Result<(R, bool), Stop>,
) -> Result<R, Stop> {
    let shared = shared(mem)?.borrow();
    let cell = shared.device(pointer).ok_or(Stop::Guest)?;
    let (result, worked) = {
        let mut hot: RefMut<'_, HotDevice> = cell.borrow_mut();
        f(&mut hot)?
    };
    if worked {
        // SAFETY: `mem.services()` carries live machine-owned pointers.
        unsafe { adv_machine::service::work(mem.services()) };
    }
    Ok(result)
}

fn mark_work<M: Mem>(mem: &M) {
    // SAFETY: `mem.services()` carries live machine-owned pointers.
    unsafe { adv_machine::service::work(mem.services()) };
}

fn write_u32<M: Mem>(mem: &mut M, at: u32, value: u32) -> Result<Ret, Stop> {
    let out = mem.output(at, 4).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.output(at, 4)` checked and granted 4 writable bytes.
    unsafe { out.cast::<[u8; 4]>().write_unaligned(value.to_le_bytes()) };
    mem.commit()?;
    Ok(0u32.into())
}

fn read_f32s<M: Mem, const N: usize>(mem: &mut M, at: u32) -> Result<[f32; N], Stop> {
    let ptr = mem.input(at, (N * 4) as u32).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.input` checked `N * 4` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, N * 4) };
    Ok(std::array::from_fn(|i| {
        f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
    }))
}

fn write_f32s<M: Mem, const N: usize>(
    mem: &mut M,
    at: u32,
    values: &[f32; N],
) -> Result<Ret, Stop> {
    let out = mem.output(at, (N * 4) as u32).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.output` checked `N * 4` writable bytes.
    let dst = unsafe { std::slice::from_raw_parts_mut(out, N * 4) };
    for (i, &v) in values.iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    mem.commit()?;
    Ok(0u32.into())
}

body!(pub(super) SET_RENDER = set_render, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn set_render<M: Mem>(mem: &mut M, [this, state, value, ..]: [u32; 7]) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        match fixed::apply_render(&mut hot.fixed, state as i32, value) {
            Ok(changed) => {
                if changed {
                    hot.touch(0);
                }
                Ok((0u32.into(), changed))
            }
            Err(code) => reject(hot, [0, 0, state, value], code),
        }
    })
}

/// The first unavailable rejection of each option takes the full handler,
/// which reports it; repeats return the code here.
fn reject(hot: &mut HotDevice, option: [u32; 4], code: u32) -> Result<(Ret, bool), Stop> {
    if code == UNAVAILABLE && hot.reported.insert(option) {
        return Err(Stop::Guest);
    }
    Ok((code.into(), false))
}

body!(pub(super) GET_RENDER = get_render, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn get_render<M: Mem>(mem: &mut M, [this, state, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let value = with_device(mem, this, |hot| {
        Ok(hot.fixed.render.get(&(state as i32)).copied())
    })?;
    match value {
        Some(value) => write_u32(mem, output, value),
        None => Ok(INVALID.into()),
    }
}

body!(pub(super) SET_STAGE = set_stage, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
fn set_stage<M: Mem>(mem: &mut M, [this, stage, kind, value, ..]: [u32; 7]) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        match fixed::apply_stage(&mut hot.fixed, stage, kind as i32, value) {
            Ok(changed) => {
                if changed {
                    hot.touch(1);
                }
                Ok((0u32.into(), changed))
            }
            Err(code) => reject(hot, [1, stage, kind, value], code),
        }
    })
}

body!(pub(super) GET_STAGE = get_stage, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
fn get_stage<M: Mem>(mem: &mut M, [this, stage, kind, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let value = with_device(mem, this, |hot| {
        Ok(fixed::stage_value(&hot.fixed, stage, kind as i32))
    })?;
    match value {
        Some(value) => write_u32(mem, output, value),
        None => Ok(INVALID.into()),
    }
}

body!(pub(super) SET_SAMPLER = set_sampler, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
fn set_sampler<M: Mem>(
    mem: &mut M,
    [this, sampler, kind, value, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        match fixed::apply_sampler(&mut hot.fixed, sampler, kind as i32, value) {
            Ok(changed) => {
                if changed {
                    hot.touch(2);
                }
                Ok((0u32.into(), changed))
            }
            Err(code) => reject(hot, [2, sampler, kind, value], code),
        }
    })
}

body!(pub(super) GET_SAMPLER = get_sampler, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
fn get_sampler<M: Mem>(
    mem: &mut M,
    [this, sampler, kind, output, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let value = with_device(mem, this, |hot| {
        Ok(fixed::sampler_value(&hot.fixed, sampler, kind as i32))
    })?;
    match value {
        Some(value) => write_u32(mem, output, value),
        None => Ok(INVALID.into()),
    }
}

body!(pub(super) SET_FVF = set_fvf, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_fvf<M: Mem>(mem: &mut M, [this, fvf, ..]: [u32; 7]) -> Result<Ret, Stop> {
    if fvf == 0 {
        return with_device(mem, this, |_| Ok(0u32.into()));
    }
    with_device_mut(mem, this, |hot| {
        if hot.recording || hot.buffers.declaration.is_some() {
            return Err(Stop::Guest);
        }
        if hot.fixed.fvf == fvf {
            return Ok((0u32.into(), false));
        }
        hot.fixed.state_mut().apply(crate::fixed::Setting::Fvf(fvf));
        hot.touch(7);
        Ok((0u32.into(), true))
    })
}

body!(pub(super) GET_FVF = get_fvf, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn get_fvf<M: Mem>(mem: &mut M, [this, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let fvf = with_device(mem, this, |hot| Ok(hot.fixed.fvf))?;
    write_u32(mem, output, fvf)
}

body!(pub(super) SET_VIEWPORT = set_viewport, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_viewport<M: Mem>(mem: &mut M, [this, ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let bytes = mem.read::<24>(ptr).map_err(|_| Stop::Slow)?;
    let word = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    let viewport = D3DVIEWPORT9 {
        X: word(0),
        Y: word(1),
        Width: word(2),
        Height: word(3),
        MinZ: f32::from_bits(word(4)),
        MaxZ: f32::from_bits(word(5)),
    };
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        let target = hot.target_size.ok_or(Stop::Guest)?;
        match fixed::apply_viewport(&mut hot.fixed, viewport, target) {
            Ok(changed) => {
                if changed {
                    hot.touch(3);
                }
                Ok((0u32.into(), changed))
            }
            Err(code) => Ok((code.into(), false)),
        }
    })
}

body!(pub(super) GET_VIEWPORT = get_viewport, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn get_viewport<M: Mem>(mem: &mut M, [this, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let v = with_device(mem, this, |hot| {
        Ok(fixed::state_map::to_viewport(hot.fixed.viewport))
    })?;
    let out = mem.output(output, 24).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.output(output, 24)` checked 24 writable bytes.
    let dst = unsafe { std::slice::from_raw_parts_mut(out, 24) };
    let words = [
        v.X,
        v.Y,
        v.Width,
        v.Height,
        v.MinZ.to_bits(),
        v.MaxZ.to_bits(),
    ];
    for (i, w) in words.iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    mem.commit()?;
    Ok(0u32.into())
}

body!(pub(super) SET_SCISSOR = set_scissor, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_scissor<M: Mem>(mem: &mut M, [this, ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let bytes = mem.read::<16>(ptr).map_err(|_| Stop::Slow)?;
    let word = |i: usize| i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    let rect = RECT {
        left: word(0),
        top: word(1),
        right: word(2),
        bottom: word(3),
    };
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        match fixed::apply_scissor(&mut hot.fixed, rect) {
            Ok(changed) => {
                if changed {
                    hot.touch(4);
                }
                Ok((0u32.into(), changed))
            }
            Err(code) => Ok((code.into(), false)),
        }
    })
}

body!(pub(super) GET_SCISSOR = get_scissor, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn get_scissor<M: Mem>(mem: &mut M, [this, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let r = with_device(mem, this, |hot| {
        Ok(fixed::state_map::to_scissor(hot.fixed.scissor))
    })?;
    let out = mem.output(output, 16).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.output(output, 16)` checked 16 writable bytes.
    let dst = unsafe { std::slice::from_raw_parts_mut(out, 16) };
    for (i, w) in [r.left, r.top, r.right, r.bottom].iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    mem.commit()?;
    Ok(0u32.into())
}

body!(pub(super) SET_TRANSFORM = set_transform, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn set_transform<M: Mem>(mem: &mut M, [this, kind, matrix_ptr, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let Some(slot) = fixed::transform_slot(kind as i32) else {
        return with_device_mut(mem, this, |hot| reject(hot, [3, 0, kind, 0], UNAVAILABLE));
    };
    let matrix = read_f32s::<M, 16>(mem, matrix_ptr)?;
    if matrix.iter().any(|v| !v.is_finite()) {
        return Ok(INVALID.into());
    }
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        let changed = fixed::apply_transform(&mut hot.fixed, slot, matrix);
        if changed {
            hot.touch(5);
        }
        Ok((0u32.into(), changed))
    })
}

body!(pub(super) GET_TRANSFORM = get_transform, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn get_transform<M: Mem>(mem: &mut M, [this, kind, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let Some(slot) = fixed::transform_slot(kind as i32) else {
        return Ok(INVALID.into());
    };
    let matrix = with_device(mem, this, |hot| Ok(hot.fixed.transforms[slot as usize]))?;
    write_f32s(mem, output, &matrix)
}

body!(pub(super) MULTIPLY_TRANSFORM = multiply_transform, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn multiply_transform<M: Mem>(
    mem: &mut M,
    [this, kind, matrix_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let Some(slot) = fixed::transform_slot(kind as i32) else {
        return with_device_mut(mem, this, |hot| reject(hot, [4, 0, kind, 0], UNAVAILABLE));
    };
    let matrix = read_f32s::<M, 16>(mem, matrix_ptr)?;
    if matrix.iter().any(|v| !v.is_finite()) {
        return Ok(INVALID.into());
    }
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        let changed = fixed::multiply_transform_cached(&mut hot.fixed, slot, matrix);
        if changed {
            hot.touch(5);
        }
        Ok((0u32.into(), changed))
    })
}

body!(pub(super) SET_CLIP_PLANE = set_clip_plane, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn set_clip_plane<M: Mem>(
    mem: &mut M,
    [this, index, plane_ptr, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    if index >= 6 {
        return with_device(mem, this, |_| Ok(INVALID.into()));
    }
    with_device(mem, this, |hot| {
        if hot.recording {
            Err(Stop::Guest)
        } else {
            Ok(())
        }
    })?;
    let values = read_f32s::<M, 4>(mem, plane_ptr)?;
    if values.iter().any(|v| !v.is_finite()) {
        return Ok(INVALID.into());
    }
    with_device_mut(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        let changed = programmable::apply_clip_plane(&mut hot.fixed, index, values);
        if changed {
            hot.touch(6);
        }
        Ok((0u32.into(), changed))
    })
}

body!(pub(super) GET_CLIP_PLANE = get_clip_plane, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn get_clip_plane<M: Mem>(mem: &mut M, [this, index, output, ..]: [u32; 7]) -> Result<Ret, Stop> {
    let values = with_device(mem, this, |hot| {
        Ok(hot.fixed.clip_planes.get(index as usize).copied())
    })?;
    match values {
        Some(values) => write_f32s(mem, output, &values),
        None => Ok(INVALID.into()),
    }
}

fn set_constants<M: Mem>(
    mem: &mut M,
    [this, start, input_ptr, count, ..]: [u32; 7],
    stage: Stage,
    kind: ConstantKind,
) -> Result<Ret, Stop> {
    let recording = with_device(mem, this, |hot| Ok(hot.recording))?;
    let Some(range) = shaders::constant_range(start, count, shaders::constant_limit(stage, kind))
    else {
        return Ok(INVALID.into());
    };
    let bytes = count * shaders::constant_width(kind);
    if bytes == 0 {
        return Ok(0u32.into());
    }
    if recording {
        return Err(Stop::Guest);
    }
    let ptr = mem.input(input_ptr, bytes).map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.input(input_ptr, bytes)` checked `bytes` readable bytes.
    let data = unsafe { std::slice::from_raw_parts(ptr, bytes as usize) };
    let changed = {
        let shared = shared(mem)?.borrow();
        let cell = shared.device(this).ok_or(Stop::Guest)?;
        let mut hot = cell.borrow_mut();
        if hot.recording {
            return Err(Stop::Guest);
        }
        let changed = shaders::apply_constants(hot.shaders.constants_mut(stage), range, data, kind);
        if changed {
            hot.touch(13 + usize::from(stage == Stage::Pixel));
        }
        changed
    };
    if changed {
        mark_work(mem);
    }
    Ok(0u32.into())
}

fn get_constants<M: Mem>(
    mem: &mut M,
    [this, start, output_ptr, count, ..]: [u32; 7],
    stage: Stage,
    kind: ConstantKind,
) -> Result<Ret, Stop> {
    with_device(mem, this, |_| Ok(()))?;
    let Some(range) = shaders::constant_range(start, count, shaders::constant_limit(stage, kind))
    else {
        return Ok(INVALID.into());
    };
    let bytes = range.len() * shaders::constant_width(kind) as usize;
    if bytes == 0 {
        return Ok(0u32.into());
    }
    let out = mem
        .output(output_ptr, bytes as u32)
        .map_err(|_| Stop::Slow)?;
    // SAFETY: `mem.output(output_ptr, bytes)` checked `bytes` writable bytes.
    let dst = unsafe { std::slice::from_raw_parts_mut(out, bytes) };
    with_device(mem, this, |hot| {
        shaders::read_constants(hot.shaders.constants(stage), range, dst, kind);
        Ok(())
    })?;
    mem.commit()?;
    Ok(0u32.into())
}

fn set_vertex_f<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Vertex, ConstantKind::Float)
}
fn get_vertex_f<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Vertex, ConstantKind::Float)
}
fn set_vertex_i<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Vertex, ConstantKind::Integer)
}
fn get_vertex_i<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Vertex, ConstantKind::Integer)
}
fn set_vertex_b<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Vertex, ConstantKind::Boolean)
}
fn get_vertex_b<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Vertex, ConstantKind::Boolean)
}
fn set_pixel_f<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Pixel, ConstantKind::Float)
}
fn get_pixel_f<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Pixel, ConstantKind::Float)
}
fn set_pixel_i<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Pixel, ConstantKind::Integer)
}
fn get_pixel_i<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Pixel, ConstantKind::Integer)
}
fn set_pixel_b<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_constants(mem, args, Stage::Pixel, ConstantKind::Boolean)
}
fn get_pixel_b<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    get_constants(mem, args, Stage::Pixel, ConstantKind::Boolean)
}

body!(pub(super) SET_VERTEX_F = set_vertex_f, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_VERTEX_F = get_vertex_f, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) SET_VERTEX_I = set_vertex_i, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_VERTEX_I = get_vertex_i, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) SET_VERTEX_B = set_vertex_b, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_VERTEX_B = get_vertex_b, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) SET_PIXEL_F = set_pixel_f, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_PIXEL_F = get_pixel_f, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) SET_PIXEL_I = set_pixel_i, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_PIXEL_I = get_pixel_i, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) SET_PIXEL_B = set_pixel_b, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);
body!(pub(super) GET_PIXEL_B = get_pixel_b, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3)], outputs: EAX, pop: 16);

fn set_shader_body<M: Mem>(
    mem: &mut M,
    [this, pointer, ..]: [u32; 7],
    pixel: bool,
) -> Result<Ret, Stop> {
    with_device(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        let current = if pixel {
            hot.pixel_shader_ptr
        } else {
            hot.vertex_shader_ptr
        };
        if pointer == current {
            Ok(0u32.into())
        } else {
            Err(Stop::Guest)
        }
    })
}

body!(pub(super) SET_VERTEX_SHADER = set_vertex_shader, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_vertex_shader<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_shader_body(mem, args, false)
}

body!(pub(super) SET_PIXEL_SHADER = set_pixel_shader, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_pixel_shader<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    set_shader_body(mem, args, true)
}

body!(pub(super) SET_VERTEX_DECLARATION = set_vertex_declaration, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_vertex_declaration<M: Mem>(mem: &mut M, [this, pointer, ..]: [u32; 7]) -> Result<Ret, Stop> {
    with_device(mem, this, |hot| {
        if hot.recording {
            return Err(Stop::Guest);
        }
        if pointer == hot.declaration_ptr && hot.fixed.fvf == 0 {
            Ok(0u32.into())
        } else {
            Err(Stop::Guest)
        }
    })
}

body!(pub(super) SET_STREAM_SOURCE = set_stream_source, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2), Input::Stack(3), Input::Stack(4)], outputs: EAX, pop: 20);
fn set_stream_source<M: Mem>(
    mem: &mut M,
    [this, stream, pointer, offset, stride, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        let Some(current) = hot.stream_ptrs.get(stream as usize).copied() else {
            return Ok((INVALID.into(), false));
        };
        if hot.recording || pointer != current {
            return Err(Stop::Guest);
        }
        if pointer == 0 {
            return Ok((0u32.into(), false));
        }
        let len = hot.stream_lens[stream as usize];
        if offset > len || stride > 2048 {
            return Ok((INVALID.into(), false));
        }
        let s = &mut hot.buffers.streams[stream as usize];
        if s.offset == offset && s.stride == stride {
            return Ok((0u32.into(), false));
        }
        s.offset = offset;
        s.stride = stride;
        hot.touch(10);
        Ok((0u32.into(), true))
    })
}

body!(pub(super) SET_STREAM_SOURCE_FREQ = set_stream_source_freq, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn set_stream_source_freq<M: Mem>(
    mem: &mut M,
    [this, stream, setting, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        match buffers::check_stream_freq(stream, setting) {
            Err(code) => return Ok((code.into(), false)),
            Ok(false) => return Err(Stop::Guest),
            Ok(true) => {}
        }
        if hot.recording {
            return Err(Stop::Guest);
        }
        let slot = &mut hot.buffers.frequencies[stream as usize];
        let changed = *slot != setting;
        if changed {
            *slot = setting;
            hot.touch(10);
        }
        Ok((0u32.into(), changed))
    })
}

body!(pub(super) GET_STREAM_SOURCE_FREQ = get_stream_source_freq, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn get_stream_source_freq<M: Mem>(
    mem: &mut M,
    [this, stream, output, ..]: [u32; 7],
) -> Result<Ret, Stop> {
    let setting = with_device(mem, this, |hot| {
        Ok(hot.buffers.frequencies.get(stream as usize).copied())
    })?;
    match setting {
        Some(setting) => write_u32(mem, output, setting),
        None => Ok(INVALID.into()),
    }
}

body!(pub(super) SET_INDICES = set_indices, inputs: &[Input::Stack(0), Input::Stack(1)], outputs: EAX, pop: 8);
fn set_indices<M: Mem>(mem: &mut M, [this, pointer, ..]: [u32; 7]) -> Result<Ret, Stop> {
    with_device(mem, this, |hot| {
        if hot.recording || pointer != hot.indices_ptr {
            Err(Stop::Guest)
        } else {
            Ok(0u32.into())
        }
    })
}

body!(pub(super) SET_TEXTURE = set_texture, inputs: &[Input::Stack(0), Input::Stack(1), Input::Stack(2)], outputs: EAX, pop: 12);
fn set_texture<M: Mem>(mem: &mut M, [this, stage, pointer, ..]: [u32; 7]) -> Result<Ret, Stop> {
    with_device(mem, this, |hot| {
        let Some(slot) = programmable::sampler_slot(stage) else {
            return Ok(INVALID.into());
        };
        if hot.recording {
            return Err(Stop::Guest);
        }
        if pointer == hot.texture_ptrs[slot]
            && (pointer == 0 || hot.texture_clean & (1 << slot) != 0)
        {
            Ok(0u32.into())
        } else {
            Err(Stop::Guest)
        }
    })
}

fn scene_body<M: Mem>(mem: &mut M, [this, ..]: [u32; 7], begin: bool) -> Result<Ret, Stop> {
    with_device_mut(mem, this, |hot| {
        if hot.scene == begin {
            return Ok((INVALID.into(), false));
        }
        hot.scene = begin;
        Ok((0u32.into(), true))
    })
}

body!(pub(super) BEGIN_SCENE = begin_scene, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
fn begin_scene<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    scene_body(mem, args, true)
}

body!(pub(super) END_SCENE = end_scene, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
fn end_scene<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    scene_body(mem, args, false)
}

fn state_block_body<M: Mem>(mem: &mut M, [this, ..]: [u32; 7], apply: bool) -> Result<Ret, Stop> {
    if apply && cfg!(feature = "d3d9-counters") {
        return Err(Stop::Guest);
    }
    let shared = shared(mem)?.borrow();
    let sb = shared.state_block(this).ok_or(Stop::Guest)?.borrow();
    let hot = sb.device.borrow();
    if hot.recording {
        return Ok(INVALID.into());
    }
    if sb.current(&hot) {
        return Ok(0u32.into());
    }
    Err(Stop::Guest)
}

body!(pub(super) STATEBLOCK_CAPTURE = stateblock_capture, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
fn stateblock_capture<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    state_block_body(mem, args, false)
}

body!(pub(super) STATEBLOCK_APPLY = stateblock_apply, inputs: &[Input::Stack(0)], outputs: EAX, pop: 4);
fn stateblock_apply<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    state_block_body(mem, args, true)
}

pub(super) fn lookup(method: &str) -> Option<adv_cpu::body::DirectBody> {
    Some(match method {
        "IDirect3DDevice9::SetRenderState" => SET_RENDER.direct,
        "IDirect3DDevice9::GetRenderState" => GET_RENDER.direct,
        "IDirect3DDevice9::SetTextureStageState" => SET_STAGE.direct,
        "IDirect3DDevice9::GetTextureStageState" => GET_STAGE.direct,
        "IDirect3DDevice9::SetSamplerState" => SET_SAMPLER.direct,
        "IDirect3DDevice9::GetSamplerState" => GET_SAMPLER.direct,
        "IDirect3DDevice9::SetFVF" => SET_FVF.direct,
        "IDirect3DDevice9::GetFVF" => GET_FVF.direct,
        "IDirect3DDevice9::SetViewport" => SET_VIEWPORT.direct,
        "IDirect3DDevice9::GetViewport" => GET_VIEWPORT.direct,
        "IDirect3DDevice9::SetScissorRect" => SET_SCISSOR.direct,
        "IDirect3DDevice9::GetScissorRect" => GET_SCISSOR.direct,
        "IDirect3DDevice9::SetTransform" => SET_TRANSFORM.direct,
        "IDirect3DDevice9::GetTransform" => GET_TRANSFORM.direct,
        "IDirect3DDevice9::MultiplyTransform" => MULTIPLY_TRANSFORM.direct,
        "IDirect3DDevice9::SetClipPlane" => SET_CLIP_PLANE.direct,
        "IDirect3DDevice9::GetClipPlane" => GET_CLIP_PLANE.direct,
        "IDirect3DDevice9::SetVertexShaderConstantF" => SET_VERTEX_F.direct,
        "IDirect3DDevice9::GetVertexShaderConstantF" => GET_VERTEX_F.direct,
        "IDirect3DDevice9::SetVertexShaderConstantI" => SET_VERTEX_I.direct,
        "IDirect3DDevice9::GetVertexShaderConstantI" => GET_VERTEX_I.direct,
        "IDirect3DDevice9::SetVertexShaderConstantB" => SET_VERTEX_B.direct,
        "IDirect3DDevice9::GetVertexShaderConstantB" => GET_VERTEX_B.direct,
        "IDirect3DDevice9::SetPixelShaderConstantF" => SET_PIXEL_F.direct,
        "IDirect3DDevice9::GetPixelShaderConstantF" => GET_PIXEL_F.direct,
        "IDirect3DDevice9::SetPixelShaderConstantI" => SET_PIXEL_I.direct,
        "IDirect3DDevice9::GetPixelShaderConstantI" => GET_PIXEL_I.direct,
        "IDirect3DDevice9::SetPixelShaderConstantB" => SET_PIXEL_B.direct,
        "IDirect3DDevice9::GetPixelShaderConstantB" => GET_PIXEL_B.direct,
        "IDirect3DDevice9::SetVertexShader" => SET_VERTEX_SHADER.direct,
        "IDirect3DDevice9::SetPixelShader" => SET_PIXEL_SHADER.direct,
        "IDirect3DDevice9::SetVertexDeclaration" => SET_VERTEX_DECLARATION.direct,
        "IDirect3DDevice9::SetStreamSource" => SET_STREAM_SOURCE.direct,
        "IDirect3DDevice9::SetStreamSourceFreq" => SET_STREAM_SOURCE_FREQ.direct,
        "IDirect3DDevice9::GetStreamSourceFreq" => GET_STREAM_SOURCE_FREQ.direct,
        "IDirect3DDevice9::SetIndices" => SET_INDICES.direct,
        "IDirect3DDevice9::SetTexture" => SET_TEXTURE.direct,
        "IDirect3DDevice9::BeginScene" => BEGIN_SCENE.direct,
        "IDirect3DDevice9::EndScene" => END_SCENE.direct,
        "IDirect3DStateBlock9::Capture" => STATEBLOCK_CAPTURE.direct,
        "IDirect3DStateBlock9::Apply" => STATEBLOCK_APPLY.direct,
        _ => return None,
    })
}
