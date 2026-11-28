//! Device7 admission, surface bindings and recorded raster work.
use super::{Calls, INVALID, OUT_OF_MEMORY, Object, caps, creation, read, write};
use crate::{
    argument::{Arguments, CallError},
    ddraw::{
        self,
        d3d::{
            Face,
            desc::vertex,
            recording::{self, Effects, Payload},
        },
    },
    fixed::{self, Render, Rules, Sampler, Stage, VertexProgram, program::Legacy},
    record::key::hash,
};
use adv_abi::types::{D3DDP_PTRSTRIDE, D3DDRAWPRIMITIVESTRIDEDDATA, D3DVERTEXBUFFERDESC};
use adv_device::{
    constants::ConstantUsage,
    gpu::{
        self, Attribute, AttributeFormat, ConstantSnapshot, DepthFormat, IndexBytes, Program,
        ProgramTexture, ProgrammableDraw, Target, Topology, VertexBytes, VertexStream,
    },
    surface::{Access, Rect, SurfaceMemory},
};
use std::sync::Arc;

const INVALID_OBJECT: u32 = 0x88760082;
const SCENE_IN: u32 = 0x887602f8;
const SCENE_NOT_IN: u32 = 0x887602f9;
const SCENE_BEGIN_FAILED: u32 = 0x887602fa;
const UNSUPPORTED_COLOR_ARGUMENT: u32 = 0x8876081a;
const VB_LOCKED: u32 = 0x8876080e;

fn gpu_error(error: gpu::Error) -> CallError {
    error.to_string().into()
}

pub(super) struct ExecuteDraw<'a> {
    pub edges: Option<&'a [u16]>,
}

