//! Legacy argument and object adapters for the shared fixed-function device.
use super::{Calls, Face, INVALID, Object, caps, creation, decode, read, write};
use crate::{
    argument::{Arguments, CallError},
    ddraw::{self, DirectDraw},
    fixed::{self, Render, Sampler, Setting, Stage, tnl},
};
use adv_abi::types::*;
use adv_device::surface::{Blt, Rect};
use bytemuck::bytes_of;

use super::super::{desc::vertex, device, device_mut, object::PrimitiveBatch, report};

const NO_CURRENT_VIEWPORT: u32 = 0x88760307;
const VIEWPORT_HAS_NO_DEVICE: u32 = 0x88760306;
const NOT_FOUND: u32 = 0x887600ff;

impl Calls<'_> {
    pub(super) fn get_caps(&mut self, face: Face, args: &[u32]) -> Result<u32, CallError> {
        let Some(device) = device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        if face == Face::Device7 {
            write(
                self.memory,
                args[1],
                &report::derive(self.store, device.kind).device,
            )?;
        } else {
            let report = report::legacy_caps(self.store, device.kind, face);
            if args[1] == 0 && args[2] == 0 {
                return Ok(INVALID);
            }
            for (ptr, desc) in [(args[1], report.hardware), (args[2], report.software)] {
                if ptr == 0 {
                    continue;
                }
                let size = self.memory.arg_u32(ptr)?;
                if !matches!(size, 172 | 204 | 252) {
                    return Ok(INVALID);
                }
                self.memory
                    .arg_write(ptr, &bytes_of(&desc)[..size as usize])?;
            }
        }
        Ok(0)
    }

    pub(super) fn legacy_method(
        &mut self,
        face: Face,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let this = args[0];
        Ok(match method {
            "Initialize" => ddraw::ALREADY_INITIALIZED,
            "Reserve" | "Unreserve" | "PaletteChanged" => 0,
            "Unload" => {
                let Some(id) = self.draw.surface_id(self.objects, this) else {
                    return Ok(INVALID);
                };
                self.draw
                    .d3d
                    .as_mut()
                    .unwrap()
                    .flush(self.store, self.memory)?;
                self.store.evict_surface_image(id, self.memory)?;
                0
            }
            "EvictManagedTextures" => {
                self.draw
                    .d3d
                    .as_mut()
                    .unwrap()
                    .flush(self.store, self.memory)?;
                for (&id, desc) in &self.draw.described {
                    if desc.caps[1] & creation::MANAGED != 0 {
                        self.store.evict_surface_image(id, self.memory)?;
                    }
                }
                0
            }
            "FindDevice" => return self.find_device(face, args),
            "NextViewport" | "NextLight" => return self.next_attachment(method, args),
            "SetCurrentViewport" => return self.select_viewport(this, args[1]),
            "GetCurrentViewport" => {
                let Some(device) = device(self.draw, self.objects, this) else {
                    return Ok(INVALID);
                };
                let pointer = device.current_viewport;
                self.memory.arg_put_u32(args[1], pointer)?;
                if pointer == 0 {
                    NO_CURRENT_VIEWPORT
                } else {
                    super::super::add_ref(self.draw, self.objects, pointer);
                    0
                }
            }
            "Clear"
            | "Clear2"
            | "SetBackground"
            | "GetBackground"
            | "SetBackgroundDepth"
            | "SetBackgroundDepth2"
            | "GetBackgroundDepth"
            | "GetBackgroundDepth2" => {
                return self.viewport_method(method, args);
            }
            "SetLightState" | "GetLightState" => {
                return self.light_state(this, method, args[1], args[2]);
            }
            "SwapTextureHandles" => {
                let com = self.draw.com.unwrap();
                let canonical = |pointer| {
                    com.identity::<ddraw::ComState>(self.objects, pointer)
                        .and_then(|id| com.canonical(self.objects, id))
                };
                let (Some(a), Some(b)) = (canonical(args[1]), canonical(args[2])) else {
                    return Ok(INVALID);
                };
                let Some(device) = device_mut(self.draw, self.objects, this) else {
                    return Ok(INVALID);
                };
                device.swap_texture_handles(a, b);
                0
            }
            "CreateMatrix" | "SetMatrix" | "GetMatrix" | "DeleteMatrix" => {
                return self.matrix(method, args);
            }
            "Begin" | "BeginIndexed" | "Vertex" | "Index" | "End" => {
                return self.immediate(face, method, args);
            }
            "Load" => {
                if face == Face::Device7 {
                    return self.load_texture(args[1], args[3], args[2], args[4], args[5]);
                }
                return self.load_texture(this, args[1], 0, 0, 0);
            }
            "GetInfo" if face == Face::Device7 => {
                let Some(dev) = device(self.draw, self.objects, this) else {
                    return Ok(INVALID);
                };
                let zero = [0; 256];
                for offset in (0..args[3]).step_by(zero.len()) {
                    let count = (args[3] - offset).min(zero.len() as u32) as usize;
                    self.memory.arg_write(args[2] + offset, &zero[..count])?;
                }
                if dev.kind == caps::Device::Rgb && !matches!(args[1], 1..=3) {
                    0x80004005
                } else {
                    1
                }
            }
            "GetInfo" => 1,
            "GetStats" => {
                let Some(device) = device(self.draw, self.objects, this) else {
                    return Ok(INVALID);
                };
                if self.memory.arg_u32(args[1])? != 24 {
                    return Ok(INVALID);
                }
                self.memory
                    .arg_write(args[1] + 4, bytemuck::cast_slice(&device.legacy.stats))?;
                0
            }
            "ComputeSphereVisibility" => return self.sphere_visibility(args),
            _ => return Err(format!("unbound Direct3D method {method}").into()),
        })
    }

    fn find_device(&mut self, face: Face, args: &[u32]) -> Result<u32, CallError> {
        let search: D3DFINDDEVICESEARCH = read(self.memory, args[1])?;
        if search.dwSize != size_of::<D3DFINDDEVICESEARCH>() as u32
            || self.memory.arg_u32(args[2])? != size_of::<D3DFINDDEVICERESULT>() as u32
        {
            return Ok(INVALID);
        }
        for class in [caps::Device::Rgb, caps::Device::Hal] {
            let report = report::legacy_caps(self.store, class, face);
            let hardware = class != caps::Device::Rgb;
            let desc = if hardware {
                &report.hardware
            } else {
                &report.software
            };
            let flags = search.dwFlags;
            if (flags & 1 != 0 && search.dcmColorModel != desc.dcmColorModel)
                || (flags & 2 != 0 && bytes_of(&search.guid) != class.guid())
                || (flags & 4 != 0 && (search.bHardware != 0) != hardware)
                || (search.dwCaps & !desc.dwDevCaps != 0)
            {
                continue;
            }
            let fits = |caps: &D3DPRIMCAPS| {
                let request: [u32; 14] = bytemuck::cast(search.dpcPrimCaps);
                let available: [u32; 14] = bytemuck::cast(*caps);
                [1, 2, 3, 6, 4, 5, 7, 8, 9, 10, 11]
                    .into_iter()
                    .enumerate()
                    .all(|(i, field)| {
                        flags & (0x20 << i) == 0 || request[field] & !available[field] == 0
                    })
            };
            if (flags & 8 != 0 && !fits(&desc.dpcTriCaps))
                || (flags & 16 != 0 && !fits(&desc.dpcLineCaps))
            {
                continue;
            }
            return write(
                self.memory,
                args[2],
                &D3DFINDDEVICERESULT {
                    dwSize: size_of::<D3DFINDDEVICERESULT>() as u32,
                    guid: bytemuck::cast(class.guid()),
                    ddHwDesc: report.hardware,
                    ddSwDesc: report.software,
                },
            )
            .map(|()| 0);
        }
        Ok(NOT_FOUND)
    }

    fn next_attachment(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let com = self.draw.com.unwrap();
        let list = if method == "NextLight" {
            let Some(Object::Viewport { lights, .. }) = com.get::<Object>(self.objects, args[0])
            else {
                return Ok(INVALID);
            };
            lights
        } else {
            let Some(device) = device(self.draw, self.objects, args[0]) else {
                return Ok(INVALID);
            };
            &device.viewports
        };
        let item = match args[3] {
            1 => list
                .iter()
                .position(|&p| p == args[1])
                .and_then(|i| list.get(i + 1)),
            2 => list.first(),
            4 => list.last(),
            _ => return Ok(INVALID),
        }
        .copied()
        .unwrap_or(0);
        self.memory.arg_put_u32(args[2], item)?;
        if item != 0 {
            com.add_ref::<Object>(self.objects, item);
        }
        Ok(0)
    }

    pub(super) fn select_viewport(&mut self, this: u32, viewport: u32) -> Result<u32, CallError> {
        let Some(dev) = device(self.draw, self.objects, this) else {
            return Ok(INVALID);
        };
        if !dev.viewports.contains(&viewport) {
            return Ok(INVALID);
        }
        let com = self.draw.com.unwrap();
        let Some(Object::Viewport { value, lights, .. }) =
            com.get::<Object>(self.objects, viewport)
        else {
            return Ok(INVALID);
        };
        if value[0] == 0 {
            return Ok(0x88760305);
        }
        let sid = self.draw.surface_id(self.objects, dev.target).unwrap();
        let target = &self.store[sid];
        let view = match decode::viewport(
            D3DVIEWPORT7 {
                dwX: value[1],
                dwY: value[2],
                dwWidth: value[3],
                dwHeight: value[4],
                dvMinZ: f32::from_bits(value[9]),
                dvMaxZ: f32::from_bits(value[10]),
            },
            (target.width, target.height),
        ) {
            Ok(view) => view,
            Err(hr) => return Ok(hr),
        };
        let lights: Vec<_> = lights
            .iter()
            .filter_map(|&pointer| {
                let Object::Light { value, .. } = com.get::<Object>(self.objects, pointer)? else {
                    return None;
                };
                let mut words = [0; 26];
                words[0] = value[1];
                words[1..5].copy_from_slice(&value[2..6]);
                words[5..9].copy_from_slice(&value[2..6]);
                words[13..].copy_from_slice(&value[6..19]);
                decode::light(words).ok()
            })
            .collect();
        let dev = device_mut(self.draw, self.objects, this).unwrap();
        dev.current_viewport = viewport;
        let state = dev.state.as_mut().unwrap();
        state.set(Setting::Viewport(view));
        state.fixed.lights.clear();
        state.fixed.invalidate_key();
        for (i, value) in lights.into_iter().enumerate() {
            state.set(Setting::Light(i as u32, value));
            state.set(Setting::LightEnable(i as u32, true));
        }
        Ok(0)
    }

    fn viewport_method(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let com = self.draw.com.unwrap();
        let Some(Object::Viewport {
            device: owner,
            background,
            depth,
            value,
            ..
        }) = com.get_mut::<Object>(self.objects, args[0])
        else {
            return Ok(INVALID);
        };
        let (owner, background_value, value) = (*owner, *background, *value);
        match method {
            "SetBackground" => *background = args[1],
            "GetBackground" => {
                self.memory.arg_put_u32(args[1], background_value)?;
                self.memory
                    .arg_put_u32(args[2], u32::from(background_value != 0))?;
            }
            "SetBackgroundDepth" | "SetBackgroundDepth2" => {
                let previous = *depth;
                if args[1] != 0
                    && com
                        .add_ref::<ddraw::ComState>(self.objects, args[1])
                        .is_none()
                {
                    return Ok(INVALID);
                }
                if let Some(Object::Viewport { depth, .. }) =
                    com.get_mut::<Object>(self.objects, args[0])
                {
                    *depth = args[1];
                }
                if previous != 0 {
                    self.draw.release_com(self.objects, self.store, previous);
                }
            }
            "GetBackgroundDepth" | "GetBackgroundDepth2" => {
                let iid = if method.ends_with('2') {
                    ddraw::IID_SURFACE4
                } else {
                    ddraw::IID_SURFACE
                };
                let depth = *depth;
                let pointer = if depth == 0 {
                    0
                } else {
                    match com.query::<ddraw::ComState>(self.objects, depth, iid) {
                        Ok(pointer) => pointer,
                        Err(_) => return Ok(INVALID),
                    }
                };
                self.memory.arg_put_u32(args[1], pointer)?;
                self.memory.arg_put_u32(args[2], u32::from(pointer != 0))?;
            }
            "Clear" | "Clear2" => {
                let Some(dev) = owner.and_then(|owner| device(self.draw, self.objects, owner))
                else {
                    return Ok(VIEWPORT_HAS_NO_DEVICE);
                };
                let sid = self.draw.surface_id(self.objects, dev.target).unwrap();
                if self.draw.lost.contains_key(&sid) {
                    return Ok(0);
                }
                let view = Rect {
                    x: value[1],
                    y: value[2],
                    w: value[3],
                    h: value[4],
                }
                .clipped(self.store[sid].rect());
                if method == "Clear2" {
                    return self.clear(sid, view, args);
                }
                let color = dev
                    .material_for_handle(background_value)
                    .and_then(|pointer| com.get::<Object>(self.objects, pointer))
                    .and_then(|object| match object {
                        Object::Material(words) => Some(super::process::argb(
                            words[1..5]
                                .try_into()
                                .map(|a: [u32; 4]| a.map(f32::from_bits))
                                .unwrap(),
                        )),
                        Object::Device(_)
                        | Object::Viewport { .. }
                        | Object::Light { .. }
                        | Object::VertexBuffer { .. }
                        | Object::ExecuteBuffer(_) => None,
                    })
                    .unwrap_or(0);
                return self.clear(
                    sid,
                    view,
                    &[args[0], args[1], args[2], args[3], color, 1f32.to_bits(), 0],
                );
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    pub(super) fn light_state(
        &mut self,
        this: u32,
        method: &str,
        selector: u32,
        value: u32,
    ) -> Result<u32, CallError> {
        let Some(dev) = device(self.draw, self.objects, this) else {
            return Ok(INVALID);
        };
        if method == "GetLightState" {
            let Some(value_read) = dev.legacy.light.get(&selector).copied().or(match selector {
                1 | 2 | 4 | 5 => Some(0),
                3 => Some(2),
                6 | 7 => Some(1f32.to_bits()),
                8 => Some(1),
                _ => None,
            }) else {
                return Ok(INVALID);
            };
            self.memory.arg_put_u32(value, value_read)?;
            return Ok(0);
        }
        let setting = match selector {
            1 => {
                let material = if value == 0 {
                    decode::defaults(1, 1, false).material
                } else {
                    let Some(pointer) = dev.material_for_handle(value) else {
                        return Ok(INVALID);
                    };
                    let Some(Object::Material(words)) =
                        self.draw.com.unwrap().get::<Object>(self.objects, pointer)
                    else {
                        return Ok(INVALID);
                    };
                    fixed::material(
                        <[u32; 17]>::try_from(&words[1..18])
                            .unwrap()
                            .map(f32::from_bits),
                    )
                };
                Setting::Material(material)
            }
            2 => Setting::Render(Render::Ambient, value),
            3 if value == 2 => return Ok(0),
            4 => Setting::Render(Render::VertexFog, value),
            5 => Setting::Render(Render::FogStart, value),
            6 => Setting::Render(Render::FogEnd, value),
            7 => Setting::Render(Render::FogDensity, value),
            8 => Setting::Render(Render::ColorVertex, value),
            _ => return Ok(INVALID),
        };
        let dev = device_mut(self.draw, self.objects, this).unwrap();
        dev.state.as_mut().unwrap().set(setting);
        dev.legacy.light.insert(selector, value);
        Ok(0)
    }

    pub(super) fn legacy_render_state(
        &mut self,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let [this, slot, value] = <[u32; 3]>::try_from(args).unwrap();
        let Some(dev) = device(self.draw, self.objects, this) else {
            return Ok(INVALID);
        };
        if method == "GetRenderState" {
            let Some(value_read) = dev.legacy.render.get(&slot).copied().or(match slot {
                1 | 5 | 6 => Some(0),
                3 | 17 | 18 => Some(1),
                21 => Some(2),
                _ => None,
            }) else {
                return Ok(INVALID);
            };
            self.memory.arg_put_u32(value, value_read)?;
            return Ok(0);
        }
        if slot == 1 {
            let texture = if value == 0 {
                0
            } else {
                let Some(texture) = dev.texture_for_handle(value) else {
                    return Ok(0x887602d6);
                };
                texture
            };
            let hr = self.render_method(Face::Device3, "SetTexture", &[this, 0, texture])?;
            if hr != 0 {
                return Ok(hr);
            }
        }
        let dev = device_mut(self.draw, self.objects, this).unwrap();
        let state = dev.state.as_mut().unwrap();
        match slot {
            1 => (),
            3 => {
                state.set(Setting::Sampler(Sampler::AddressU, value));
                state.set(Setting::Sampler(Sampler::AddressV, value));
            }
            5 | 6 => {
                let wrap = state.fixed.render[&128];
                let bit = 1 << (slot - 5);
                state.set(Setting::Render(
                    Render::Wrap0,
                    (wrap & !bit) | (u32::from(value != 0) * bit),
                ));
            }
            17 if matches!(value, 1 | 2) => state.set(Setting::Sampler(Sampler::MagFilter, value)),
            18 if (1..=6).contains(&value) => {
                state.set(Setting::Sampler(
                    Sampler::MinFilter,
                    if matches!(value, 2 | 5 | 6) { 2 } else { 1 },
                ));
                state.set(Setting::Sampler(
                    Sampler::MipFilter,
                    match value {
                        3 | 5 => 1,
                        4 | 6 => 2,
                        _ => 0,
                    },
                ));
            }
            21 if (1..=8).contains(&value) => {
                let (color, alpha) = match value {
                    1 | 7 => (2, 2),
                    2 => (4, 2),
                    3 | 5 => (13, 3),
                    4 | 6 => (4, 4),
                    8 => (7, 3),
                    _ => unreachable!(),
                };
                for (slot, value) in [
                    (Stage::ColorOp, color),
                    (Stage::ColorArg1, 2),
                    (Stage::ColorArg2, 0),
                    (Stage::AlphaOp, alpha),
                    (Stage::AlphaArg1, 2),
                    (Stage::AlphaArg2, 0),
                ] {
                    state.set(Setting::Stage(slot, value));
                }
            }
            43 => state.set(Setting::Sampler(Sampler::BorderColor, value)),
            44 => state.set(Setting::Sampler(Sampler::AddressU, value)),
            45 => state.set(Setting::Sampler(Sampler::AddressV, value)),
            46 => state.set(Setting::Sampler(Sampler::MipLodBias, value)),
            49 => state.set(Setting::Sampler(Sampler::MaxAnisotropy, value)),
            _ => return Ok(INVALID),
        }
        dev.legacy.render.insert(slot, value);
        Ok(0)
    }

    fn matrix(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let Some(dev) = device_mut(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let matrices = &mut dev.legacy.matrices;
        match method {
            "CreateMatrix" => {
                dev.legacy.next_matrix += 1;
                self.memory.arg_put_u32(args[1], dev.legacy.next_matrix)?;
                matrices.insert(
                    dev.legacy.next_matrix,
                    std::array::from_fn(|i| f32::from(i % 5 == 0)),
                );
            }
            "DeleteMatrix" => {
                if matrices.remove(&args[1]).is_none() {
                    return Ok(0x887602db);
                }
            }
            "SetMatrix" | "GetMatrix" => {
                let Some(matrix) = matrices.get_mut(&args[1]) else {
                    return Ok(if method == "SetMatrix" {
                        0x887602dc
                    } else {
                        0x887602dd
                    });
                };
                if method == "SetMatrix" {
                    *matrix = read(self.memory, args[2])?;
                } else {
                    write(self.memory, args[2], matrix)?;
                }
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    fn immediate(&mut self, face: Face, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let Some(dev) = device_mut(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        match method {
            "Begin" | "BeginIndexed" => {
                if dev.legacy.batch.is_some() {
                    return Ok(INVALID);
                }
                let indexed = method == "BeginIndexed";
                let fvf = if face == Face::Device2 {
                    let Some(fvf) = vertex::legacy_fvf(args[2]) else {
                        return Ok(INVALID);
                    };
                    fvf
                } else {
                    args[2]
                };
                let Ok(layout) = vertex::layout(fvf) else {
                    return Ok(INVALID);
                };
                let vertices = if indexed {
                    self.memory
                        .arg_read(args[3], args[4] as usize * layout.size as usize)?
                        .into_owned()
                } else {
                    Vec::new()
                };
                dev.legacy.batch = Some(PrimitiveBatch {
                    fvf,
                    vertices,
                    primitive: args[1],
                    flags: args[if indexed { 5 } else { 3 }],
                    indices: indexed.then(Vec::new),
                });
            }
            "Vertex" | "Index" => {
                let Some(batch) = dev.legacy.batch.as_mut() else {
                    return Ok(INVALID);
                };
                match (&mut batch.indices, method) {
                    (Some(indices), "Index") => indices.push(args[1]),
                    (None, "Vertex") => {
                        let stride = vertex::layout(batch.fvf).unwrap().size;
                        batch
                            .vertices
                            .extend_from_slice(&self.memory.arg_read(args[1], stride as usize)?);
                    }
                    _ => return Ok(INVALID),
                }
            }
            "End" => {
                let Some(batch) = dev.legacy.batch.take() else {
                    return Ok(INVALID);
                };
                return self.emit_primitive(args[0], batch, None);
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    pub(super) fn load_texture(
        &mut self,
        dst: u32,
        src: u32,
        point: u32,
        rect: u32,
        flags: u32,
    ) -> Result<u32, CallError> {
        let (Some(dst), Some(src)) = (
            self.draw.surface_id(self.objects, dst),
            self.draw.surface_id(self.objects, src),
        ) else {
            return Ok(INVALID);
        };
        if flags != 0
            || [dst, src]
                .into_iter()
                .any(|sid| self.draw.caps(sid)[0] & creation::TEXTURE == 0)
        {
            return Ok(INVALID);
        }
        if self.draw.lost.contains_key(&dst) || self.draw.lost.contains_key(&src) {
            return Ok(ddraw::SURFACE_LOST);
        }
        let src_members = std::iter::once(src).chain(self.draw.attached(src));
        let dst_members = std::iter::once(dst).chain(self.draw.attached(dst));
        let [x, y]: [i32; 2] = if point == 0 {
            [0, 0]
        } else {
            read(self.memory, point)?
        };
        let source = if rect == 0 {
            self.store[src].rect()
        } else {
            let [x, y, right, bottom]: [i32; 4] = read(self.memory, rect)?;
            if x < 0
                || y < 0
                || right <= x
                || bottom <= y
                || right as u32 > self.store[src].width
                || bottom as u32 > self.store[src].height
            {
                return Ok(INVALID);
            }
            Rect {
                x: x as u32,
                y: y as u32,
                w: (right - x) as u32,
                h: (bottom - y) as u32,
            }
        };
        self.draw
            .d3d
            .as_mut()
            .unwrap()
            .flush(self.store, self.memory)?;
        for (level, (src, dst)) in src_members.zip(dst_members).enumerate() {
            let source = Rect {
                x: source.x >> level,
                y: source.y >> level,
                w: (source.w >> level).max(1),
                h: (source.h >> level).max(1),
            };
            let (x, y) = (x >> level, y >> level);
            if x < 0
                || y < 0
                || x as u32 + source.w > self.store[dst].width
                || y as u32 + source.h > self.store[dst].height
            {
                return Ok(INVALID);
            }
            let dest = self.draw.blt_surface(
                dst,
                Rect {
                    x: x as u32,
                    y: y as u32,
                    w: source.w,
                    h: source.h,
                },
            );
            let source_surface = self.draw.blt_surface(src, source);
            if let Err(hr) = DirectDraw::execute_blt(
                self.store,
                dest,
                Blt::Copy {
                    source: source_surface,
                    mirror: [false; 2],
                    key: None,
                },
                self.memory,
            )? {
                return Ok(hr);
            }
        }
        Ok(0)
    }

    fn sphere_visibility(&mut self, args: &[u32]) -> Result<u32, CallError> {
        let Some(dev) = device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let transforms = &dev.state.as_ref().unwrap().fixed.transforms;
        let matrix: [[f32; 4]; 4] = std::array::from_fn(|i| {
            tnl::row(
                tnl::row(
                    transforms[0][i * 4..i * 4 + 4].try_into().unwrap(),
                    &transforms[1],
                ),
                &transforms[2],
            )
        });
        let planes: [[f32; 4]; 6] = std::array::from_fn(|i| {
            std::array::from_fn(|j| match i {
                0 => matrix[j][3] + matrix[j][0],
                1 => matrix[j][3] - matrix[j][0],
                2 => matrix[j][3] - matrix[j][1],
                3 => matrix[j][3] + matrix[j][1],
                4 => matrix[j][2],
                _ => matrix[j][3] - matrix[j][2],
            })
        });
        let centers = self
            .memory
            .arg_read(args[1], args[3] as usize * 12)?
            .into_owned();
        let radii = self
            .memory
            .arg_read(args[2], args[3] as usize * 4)?
            .into_owned();
        let result: Vec<u32> = centers
            .as_chunks::<12>()
            .0
            .iter()
            .zip(radii.as_chunks::<4>().0)
            .map(|(center, radius)| {
                let center: [f32; 3] = bytemuck::pod_read_unaligned(center);
                let radius = f32::from_le_bytes(*radius);
                planes.iter().enumerate().fold(0, |flags, (i, plane)| {
                    let distance = (0..3).map(|j| center[j] * plane[j]).sum::<f32>() + plane[3];
                    let extent = radius * (0..3).map(|j| plane[j] * plane[j]).sum::<f32>().sqrt();
                    flags
                        | if distance < -extent {
                            (1 << i) | (1 << (i + 12))
                        } else if distance < extent {
                            1 << i
                        } else {
                            0
                        }
                })
            })
            .collect();
        self.memory
            .arg_write(args[5], bytemuck::cast_slice(&result))?;
        Ok(0)
    }
}
