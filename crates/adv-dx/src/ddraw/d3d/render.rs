//! Device7 admission, surface bindings and recorded raster work.
use super::{Calls, INVALID, OUT_OF_MEMORY, Object, caps, creation, read, write};
use crate::{
    argument::{Arguments, CallError},
    ddraw::{
        self,
        d3d::{
            Face,
            desc::vertex,
            recording::{self, Boundary, Effects, Payload},
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

impl Calls<'_> {
    pub(super) fn create_vertex_buffer(&mut self, args: &[u32]) -> Result<u32, CallError> {
        self.memory.arg_put_u32(args[2], 0)?;
        if args[3] != 0 {
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
            &[Face::VertexBuffer7],
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

    pub(super) fn vertex_buffer(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
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
                if *locked {
                    return Ok(VB_LOCKED);
                }
                self.memory.arg_put_u32(args[2], *data)?;
                if args[3] != 0 {
                    self.memory.arg_put_u32(args[3], *size)?;
                }
                *locked = true;
            }
            "Unlock" => {
                if !*locked {
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

    pub(super) fn render_method(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let Some(Object::Device(device)) = self
            .draw
            .com
            .and_then(|com| com.get::<Object>(self.objects, args[0]))
        else {
            return Ok(INVALID);
        };
        let Some(state) = &device.state else {
            return Ok(INVALID);
        };
        let target = device.target;
        let sid = self.draw.surface_id(self.objects, target).unwrap();
        let lost = self.draw.lost.contains(&sid);
        match method {
            "BeginScene" => {
                if state.scene {
                    return Ok(SCENE_IN);
                }
                if lost {
                    return Ok(SCENE_BEGIN_FAILED);
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
                let d3d = self.draw.d3d.as_mut().unwrap();
                recording::flush(&mut d3d.queue, &mut d3d.gpu, Boundary::EndScene)
                    .map_err(gpu_error)?;
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
                self.memory.arg_put_u32(out, pointer)?;
                if pointer != 0 {
                    ddraw::d3d::add_ref(self.draw, self.objects, pointer);
                }
            }
            "SetRenderTarget" => {
                let Some(next) = self.draw.surface_id(self.objects, args[1]) else {
                    return Ok(INVALID_OBJECT);
                };
                let surface = &self.store[next];
                if args[2] != 0
                    || self.draw.caps(next)[0] & creation::THREE_D_DEVICE == 0
                    || surface.width > 2048
                    || surface.height > 2048
                {
                    return Ok(INVALID);
                }
                self.draw.d3d.as_mut().unwrap().flush()?;
                ddraw::d3d::add_ref(self.draw, self.objects, args[1]);
                let device = ddraw::d3d::device_mut(self.draw, self.objects, args[0]).unwrap();
                device.target = args[1];
                device
                    .state
                    .as_mut()
                    .unwrap()
                    .fixed
                    .target_changed(surface.width, surface.height);
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
                if rejects_rgb(device.kind, &state.fixed) {
                    return Ok(UNSUPPORTED_COLOR_ARGUMENT);
                }
                self.memory.arg_put_u32(args[1], 1)?;
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

    fn clear(&mut self, sid: u32, view: Rect, args: &[u32]) -> Result<u32, CallError> {
        let flags = args[3];
        if flags == 0 || flags & !7 != 0 {
            return Ok(INVALID);
        }
        let rects = if args[1] == 0 {
            vec![view]
        } else {
            let bytes = self.memory.arg_read(args[2], args[1] as usize * 16)?;
            bytes
                .chunks_exact(16)
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
        let masks = target_masks(self.draw, self.store, sid);
        let color = (flags & 1 != 0).then(|| pack_clear(fixed::tnl::color(args[4]), masks));
        let depth = (flags & 2 != 0).then_some(depth);
        let stencil = (flags & 4 != 0).then_some(args[6]);
        let extent = self.store[sid].rect();
        for rect in rects.into_iter().filter(|rect| !rect.empty()) {
            let target = self.target(sid, rect, color.is_some(), z.as_ref())?;
            let d3d = self.draw.d3d.as_mut().unwrap();
            recording::admit(
                &mut d3d.queue,
                &mut d3d.gpu,
                Payload::Clear {
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
            )
            .map_err(gpu_error)?;
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

    fn target(
        &mut self,
        id: u32,
        area: Rect,
        write: bool,
        depth: Option<&(u32, gpu::Depth)>,
    ) -> Result<Target, CallError> {
        self.store
            .render_access(
                id,
                area,
                if write { Access::Write } else { Access::Read },
                self.memory,
            )?
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
        let mut levels = vec![(extent, image)];
        let mut next = id;
        while let Some(&level) = self.draw.complex.get(&next).and_then(|chain| chain.first()) {
            let rect = self.store[level].rect();
            let image = self
                .store
                .gpu_access(level, rect, Access::Read, self.memory)?;
            levels.push((rect, image));
            next = level;
        }
        let gpu = &mut self.draw.d3d.as_mut().unwrap().gpu;
        let image = gpu
            .target_levels(extent.w, extent.h, levels.len() as u32)
            .map_err(gpu_error)?
            .image;
        for (level, (rect, source)) in levels.into_iter().enumerate() {
            let destination = image.mip_level(level as u32).map_err(gpu_error)?;
            gpu.copy(&source, rect, &destination, 0, 0)
                .map_err(gpu_error)?;
        }
        Ok(image)
    }

    pub(super) fn draw_primitive(&mut self, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let Some(com) = self.draw.com else {
            return Ok(INVALID);
        };
        let Some(Object::Device(device)) = com.get::<Object>(self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let Some(state) = &device.state else {
            return Ok(INVALID);
        };
        let sid = self.draw.surface_id(self.objects, device.target).unwrap();
        if self.draw.lost.contains(&sid) {
            return Ok(0);
        }
        let fixed = state.fixed.clone();
        let textures = state.textures;
        let kind = device.kind;
        let strided = method.ends_with("Strided");
        let vb = method.ends_with("VB");
        let count = args[4];
        if count == 0 {
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
            if args[3]
                .checked_add(count)
                .is_none_or(|n| n > desc.dwNumVertices)
            {
                return Ok(INVALID);
            }
            let layout = vertex::layout(desc.dwFVF).unwrap();
            (desc.dwFVF, data + args[3] * layout.size)
        } else {
            (args[2], args[3])
        };
        let layout = match vertex::layout(fvf) {
            Ok(layout) => layout,
            Err(hr) => return Ok(hr),
        };
        let weights = (fvf & 0xe).saturating_sub(4) / 2;
        if weights != 0 && kind != caps::Device::TnlHal {
            return Ok(0x80004005);
        }
        let blend_weights = weights.min(fixed.render[&Render::VertexBlend]) as u8;
        let indexed = method.starts_with("DrawIndexed");
        let mut indices = if indexed {
            let bytes = self.memory.arg_read(args[5], args[6] as usize * 2)?;
            let values = bytes
                .chunks_exact(2)
                .map(|word| u32::from(u16::from_le_bytes(word.try_into().unwrap())))
                .collect::<Vec<_>>();
            if values.iter().any(|&i| i >= count) {
                return Ok(INVALID);
            }
            Some(values)
        } else {
            None
        };
        let entries = indices.as_ref().map_or(count as usize, Vec::len);
        if entries == 0 {
            return Ok(0);
        }
        let topology = match vertex::primitive(args[1], entries) {
            Ok(topology) => topology,
            Err(hr) => return Ok(hr),
        };
        if args[1] == 6 {
            let vertex = |n| indices.as_ref().map_or(n, |i| i[n as usize]);
            indices = Some(vertex::expand_fan(vertex, entries as u32));
        }
        let Some(size) = count.checked_mul(layout.size) else {
            return Ok(INVALID);
        };
        let bytes = if strided {
            let descriptor = read(self.memory, data)?;
            interleave(self.memory, descriptor, layout, count, weights)?
        } else {
            self.memory.arg_read(data, size as usize)?.into_owned()
        };
        let extent = self.store[sid].rect();
        let rules = rules(kind);
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
        let mut texture_words = [[0; 4]; 16];
        for (n, &texture) in textures[..active].iter().enumerate() {
            let sampler = fixed
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
                let key = (fixed.render[&Render::ColorKeyEnable] != 0)
                    .then(|| self.draw.color_keys.get(&(texture, 8)).map(|&(low, _)| low))
                    .flatten();
                texture_words[n * 2 + 1] = [
                    key.unwrap_or(0),
                    rules
                        .color_key_mask
                        .unwrap_or_else(|| masks.into_iter().fold(0, |a, b| a | b)),
                    2 | u32::from(key.is_some()),
                    registers[&Sampler::MipLodBias],
                ];
                if sampler.mip.is_some() && self.draw.caps(texture)[0] & creation::MIPMAP != 0 {
                    // Resolve prior writes before assembling the mip-level views.
                    self.draw.d3d.as_mut().unwrap().flush()?;
                }
                Some(self.render_texture(texture, sampler.mip.is_some())?)
            } else {
                None
            };
            bindings.push(ProgramTexture { image, sampler });
        }
        let lowered = fixed::lower(
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
        if rules.line_last_pixel == Some(true)
            && matches!(topology, Topology::Lines | Topology::LineStrip)
            && layout.transformed
            && active == 0
            && !features_require_shader(lowered.program.features)
            && target_masks(self.draw, self.store, sid) == [0; 4]
        {
            let positions = bytes
                .chunks_exact(layout.size as usize)
                .map(|row| {
                    let position = fixed::tnl::pretransformed(
                        bytemuck::pod_read_unaligned(&row[..16]),
                        lowered.state.viewport,
                    )?;
                    let color = layout.color.map_or([1.; 4], |offset| {
                        fixed::tnl::color(word(row, offset as usize))
                    });
                    Some(gpu::Vertex {
                        position,
                        color,
                        uv: [0.; 2],
                    })
                })
                .collect::<Vec<_>>();
            let ordered = indices
                .as_ref()
                .map_or_else(|| (0..count).collect::<Vec<_>>(), Clone::clone);
            let Some(vertices) = ordered
                .into_iter()
                .map(|i| positions[i as usize])
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(INVALID);
            };
            let target = self.target(sid, bounds, true, z.as_ref())?;
            let d3d = self.draw.d3d.as_mut().unwrap();
            recording::admit(
                &mut d3d.queue,
                &mut d3d.gpu,
                Payload::Lines {
                    target,
                    vertices: Arc::new(vertices),
                    state: lowered.state,
                },
                Effects {
                    area: bounds,
                    target: sid,
                    depth: z.as_ref().map(|&(id, _)| id),
                    view: extent,
                    reads: &[],
                    bytes: size as usize,
                    state_key: hash((&bytes, &indices, fixed.key())),
                },
            )
            .map_err(gpu_error)?;
            return Ok(0);
        }
        let viewport = lowered.state.viewport;
        parameters[0] = [viewport.width, viewport.height, viewport.x, viewport.y].map(f32::to_bits);
        parameters[1][2] = lowered.state.alpha_test as u32;
        parameters[1][3] = lowered.state.alpha_reference.to_bits();
        parameters[3][0] = fixed.render[&Render::ClipPlaneEnable];
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
        parameters[39] = target_masks(self.draw, self.store, sid);
        let legacy = Legacy {
            stages: active as u8,
            point_rounding: (topology == Topology::Points).then_some(rules.round_points),
            blend_weights,
            perspective: fixed.render[&Render::TexturePerspective] != 0,
        };
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
        let contents = hash((&bytes, &indices, fixed.key(), parameters, texture_words));
        let stream = VertexStream {
            instance: false,
            bytes: VertexBytes::from(bytes),
            stride: layout.size,
            attributes: inputs
                .iter()
                .map(|input| Attribute {
                    location: input.register,
                    offset: input.offset,
                    format: input.format,
                })
                .collect(),
        };
        let draw = Arc::new(ProgrammableDraw {
            instance_count: 1,
            program,
            streams: vec![stream],
            indices: indices.map(IndexBytes::from),
            vertex_count: count,
            vertex_constants,
            pixel_constants: ConstantSnapshot::capture(texture_use, |n| texture_words[n]),
            textures: bindings,
            parameters,
        });
        let target = self.target(sid, bounds, true, z.as_ref())?;
        let d3d = self.draw.d3d.as_mut().unwrap();
        recording::admit(
            &mut d3d.queue,
            &mut d3d.gpu,
            Payload::Draw {
                target,
                draw,
                state: lowered.state,
            },
            Effects {
                area: bounds,
                target: sid,
                depth: z.as_ref().map(|&(id, _)| id),
                view: extent,
                reads: &reads,
                bytes: size as usize,
                state_key: contents,
            },
        )
        .map_err(gpu_error)?;
        Ok(0)
    }
}

fn features_require_shader(features: u32) -> bool {
    features & !fixed::STAGE_ARGUMENTS != 0
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

fn interleave(
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

fn target_masks(
    draw: &ddraw::DirectDraw,
    store: &adv_device::surface::SurfaceStore,
    id: u32,
) -> [u32; 4] {
    if store[id].format.bytes() == 4 {
        return [0; 4];
    }
    let format = draw.described[&id].format;
    std::array::from_fn(|i| word(&format, 16 + i * 4))
}

fn pack_clear(color: [f32; 4], masks: [u32; 4]) -> [f64; 4] {
    if masks[0] == 0 {
        return color.map(f64::from);
    }
    let bits = masks
        .into_iter()
        .zip(color)
        .filter(|&(mask, _)| mask != 0)
        .fold(0, |bits, (mask, color)| {
            let shift = mask.trailing_zeros();
            bits | (((color * (mask >> shift) as f32).round() as u32) << shift)
        });
    fixed::tnl::color(bits).map(f64::from)
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