impl Calls<'_> {
    pub(super) fn create_vertex_buffer(
        &mut self,
        face: Face,
        args: &[u32],
    ) -> Result<u32, CallError> {
        self.memory.arg_put_u32(args[2], 0)?;
        if args[3] != 0 || (face == Face::Direct3D3 && args[4] != 0) {
            return Ok(INVALID);
        }
        let desc: D3DVERTEXBUFFERDESC = read(self.memory, args[1])?;
        let Ok(layout) = vertex::layout(desc.dwFVF) else {
            return Ok(0x88760800);
        };
        let Some(size) = desc
            .dwNumVertices
            .checked_mul(layout.size)
            .filter(|&size| size != 0)
        else {
            return Ok(INVALID);
        };
        if desc.dwSize != size_of::<D3DVERTEXBUFFERDESC>() as u32 {
            return Ok(INVALID);
        }
        let data = self.store.alloc(size, self.memory)?;
        let result = self.make(
            &[if face == Face::Direct3D3 {
                Face::VertexBuffer
            } else {
                Face::VertexBuffer7
            }],
            &[],
            Object::VertexBuffer {
                desc,
                data,
                size,
                locked: false,
            },
        );
        if !matches!(result, Ok(pointer) if pointer != OUT_OF_MEMORY) {
            self.store.release(data, size);
        }
        let pointer = result?;
        if pointer == OUT_OF_MEMORY {
            return Ok(pointer);
        }
        self.memory.arg_put_u32(args[2], pointer)?;
        Ok(0)
    }

    pub(super) fn vertex_buffer(
        &mut self,
        face: Face,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let Some(Object::VertexBuffer {
            desc,
            data,
            size,
            locked,
        }) = self
            .draw
            .com
            .and_then(|com| com.get_mut::<Object>(self.objects, args[0]))
        else {
            return Ok(INVALID);
        };
        match method {
            "Lock" => {
                if *locked && face != Face::VertexBuffer7 {
                    return Ok(VB_LOCKED);
                }
                self.memory.arg_put_u32(args[2], *data)?;
                if args[3] != 0 {
                    self.memory.arg_put_u32(args[3], *size)?;
                }
                *locked = true;
            }
            "Unlock" => {
                if !*locked && face != Face::VertexBuffer7 {
                    return Ok(0x8876080f);
                }
                *locked = false;
            }
            "GetVertexBufferDesc" => {
                if self.memory.arg_u32(args[1])? != desc.dwSize {
                    return Ok(INVALID);
                }
                write(self.memory, args[1], desc)?;
            }
            "Optimize" => {
                if *locked {
                    return Ok(VB_LOCKED);
                }
                desc.dwCaps |= 0x8000_0000;
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    pub(super) fn render_method(
        &mut self,
        face: Face,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let Some(device) = ddraw::d3d::device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let Some(state) = &device.state else {
            return Ok(INVALID);
        };
        let target = device.target;
        let sid = self.draw.surface_id(self.objects, target).unwrap();
        let lost = self.draw.lost.contains_key(&sid);
        match method {
            "BeginScene" => {
                if state.scene {
                    return Ok(SCENE_IN);
                }
                if lost {
                    return Ok(
                        if self.draw.lost.get(&sid) == Some(&ddraw::Loss::Activation)
                            || (self.draw.exclusive_owner.is_none()
                                && self.draw.creation_modes.get(&sid).is_some_and(|&mode| {
                                    mode != crate::ddraw::front::modes::current(self.store)
                                }))
                        {
                            ddraw::SURFACE_LOST
                        } else {
                            SCENE_BEGIN_FAILED
                        },
                    );
                }
                ddraw::d3d::device_mut(self.draw, self.objects, args[0])
                    .unwrap()
                    .state
                    .as_mut()
                    .unwrap()
                    .scene = true;
            }
            "EndScene" => {
                if !state.scene {
                    return Ok(SCENE_NOT_IN);
                }
                // Recorded writes stay queued until a surface access or
                // presentation plans them.
                ddraw::d3d::device_mut(self.draw, self.objects, args[0])
                    .unwrap()
                    .state
                    .as_mut()
                    .unwrap()
                    .scene = false;
            }
            "GetRenderTarget" | "GetTexture" => {
                let (pointer, out) = if method == "GetRenderTarget" {
                    (target, args[1])
                } else {
                    let Some(&texture) = state.textures.get(args[1] as usize) else {
                        return Ok(INVALID);
                    };
                    (texture, args[2])
                };
                let iid = match (face, method) {
                    (Face::Device2, "GetRenderTarget") => Some(ddraw::IID_SURFACE),
                    (Face::Device3, "GetRenderTarget") => Some(ddraw::IID_SURFACE4),
                    (Face::Device3, "GetTexture") => Some(adv_abi::gfx_iids::IID_IDIRECT3DTEXTURE2),
                    _ => None,
                };
                let pointer = if let Some(iid) = iid.filter(|_| pointer != 0) {
                    match self.draw.com.unwrap().resolve_interface::<ddraw::ComState>(
                        self.objects,
                        pointer,
                        iid,
                    ) {
                        Ok(pointer) => pointer,
                        Err(_) => return Ok(INVALID),
                    }
                } else {
                    pointer
                };
                self.memory.arg_put_u32(out, pointer)?;
                if pointer != 0 {
                    ddraw::d3d::add_ref(self.draw, self.objects, pointer);
                }
            }
            "SetRenderTarget" => {
                let Some(next) = self.draw.surface_id(self.objects, args[1]) else {
                    return Ok(INVALID_OBJECT);
                };
                let (width, height) = (self.store[next].width, self.store[next].height);
                if args[2] != 0
                    || self.draw.caps(next)[0] & creation::THREE_D_DEVICE == 0
                    || width > 2048
                    || height > 2048
                {
                    return Ok(INVALID);
                }
                self.draw
                    .d3d
                    .as_mut()
                    .unwrap()
                    .flush(self.store, self.memory)?;
                ddraw::d3d::add_ref(self.draw, self.objects, args[1]);
                let device = ddraw::d3d::device_mut(self.draw, self.objects, args[0]).unwrap();
                device.target = args[1];
                device
                    .state
                    .as_mut()
                    .unwrap()
                    .fixed
                    .target_changed(width, height);
                self.draw.release_com(self.objects, self.store, target);
            }
            "SetTexture" => {
                if lost {
                    return Ok(0);
                }
                let Some(previous) = state.texture_for_set(args[1] as usize) else {
                    return Ok(INVALID);
                };
                let next = args[2];
                if next == previous {
                    ddraw::d3d::device_mut(self.draw, self.objects, args[0])
                        .unwrap()
                        .state
                        .as_mut()
                        .unwrap()
                        .set_texture(args[1] as usize, next);
                    return Ok(0);
                }
                if next != 0 {
                    let Some(next_surface) = self.draw.surface_id(self.objects, next) else {
                        return Ok(INVALID);
                    };
                    let surface = &self.store[next_surface];
                    let limit =
                        texture_limit(device.kind, self.store.device().caps().limits.max_image_2d);
                    if self.draw.caps(next_surface)[0] & creation::TEXTURE == 0
                        || surface.width > limit
                        || surface.height > limit
                    {
                        return Ok(INVALID);
                    }
                    ddraw::d3d::add_ref(self.draw, self.objects, next);
                }
                ddraw::d3d::device_mut(self.draw, self.objects, args[0])
                    .unwrap()
                    .state
                    .as_mut()
                    .unwrap()
                    .set_texture(args[1] as usize, next);
                if previous != 0 {
                    self.draw.release_com(self.objects, self.store, previous);
                }
            }
            "PreLoad" => {
                let Some(texture) = self.draw.surface_id(self.objects, args[1]) else {
                    return Ok(INVALID);
                };
                self.render_texture(texture, true)?;
            }
            "ValidateDevice" => {
                self.memory.arg_put_u32(args[1], 1)?;
                if rejects_rgb(device.kind, &state.fixed) {
                    return Ok(UNSUPPORTED_COLOR_ARGUMENT);
                }
            }
            "Clear" => {
                if lost {
                    return Ok(0);
                }
                let view = state.fixed.viewport_rect().clipped(self.store[sid].rect());
                return self.clear(sid, view, args);
            }
            _ => unreachable!(),
        }
        Ok(0)
    }

    pub(super) fn clear(&mut self, sid: u32, view: Rect, args: &[u32]) -> Result<u32, CallError> {
        let flags = args[3];
        if flags == 0 || flags & !7 != 0 {
            return Ok(INVALID);
        }
        let rects = if args[1] == 0 {
            vec![view]
        } else {
            let bytes = self.memory.arg_read(args[2], args[1] as usize * 16)?;
            bytes
                .as_chunks::<16>()
                .0
                .iter()
                .filter_map(|bytes| {
                    let [x, y, right, bottom]: [i32; 4] = bytemuck::pod_read_unaligned(bytes);
                    let x = x.max(view.x as i32);
                    let y = y.max(view.y as i32);
                    let right = right.min((view.x + view.w) as i32);
                    let bottom = bottom.min((view.y + view.h) as i32);
                    (right > x && bottom > y).then(|| Rect {
                        x: x as u32,
                        y: y as u32,
                        w: (right - x) as u32,
                        h: (bottom - y) as u32,
                    })
                })
                .collect()
        };
        let depth = f32::from_bits(args[5]);
        if flags & 2 != 0 && !(0. ..=1.).contains(&depth) {
            return Ok(INVALID);
        }
        let z = self.depth_attachment(sid)?;
        if flags & 4 != 0
            && z.as_ref()
                .is_none_or(|(_, depth)| !depth.format().has_stencil())
        {
            return Ok(0x88760817);
        }
        if flags & 2 != 0 && z.is_none() {
            return Ok(0x88760816);
        }
        let color = (flags & 1 != 0).then(|| {
            let color = fixed::tnl::color(args[4]);
            let masks = self.store.packed_color(sid);
            if masks[0] == 0 {
                color.map(f64::from)
            } else {
                self.store.packed_clear(sid, color)
            }
        });
        let depth = (flags & 2 != 0).then_some(depth);
        let stencil = (flags & 4 != 0).then_some(args[6]);
        let extent = self.store[sid].rect();
        for rect in rects.into_iter().filter(|rect| !rect.empty()) {
            let target = self.target(sid, rect, z.as_ref())?;
            let d3d = self.draw.d3d.as_mut().unwrap();
            d3d.written.insert(sid);
            d3d.written.extend(z.as_ref().map(|&(id, _)| id));
            recording::admit(
                &mut d3d.queue,
                &mut d3d.gpu,
                self.store,
                self.memory,
                Payload::Clear {
                    surface: sid,
                    target,
                    rect,
                    color,
                    depth,
                    stencil,
                },
                Effects {
                    area: rect,
                    target: sid,
                    depth: z.as_ref().map(|&(id, _)| id),
                    view: extent,
                    reads: &[],
                    bytes: 0,
                    state_key: 0,
                },
            )?;
        }
        Ok(0)
    }

    fn depth_attachment(&mut self, sid: u32) -> Result<Option<(u32, gpu::Depth)>, CallError> {
        let Some(id) = self
            .draw
            .attached(sid)
            .into_iter()
            .find(|&id| self.draw.caps(id)[0] & creation::ZBUFFER != 0)
        else {
            return Ok(None);
        };
        let format = self.draw.described[&id].format;
        let format = if word(&format, 4) & 0x4000 != 0 {
            DepthFormat::Depth24Stencil8
        } else if word(&format, 12) == 16 {
            DepthFormat::Unorm16
        } else {
            DepthFormat::Float32
        };
        let d3d = self.draw.d3d.as_mut().unwrap();
        let depth = if let Some(depth) = d3d.depths.get(&id) {
            depth.clone()
        } else {
            let surface = &self.store[id];
            let depth = d3d
                .gpu
                .depth(surface.width, surface.height, format)
                .map_err(gpu_error)?;
            d3d.depths.insert(id, depth.clone());
            depth
        };
        Ok(Some((id, depth)))
    }

    /// The target with CPU writes in `area` uploaded. Execution marks the
    /// cells it writes.
    fn target(
        &mut self,
        id: u32,
        area: Rect,
        depth: Option<&(u32, gpu::Depth)>,
    ) -> Result<Target, CallError> {
        self.store
            .render_access(id, area, Access::Read, self.memory)?
            .with_depth(depth.map(|(_, depth)| depth.clone()))
            .map_err(gpu_error)
    }

    fn render_texture(&mut self, id: u32, mips: bool) -> Result<gpu::Image, CallError> {
        let extent = self.store[id].rect();
        let image = self
            .store
            .gpu_access(id, extent, Access::Read, self.memory)?;
        if !mips || self.draw.caps(id)[0] & creation::MIPMAP == 0 {
            return Ok(image);
        }
        let mut levels = smallvec::SmallVec::<[_; 12]>::new();
        levels.push((id, extent, image));
        let mut next = id;
        while let Some(&level) = self.draw.complex.get(&next).and_then(|chain| chain.first()) {
            let rect = self.store[level].rect();
            let image = self
                .store
                .gpu_access(level, rect, Access::Read, self.memory)?;
            levels.push((level, rect, image));
            next = level;
        }
        // Each level's storage, version and writes on either side, after
        // the reads above uploaded its CPU changes.
        let key = hash(
            levels
                .iter()
                .map(|(level, _, image)| {
                    (
                        *level,
                        image.id(),
                        self.store[*level].version,
                        self.store.write_stamp(*level),
                    )
                })
                .collect::<smallvec::SmallVec<[_; 12]>>(),
        );
        let d3d = self.draw.d3d.as_mut().unwrap();
        if let Some((tag, image)) = d3d.mips.get(&id)
            && *tag == key
        {
            return Ok(image.clone());
        }
        let gpu = &mut d3d.gpu;
        let image = gpu
            .target_levels(extent.w, extent.h, levels.len() as u32)
            .map_err(gpu_error)?
            .image;
        for (level, (_, rect, source)) in levels.into_iter().enumerate() {
            let destination = image.mip_level(level as u32).map_err(gpu_error)?;
            gpu.copy(&source, rect, &destination, 0, 0)
                .map_err(gpu_error)?;
        }
        d3d.mips.insert(id, (key, image.clone()));
        Ok(image)
    }

    pub(super) fn draw_primitive(
        &mut self,
        face: Face,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let Some(com) = self.draw.com else {
            return Ok(INVALID);
        };
        let Some(device) = ddraw::d3d::device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let sid = self.draw.surface_id(self.objects, device.target).unwrap();
        if self.draw.lost.contains_key(&sid) {
            return Ok(0);
        }
        if face != Face::Device7 && device.current_viewport != 0 {
            let hr = self.select_viewport(args[0], device.current_viewport)?;
            if hr != 0 {
                return Ok(hr);
            }
        }
        let strided = method.ends_with("Strided");
        let vb = method.ends_with("VB");
        let old_indexed_vb = face == Face::Device3 && method == "DrawIndexedPrimitiveVB";
        let mut count = if old_indexed_vb { 0 } else { args[4] };
        if count == 0 && !old_indexed_vb {
            return Ok(0);
        }
        let (fvf, data) = if vb {
            let Some(Object::VertexBuffer {
                desc, data, locked, ..
            }) = com.get::<Object>(self.objects, args[2])
            else {
                return Ok(INVALID);
            };
            if *locked {
                return Ok(VB_LOCKED);
            }
            if old_indexed_vb {
                count = desc.dwNumVertices;
            }
            let start = if old_indexed_vb { 0 } else { args[3] };
            if start
                .checked_add(count)
                .is_none_or(|n| n > desc.dwNumVertices)
            {
                return Ok(INVALID);
            }
            let layout = vertex::layout(desc.dwFVF).unwrap();
            (desc.dwFVF, data + start * layout.size)
        } else if face == Face::Device2 {
            let Some(fvf) = vertex::legacy_fvf(args[2]) else {
                return Ok(INVALID);
            };
            (fvf, args[3])
        } else {
            (args[2], args[3])
        };
        let layout = match vertex::layout(fvf) {
            Ok(layout) => layout,
            Err(hr) => return Ok(hr),
        };
        let indexed = method.starts_with("DrawIndexed");
        let indices = if indexed {
            let index_arg = if old_indexed_vb { 3 } else { 5 };
            let bytes = self
                .memory
                .arg_read(args[index_arg], args[index_arg + 1] as usize * 2)?;
            let values = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|word| u32::from(u16::from_le_bytes(*word)))
                .collect::<Vec<_>>();
            if values.iter().any(|&i| i >= count) {
                return Ok(INVALID);
            }
            Some(values)
        } else {
            None
        };
        let Some(size) = count.checked_mul(layout.size) else {
            return Ok(INVALID);
        };
        let bytes = if strided {
            let descriptor = read(self.memory, data)?;
            interleave(
                self.memory,
                descriptor,
                layout,
                count,
                (fvf & 0xe).saturating_sub(4) / 2,
            )?
        } else {
            self.memory.arg_read(data, size as usize)?.into_owned()
        };
        self.emit_primitive(
            args[0],
            super::object::PrimitiveBatch {
                vertices: bytes,
                indices,
                flags: args[if indexed && !old_indexed_vb { 7 } else { 5 }],
                primitive: args[1],
                fvf,
            },
            None,
        )
    }

    pub(super) fn emit_primitive(
        &mut self,
        this: u32,
        batch: super::object::PrimitiveBatch,
        execute: Option<ExecuteDraw<'_>>,
    ) -> Result<u32, CallError> {
        let super::object::PrimitiveBatch {
            mut vertices,
            mut indices,
            flags,
            primitive,
            fvf,
        } = batch;
        let Some(device) = ddraw::d3d::device(self.draw, self.objects, this) else {
            return Ok(INVALID);
        };
        let sid = self.draw.surface_id(self.objects, device.target).unwrap();
        if self.draw.lost.contains_key(&sid) {
            return Ok(0);
        }
        let state = device.state.as_ref().unwrap();
        let fixed = state.draw_state(flags & 0x10 == 0);
        let textures = state.textures;
        let kind = device.kind;
        let legacy_device = device.face != Face::Device7;
        let layout = vertex::layout(fvf).unwrap();
        let mut count = vertices.len() as u32 / layout.size;
        let entries = indices.as_ref().map_or(count as usize, Vec::len);
        if entries == 0 {
            return Ok(0);
        }
        if indices
            .as_ref()
            .is_some_and(|values| values.iter().any(|&i| i >= count))
        {
            return Ok(INVALID);
        }
        let mut topology = match vertex::primitive(primitive, entries) {
            Ok(topology) => topology,
            Err(hr) => return Ok(hr),
        };
        let stats = match primitive {
            1 => (2, entries),
            2 => (1, entries / 2),
            3 => (1, entries - 1),
            4 => (0, entries / 3),
            5 | 6 => (0, entries - 2),
            _ => unreachable!(),
        };
        let weights = (fvf & 0xe).saturating_sub(4) / 2;
        if weights != 0 && kind != caps::Device::TnlHal {
            return Ok(0x80004005);
        }
        let blend_weights = weights.min(fixed.render[&Render::VertexBlend]) as u8;
        if primitive == 6 {
            let vertex = |n| indices.as_ref().map_or(n, |i| i[n as usize]);
            indices = Some(vertex::expand_fan(vertex, entries as u32));
        }
        let mut bytes = std::mem::take(&mut vertices);
        let projection = Projection {
            fixed: &fixed,
            layout,
            blend_weights,
        };
        let fill = fixed.render[&Render::FillMode];
        let wrap = std::array::from_fn(|n| fixed.render[&(128 + n as i32)]);
        let wraps = wrap
            .iter()
            .zip(layout.texcoords)
            .any(|(&mask, coord)| mask != 0 && coord.is_some());
        let triangles = matches!(topology, Topology::Triangles | Topology::TriangleStrip);
        if (triangles && fill != 3) || (wraps && topology != Topology::Points) {
            let vertex = |n| indices.as_ref().map_or(n, |indices| indices[n as usize]);
            let spans = indices
                .as_ref()
                .map_or(count, |indices| indices.len() as u32);
            let mut expanded = Vec::new();
            if triangles {
                let triangle = |i: u32| {
                    if topology != Topology::TriangleStrip {
                        [vertex(i * 3), vertex(i * 3 + 1), vertex(i * 3 + 2)]
                    } else if i.is_multiple_of(2) {
                        [vertex(i), vertex(i + 1), vertex(i + 2)]
                    } else {
                        [vertex(i), vertex(i + 2), vertex(i + 1)]
                    }
                };
                let triangles = if topology == Topology::TriangleStrip {
                    spans - 2
                } else {
                    spans / 3
                };
                let mut triangle_bytes = Vec::with_capacity(layout.size as usize * 3);
                for i in 0..triangles {
                    let vertices = triangle(i);
                    if fill != 3 && projection.culled(&bytes, vertices) {
                        continue;
                    }
                    triangle_bytes.clear();
                    for vertex in vertices {
                        append_vertex(&mut triangle_bytes, &bytes, vertex, layout.size);
                    }
                    wrap_vertices(&mut triangle_bytes, layout, &wrap);
                    if fill != 3 && fixed.render[&Render::ShadeMode] == 1 {
                        for offset in [layout.color, layout.specular.map(|(_, offset)| offset)]
                            .into_iter()
                            .flatten()
                        {
                            for i in 1..3 {
                                let start = offset as usize;
                                triangle_bytes.copy_within(
                                    start..start + 4,
                                    (i * layout.size + offset) as usize,
                                );
                            }
                        }
                    }
                    if fill == 2 {
                        for (edge, pair) in [[0, 1], [1, 2], [2, 0]].into_iter().enumerate() {
                            if execute
                                .as_ref()
                                .and_then(|draw| draw.edges)
                                .is_some_and(|flags| flags[i as usize] & (0x100 << edge) == 0)
                            {
                                continue;
                            }
                            for vertex in pair {
                                append_vertex(&mut expanded, &triangle_bytes, vertex, layout.size);
                            }
                        }
                    } else {
                        expanded.extend_from_slice(&triangle_bytes);
                    }
                }
                topology = match fill {
                    1 => Topology::Points,
                    2 => Topology::Lines,
                    _ => Topology::Triangles,
                };
            } else {
                for entry in 0..spans {
                    append_vertex(&mut expanded, &bytes, vertex(entry), layout.size);
                }
                if topology == Topology::Lines {
                    for line in expanded.chunks_exact_mut(layout.size as usize * 2) {
                        wrap_vertices(line, layout, &wrap);
                    }
                } else {
                    wrap_strip(&mut expanded, layout, &wrap);
                }
            }
            bytes = expanded;
            indices = None;
            count = bytes.len() as u32 / layout.size;
            if count == 0 {
                return Ok(0);
            }
        }
        let pattern = if kind == caps::Device::Rgb
            && matches!(topology, Topology::LineStrip | Topology::Lines)
        {
            fixed.render[&Render::LinePattern]
        } else {
            0
        };
        let mut stipple = Vec::new();
        let stippled_strip = topology == Topology::LineStrip && pattern != 0;
        if pattern != 0 {
            let entries = indices
                .as_ref()
                .map_or(count, |entries| entries.len() as u32);
            let vertex = |entry| {
                indices
                    .as_ref()
                    .map_or(entry, |indices| indices[entry as usize])
            };
            let mut expanded = Vec::new();
            for start in (0..entries - 1).step_by(if stippled_strip { 1 } else { 2 }) {
                let [a, b] = [vertex(start), vertex(start + 1)];
                let line = match [a, b].map(|index| projection.screen(&bytes, index)) {
                    [Some([x, y, _]), Some([end_x, end_y, _])] => {
                        let dx = end_x - x;
                        let dy = end_y - y;
                        let step = if dx.abs() >= dy.abs() {
                            [dx.signum(), 0.]
                        } else {
                            [0., dy.signum()]
                        };
                        [x.round(), y.round(), step[0], step[1]]
                    }
                    _ => [0.; 4],
                };
                for index in [a, b] {
                    append_vertex(&mut expanded, &bytes, index, layout.size);
                    stipple.push(line);
                }
            }
            bytes = expanded;
            indices = None;
            count = stipple.len() as u32;
            topology = Topology::Lines;
        }
        let extent = self.store[sid].rect();
        let mut rules = rules(kind);
        rules.round_points |= execute.is_some();
        let view = fixed.viewport_rect().clipped(extent);
        let bounds = draw_bounds(&fixed, layout, topology, &bytes).clipped(view);
        if bounds.empty() {
            return Ok(0);
        }
        let z = self.depth_attachment(sid)?;
        let vertex =
            VertexProgram::new(layout, &fixed, topology).map_err(|error| format!("{error:?}"))?;
        let mut inputs = fixed::inputs(layout);
        if blend_weights != 0 {
            inputs.push(fixed::Input {
                stream: 0,
                offset: 12,
                register: 12,
                format: [
                    AttributeFormat::Float1,
                    AttributeFormat::Float2,
                    AttributeFormat::Float3,
                ][blend_weights as usize - 1],
            });
        }
        let mut active = 0;
        if !rejects_rgb(kind, &fixed) {
            for (n, &texture) in textures.iter().enumerate() {
                let stage = if n == 0 {
                    &fixed.stage
                } else {
                    &fixed.stage_slots[n]
                };
                if stage[&Stage::ColorOp] == 1 || texture == 0 && stage[&Stage::ColorArg1] & 15 == 2
                {
                    break;
                }
                active += 1;
            }
        }
        let mut reads = Vec::new();
        let mut bindings = Vec::new();
        let mut parameters = [[0; 4]; 40];
        parameters[14][1] = pattern;
        let mut texture_words = [[0; 4]; 16];
        // CPU writes change a texture's surface version and storage stamp,
        // not a recorded version.
        let mut texture_versions = [0u128; 8];
        for (n, &texture) in textures[..active].iter().enumerate() {
            let mut sampler = fixed
                .program_sampler(n)
                .map_err(|error| format!("{error:?}"))?;
            let stage = if n == 0 {
                &fixed.stage
            } else {
                &fixed.stage_slots[n]
            };
            let registers = if n == 0 {
                &fixed.sampler
            } else {
                &fixed.sampler_slots[n]
            };
            let wrap = wrap[(stage[&Stage::CoordinateIndex] & 7) as usize];
            if wrap & 1 != 0 {
                sampler.u = gpu::Address::Repeat;
            }
            if wrap & 2 != 0 {
                sampler.v = gpu::Address::Repeat;
            }
            parameters[16 + n] = gpu::sampler_parameters(sampler);
            parameters[24 + n] = [
                rules.texture_op(stage[&Stage::ColorOp]),
                stage[&Stage::ColorArg1] | stage[&Stage::ColorArg2] << 8,
                rules.texture_op(stage[&Stage::AlphaOp]),
                stage[&Stage::AlphaArg1] | stage[&Stage::AlphaArg2] << 8,
            ];
            let image = if let Some(texture) = self.draw.surface_id(self.objects, texture) {
                if texture == sid || self.store[texture].locked.is_some() {
                    return Ok(INVALID);
                }
                let texture_key = recording::key(texture, 0);
                reads.push(texture_key);
                let format = self.draw.described[&texture].format;
                let masks = match word(&format, 4) {
                    2 => [0, 0, 0, 0xff],
                    0x20000 => [0xff, 0xff, 0xff, 0],
                    0x20001 => [0xff, 0xff, 0xff, 0xff00],
                    _ => std::array::from_fn(|i| word(&format, 16 + i * 4)),
                };
                texture_words[n * 2] = masks;
                let key = (fixed.render[&Render::ColorKeyEnable] != 0
                    || fixed.render[&Render::ColorKeyBlendEnable] != 0)
                    .then(|| self.draw.color_keys.get(&(texture, 8)).map(|&(low, _)| low))
                    .flatten();
                texture_words[n * 2 + 1] = [
                    key.unwrap_or(0),
                    rules
                        .color_key_mask
                        .unwrap_or_else(|| masks.into_iter().fold(0, |a, b| a | b)),
                    2 | u32::from(key.is_some())
                        | (u32::from(self.store.packed_color(texture)[0] != 0) << 2)
                        | (u32::from(
                            key.is_some() && fixed.render[&Render::ColorKeyBlendEnable] != 0,
                        ) << 3),
                    registers[&Sampler::MipLodBias],
                ];
                let mut levels = smallvec::SmallVec::<[u32; 12]>::from_elem(texture, 1);
                if sampler.mip.is_some() && self.draw.caps(texture)[0] & creation::MIPMAP != 0 {
                    while let Some(&level) = self
                        .draw
                        .complex
                        .get(levels.last().unwrap())
                        .and_then(|chain| chain.first())
                    {
                        levels.push(level);
                    }
                    // Resolve prior writes before assembling the mip-level views.
                    let d3d = self.draw.d3d.as_mut().unwrap();
                    if levels
                        .iter()
                        .any(|&level| d3d.queue.observes(recording::key(level, 0), true))
                    {
                        d3d.flush(self.store, self.memory)?;
                    }
                }
                texture_versions[n] = hash(
                    levels
                        .iter()
                        .map(|&level| (self.store[level].version, self.store.cpu_stamp(level)))
                        .collect::<smallvec::SmallVec<[_; 12]>>(),
                );
                Some(self.render_texture(texture, sampler.mip.is_some())?)
            } else {
                None
            };
            bindings.push(ProgramTexture { image, sampler });
        }
        let mut lowered = fixed::lower(
            &fixed,
            &fixed::Layout {
                topology,
                vertex: Some(vertex),
                extent: (extent.w, extent.h),
                depth: z.is_some(),
                stencil: z
                    .as_ref()
                    .is_some_and(|(_, depth)| depth.format().has_stencil()),
                texture: None,
                stage_zero_bound: false,
            },
            rules,
        )
        .map_err(|error| format!("{error:?}"))?;
        let viewport = lowered.state.viewport;
        parameters[0] = [viewport.width, viewport.height, viewport.x, viewport.y].map(f32::to_bits);
        parameters[1][2] = lowered.state.alpha_test as u32;
        parameters[1][3] = lowered.state.alpha_reference.to_bits();
        let clipping = fixed.render[&Render::Clipping] != 0 && flags & 4 == 0;
        if clipping {
            parameters[3][0] = fixed.render[&Render::ClipPlaneEnable];
        }
        for (out, plane) in parameters[4..10].iter_mut().zip(fixed.clip_planes) {
            *out = plane.map(f32::to_bits);
        }
        parameters[10] = fixed::tnl::color(fixed.render[&Render::FogColor]).map(f32::to_bits);
        parameters[11] = [
            Render::FogStart,
            Render::FogEnd,
            Render::FogDensity,
            Render::PointSize,
        ]
        .map(|r| fixed.render[&r]);
        parameters[36] = fixed::tnl::color(fixed.render[&Render::TextureFactor]).map(f32::to_bits);
        parameters[38] = lowered.raster_parameters;
        let masks = self.store.packed_color(sid);
        if lowered.state.blend.is_none() {
            parameters[39] = masks;
        } else if masks[0] != 0 && masks[3] == 0 {
            lowered.state.color_write &= 7;
        }
        let entries = indices
            .as_ref()
            .map_or(count, |indices| indices.len() as u32);
        let vertex_index = |entry| {
            indices
                .as_ref()
                .map_or(entry, |indices| indices[entry as usize])
        };
        let line_ranges = if lowered.state.last_pixel && fixed.render[&Render::LastPixel] != 0 {
            let pixel = |entry| {
                projection
                    .screen(&bytes, vertex_index(entry))
                    .map(|[x, y, _]| [x.round(), y.round()])
            };
            if stippled_strip {
                vec![0..entries, entries - 1..entries]
            } else {
                gpu::Inclusive::ranges(topology, entries, |a, b| pixel(a) == pixel(b))
            }
        } else {
            Vec::new()
        };
        let update_extents = fixed.render[&Render::Extents] != 0 && flags & 8 == 0;
        let clip_extents = (kind != caps::Device::TnlHal && (!legacy_device || update_extents))
            .then(|| projection.extents(&bytes, (0..entries).map(vertex_index)));
        let legacy = Legacy {
            stages: active as u8,
            point_rounding: (topology == Topology::Points).then_some(rules.round_points),
            blend_weights,
            perspective: fixed.render[&Render::TexturePerspective] != 0,
            clipping,
            line_pattern: !stipple.is_empty(),
        };
        if legacy.line_pattern {
            inputs.push(fixed::Input {
                stream: 1,
                offset: 0,
                register: 13,
                format: AttributeFormat::Float4,
            });
        }
        let features = (
            parameters[3][0],
            fixed.render[&Render::FogEnable] != 0,
            lowered.program.features,
        );
        let key = hash((vertex, features, legacy));
        let program = self.draw.d3d.as_mut().unwrap().programs.get(&key).cloned();
        let (vertex_constants, usage) = shader_constants(&fixed, &vertex, legacy)?;
        let mut texture_use = ConstantUsage::default();
        for n in 0..active * 2 {
            texture_use.set(n);
        }
        let program = if let Some(program) = program {
            program
        } else {
            let source = fixed::program::build_legacy(&vertex, &inputs, features, legacy)
                .map_err(|hr| format!("fixed program admission {hr:#x}"))?;
            let mut program = Program::new(source.into(), vec![false; active]);
            program.constants = [usage, texture_use];
            gpu::validate_program(&program).map_err(gpu_error)?;
            self.draw
                .d3d
                .as_mut()
                .unwrap()
                .programs
                .insert(key, program.clone());
            program
        };
        let stipple = bytemuck::cast_slice::<_, u8>(&stipple);
        let contents = hash((
            &bytes,
            &indices,
            fixed.key(),
            parameters,
            texture_words,
            texture_versions,
            stipple,
        ));
        let draw_bytes = bytes.len() + stipple.len();
        let stream = VertexStream {
            instance: false,
            bytes: VertexBytes::from(bytes),
            stride: layout.size,
            attributes: inputs
                .iter()
                .filter(|input| input.stream == 0)
                .map(|input| Attribute {
                    location: input.register,
                    offset: input.offset,
                    format: input.format,
                })
                .collect(),
        };
        let mut streams = vec![stream];
        if legacy.line_pattern {
            streams.push(VertexStream {
                instance: false,
                bytes: VertexBytes::from(stipple.to_vec()),
                stride: 16,
                attributes: vec![Attribute {
                    location: 13,
                    offset: 0,
                    format: AttributeFormat::Float4,
                }],
            });
        }
        let draw = Arc::new(ProgrammableDraw {
            instance_count: 1,
            program,
            streams,
            indices: indices.map(IndexBytes::from),
            vertex_count: count,
            vertex_constants,
            pixel_constants: ConstantSnapshot::capture(texture_use, |n| texture_words[n]),
            textures: bindings,
            parameters,
        });
        let mut target = self.target(sid, bounds, z.as_ref())?;
        if masks[0] != 0 && masks[3] == 0 {
            target = target.with_alpha_mode(gpu::AlphaMode::Opaque);
        }
        let d3d = self.draw.d3d.as_mut().unwrap();
        d3d.written.insert(sid);
        d3d.written.extend(z.as_ref().map(|&(id, _)| id));
        recording::admit(
            &mut d3d.queue,
            &mut d3d.gpu,
            self.store,
            self.memory,
            Payload::Draw {
                surface: sid,
                area: bounds,
                target,
                draw,
                state: lowered.state,
                line_ranges: (!line_ranges.is_empty()).then(|| line_ranges.into()),
            },
            Effects {
                area: bounds,
                target: sid,
                depth: z.as_ref().map(|&(id, _)| id),
                view: extent,
                reads: &reads,
                bytes: draw_bytes,
                state_key: contents,
            },
        )?;
        if self.draw.text.on() {
            // Each texture the draw samples, with no map a vertex list states.
            for read in &reads {
                let texture = &self.store[read.0.0 as u32];
                self.draw.text.push(crate::text::SurfaceMove {
                    kind: adv_machine::text_records::MoveKind::Draw,
                    destination: Some(crate::text::surface(&self.store[sid])),
                    rect: bounds,
                    source: crate::text::surface(texture),
                    map: None,
                });
            }
        }
        if legacy_device && execute.is_none() {
            let dev = ddraw::d3d::device_mut(self.draw, self.objects, this).unwrap();
            dev.legacy.stats[stats.0] += stats.1 as u32;
            if !layout.transformed {
                dev.legacy.stats[4] += count;
            }
        }
        if let Some((extents, union, intersection)) = clip_extents {
            let state = ddraw::d3d::device_mut(self.draw, self.objects, this)
                .unwrap()
                .state
                .as_mut()
                .unwrap();
            if legacy_device {
                state.clip_status[0] |= 2;
                state.clip_status[2..].copy_from_slice(&extents.map(f32::to_bits));
            } else {
                state.clip_status[1] |= union;
                state.clip_status[1] &= !0x00ff_f000 | (intersection << 12);
                if update_extents {
                    for (i, (stored, value)) in
                        state.clip_status[2..6].iter_mut().zip(extents).enumerate()
                    {
                        let old = f32::from_bits(*stored);
                        *stored = if i % 2 == 0 {
                            old.min(value)
                        } else {
                            old.max(value)
                        }
                        .to_bits();
                    }
                }
            }
        }
        Ok(0)
    }
}

fn rules(kind: caps::Device) -> Rules {
    let rgb = kind == caps::Device::Rgb;
    Rules {
        color_key_mask: rgb.then_some(u32::MAX),
        uncapped_op: rgb.then_some((10, 7)),
        integer_zbias: !rgb,
        round_points: rgb,
        line_last_pixel: Some(rgb),
    }
}

fn rejects_rgb(kind: caps::Device, state: &fixed::State) -> bool {
    kind == caps::Device::Rgb
        && (state.stage_slots[1][&Stage::ColorOp] != 1 || state.stage[&Stage::ColorArg1] & 15 == 3)
}

fn texture_limit(device: caps::Device, limit: u32) -> u32 {
    limit.min(if device == caps::Device::Rgb {
        1024
    } else {
        16384
    })
}

pub(super) fn interleave(
    memory: &dyn SurfaceMemory,
    desc: D3DDRAWPRIMITIVESTRIDEDDATA,
    layout: fixed::VertexLayout,
    count: u32,
    weights: u32,
) -> Result<Vec<u8>, CallError> {
    let size = layout.size as usize;
    let mut bytes = vec![0; size * count as usize];
    let mut copy = |stream: D3DDP_PTRSTRIDE, offset: u32, length: u32| -> Result<(), CallError> {
        let span = (count - 1) as usize * stream.dwStride as usize + length as usize;
        let source = memory.arg_read(stream.lpvData, span)?;
        for (i, row) in bytes.chunks_exact_mut(size).enumerate() {
            let from = i * stream.dwStride as usize;
            row[offset as usize..][..length as usize]
                .copy_from_slice(&source[from..][..length as usize]);
        }
        Ok(())
    };
    copy(
        desc.position,
        0,
        if layout.transformed {
            16
        } else {
            12 + weights * 4
        },
    )?;
    if let Some((_, offset)) = layout.normal {
        copy(desc.normal, offset, 12)?;
    }
    if let Some(offset) = layout.color {
        copy(desc.diffuse, offset, 4)?;
    }
    if let Some((_, offset)) = layout.specular {
        copy(desc.specular, offset, 4)?;
    }
    for (n, attribute) in layout.texcoords.into_iter().enumerate() {
        if let Some((_, offset, count)) = attribute {
            copy(desc.textureCoords[n], offset, u32::from(count) * 4)?;
        }
    }
    Ok(bytes)
}

fn shader_constants(
    fixed: &fixed::State,
    vertex: &VertexProgram,
    legacy: Legacy,
) -> Result<(ConstantSnapshot, ConstantUsage), CallError> {
    let (base, mut usage) = vertex
        .constants(fixed)
        .map_err(|error| format!("{error:?}"))?;
    for stage in 0..legacy.stages as usize {
        for row in 0..4 {
            usage.set(70 + stage * 4 + row);
        }
        usage.set(114 + stage);
    }
    for i in 0..usize::from(legacy.blend_weights) * 4 {
        usage.set(102 + i);
    }
    let snapshot = ConstantSnapshot::capture(usage, |n| match n {
        70..=101 => {
            fixed.transform(6 + (n - 70) / 4).as_chunks::<4>().0[(n - 70) % 4].map(f32::to_bits)
        }
        102..=113 => {
            fixed.transform(3 + (n - 102) / 4).as_chunks::<4>().0[(n - 102) % 4].map(f32::to_bits)
        }
        114..=121 => {
            let stage = if n == 114 {
                &fixed.stage
            } else {
                &fixed.stage_slots[n - 114]
            };
            [
                stage[&Stage::CoordinateIndex],
                stage[&Stage::TransformFlags],
                0,
                0,
            ]
        }
        _ => base.register(n),
    });
    Ok((snapshot, usage))
}

struct Projection<'a> {
    fixed: &'a fixed::State,
    layout: fixed::VertexLayout,
    blend_weights: u8,
}

impl Projection<'_> {
    fn screen(&self, vertices: &[u8], index: u32) -> Option<[f32; 3]> {
        let offset = (index * self.layout.size) as usize;
        if self.layout.transformed {
            let [x, y, z, rhw]: [f32; 4] =
                bytemuck::pod_read_unaligned(&vertices[offset..offset + 16]);
            return (rhw != 0. && [x, y, z, rhw].into_iter().all(f32::is_finite))
                .then_some([x, y, z]);
        }
        let [x, y, z] = bytemuck::pod_read_unaligned(&vertices[offset..offset + 12]);
        let input = [x, y, z, 1.];
        let mut world = [0.; 4];
        let mut remainder = 1.;
        for i in 0..usize::from(self.blend_weights) {
            let weight = f32::from_bits(word(vertices, offset + 12 + i * 4));
            let matrix = if i == 0 { 0 } else { i + 2 };
            for (out, v) in world
                .iter_mut()
                .zip(fixed::tnl::row(input, self.fixed.transform(matrix)))
            {
                *out += v * weight;
            }
            remainder -= weight;
        }
        let matrix = if self.blend_weights == 0 {
            0
        } else {
            usize::from(self.blend_weights) + 2
        };
        for (out, v) in world
            .iter_mut()
            .zip(fixed::tnl::row(input, self.fixed.transform(matrix)))
        {
            *out += v * remainder;
        }
        let eye = fixed::tnl::row(world, self.fixed.transform(1));
        let [x, y, z, w] = fixed::tnl::row(eye, self.fixed.transform(2));
        let view = self.fixed.viewport_rect();
        (w != 0. && [x, y, z, w].into_iter().all(f32::is_finite)).then(|| {
            [
                view.x as f32 + (x / w + 1.) * view.w as f32 * 0.5,
                view.y as f32 + (1. - y / w) * view.h as f32 * 0.5,
                z / w,
            ]
        })
    }

    fn culled(&self, bytes: &[u8], vertices: [u32; 3]) -> bool {
        let [Some(a), Some(b), Some(c)] = vertices.map(|index| self.screen(bytes, index)) else {
            return false;
        };
        let winding = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        winding == 0.
            || match self.fixed.render[&Render::CullMode] {
                2 => winding > 0.,
                3 => winding < 0.,
                _ => false,
            }
    }

    fn extents(&self, bytes: &[u8], indices: impl Iterator<Item = u32>) -> ([f32; 6], u32, u32) {
        let mut bounds = [
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let viewport = self.fixed.viewport_rect();
        let mut union = 0;
        let mut intersection = 0xfff;
        for position @ [x, y, z] in indices.filter_map(|index| self.screen(bytes, index)) {
            for (axis, value) in position.into_iter().enumerate() {
                bounds[axis * 2] = bounds[axis * 2].min(value);
                bounds[axis * 2 + 1] = bounds[axis * 2 + 1].max(value);
            }
            let code = u32::from(x < viewport.x as f32)
                | (u32::from(x > (viewport.x + viewport.w) as f32) << 1)
                | (u32::from(y < viewport.y as f32) << 2)
                | (u32::from(y > (viewport.y + viewport.h) as f32) << 3)
                | (u32::from(z < 0.) << 4)
                | (u32::from(z > 1.) << 5);
            union |= code;
            intersection &= code;
        }
        (bounds, union, intersection)
    }
}

fn append_vertex(out: &mut Vec<u8>, bytes: &[u8], index: u32, stride: u32) {
    out.extend_from_slice(&bytes[(index * stride) as usize..((index + 1) * stride) as usize]);
}

fn wrap_vertices(bytes: &mut [u8], layout: fixed::VertexLayout, wrap: &[u32; 8]) {
    let mut vertices = bytes.chunks_exact_mut(layout.size as usize);
    let anchor = vertices.next().unwrap();
    for vertex in vertices {
        wrap_vertex(anchor, vertex, layout, wrap);
    }
}

fn wrap_strip(bytes: &mut [u8], layout: fixed::VertexLayout, wrap: &[u32; 8]) {
    let mut vertices = bytes.chunks_exact_mut(layout.size as usize);
    let mut previous = vertices.next().unwrap();
    for vertex in vertices {
        wrap_vertex(previous, vertex, layout, wrap);
        previous = vertex;
    }
}

fn wrap_vertex(anchor: &[u8], vertex: &mut [u8], layout: fixed::VertexLayout, wrap: &[u32; 8]) {
    for (mask, coord) in wrap.iter().zip(layout.texcoords) {
        if let Some((_, offset, dimensions)) = coord {
            for axis in (0..dimensions).filter(|axis| mask & (1 << axis) != 0) {
                let start = offset as usize + usize::from(axis) * 4;
                let origin = f32::from_bits(word(anchor, start));
                let value = f32::from_bits(word(vertex, start));
                let delta = origin - value;
                if delta.abs() > 0.5 {
                    vertex[start..start + 4]
                        .copy_from_slice(&(value + delta.round()).to_le_bytes());
                }
            }
        }
    }
}

fn draw_bounds(
    fixed: &fixed::State,
    layout: fixed::VertexLayout,
    topology: Topology,
    bytes: &[u8],
) -> Rect {
    let view = fixed.viewport_rect();
    if !layout.transformed {
        return view;
    }
    let mut lower = [f32::INFINITY; 2];
    let mut upper = [f32::NEG_INFINITY; 2];
    for vertex in bytes.chunks_exact(layout.size as usize) {
        let xy: [f32; 2] = bytemuck::pod_read_unaligned(&vertex[..8]);
        for i in 0..2 {
            lower[i] = lower[i].min(xy[i]);
            upper[i] = upper[i].max(xy[i]);
        }
    }
    let pad = if matches!(
        topology,
        Topology::Lines | Topology::LineStrip | Topology::Points
    ) {
        2.
    } else {
        1.
    };
    let x = (lower[0].floor() - pad).max(view.x as f32) as u32;
    let y = (lower[1].floor() - pad).max(view.y as f32) as u32;
    let right = (upper[0].ceil() + pad).min((view.x + view.w) as f32) as u32;
    let bottom = (upper[1].ceil() + pad).min((view.y + view.h) as f32) as u32;
    Rect {
        x,
        y,
        w: right.saturating_sub(x),
        h: bottom.saturating_sub(y),
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
