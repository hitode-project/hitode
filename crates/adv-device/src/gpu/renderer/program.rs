use super::*;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    program: Program,
    streams: Vec<(u32, Vec<Attribute>)>,
    raster: PipelineKey,
}
#[derive(Clone)]
struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    constants: wgpu::BindGroup,
    textures: wgpu::BindGroupLayout,
}
#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<Key, Pipeline>,
    order: VecDeque<Key>,
    uniforms: Option<wgpu::Buffer>,
    cursor: u64,
    vertices: Vec<(wgpu::Buffer, u64)>,
    indices: Option<(wgpu::Buffer, u64)>,
    pub(super) submissions: Arc<AtomicU64>,
    pub(super) disk: Option<super::pipeline_disk::DiskCache>,
}
const CONSTANT_SIZE: u64 = std::mem::size_of::<ConstantBlock>() as u64;
const PARAMETER_SIZE: u64 = 640;
const UNIFORM_STRIDE: u64 = (CONSTANT_SIZE * 2 + PARAMETER_SIZE).div_ceil(256) * 256;
const UNIFORM_SLOTS: u64 = 32;

// Queue writes precede submitted command buffers. Each pending draw owns a
// different upload slot; external writes and consumers flush the batch first.
#[derive(Default)]
pub(super) struct Pending {
    // Drop the pass before its encoder, including during producer teardown.
    pass: Option<wgpu::RenderPass<'static>>,
    attachments: Option<(wgpu::TextureView, Option<wgpu::TextureView>)>,
    encoder: Option<wgpu::CommandEncoder>,
    owner: Option<Arc<AtomicU64>>,
    draws: u64,
}
impl Pending {
    pub(super) fn flush(&mut self, queue: &wgpu::Queue) {
        self.pass = None;
        self.attachments = None;
        if let Some(encoder) = self.encoder.take() {
            queue.submit([encoder.finish()]);
            self.owner.take().unwrap().fetch_add(1, Ordering::Relaxed);
        }
        self.draws = 0;
    }
}

/// Validate before handing guest-derived source to the device. Failure remains
/// a failed command, rather than poisoning the GPU's uncaptured-error channel.
pub fn validate_program(program: &Program) -> Result<(), Error> {
    if program.source.len() > 4 * 1024 * 1024 {
        return Err(invalid("program source exceeds limit"));
    }
    let module = wgpu::naga::front::wgsl::parse_str(&program.source)
        .map_err(|e| invalid(&e.emit_to_string(&program.source)))?;
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|e| invalid(&e.emit_to_string(&program.source)))?;
    Ok(())
}
impl Gpu {
    fn program_pipeline(&mut self, key: Key) -> Result<Pipeline, Error> {
        if let Some(p) = self.programmable.entries.get(&key) {
            return Ok(p.clone());
        }
        validate_program(&key.program)?;
        let device = &self.context.device;
        let limits = device.limits();
        if key.streams.len() > limits.max_vertex_buffers as usize
            || key.streams.iter().map(|s| s.1.len()).sum::<usize>()
                > limits.max_vertex_attributes as usize
            || key.program.textures.iter().filter(|v| **v).count()
                > limits.max_sampled_textures_per_shader_stage as usize
            || key.program.textures.iter().filter(|v| !**v).count()
                > limits.max_sampled_textures_per_shader_stage as usize
        {
            return Err(Error::Unavailable(
                "programmable draw exceeds adapter input limits".into(),
            ));
        }
        let buffer_entry = |binding, size, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        let constants = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("programmable constants"),
            entries: &[
                buffer_entry(
                    0,
                    std::mem::size_of::<ConstantBlock>() as u64,
                    wgpu::ShaderStages::VERTEX,
                ),
                buffer_entry(
                    1,
                    std::mem::size_of::<ConstantBlock>() as u64,
                    wgpu::ShaderStages::FRAGMENT,
                ),
                buffer_entry(2, PARAMETER_SIZE, wgpu::ShaderStages::VERTEX_FRAGMENT),
            ],
        });
        let mut entries = Vec::new();
        for (n, vertex) in key.program.textures.iter().enumerate() {
            let visibility = if *vertex {
                wgpu::ShaderStages::VERTEX
            } else {
                wgpu::ShaderStages::FRAGMENT
            };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: n as u32 * 2,
                visibility,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: n as u32 * 2 + 1,
                visibility,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let textures = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("programmable textures"),
            entries: &entries,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("programmable layout"),
            bind_group_layouts: &[Some(&constants), Some(&textures)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("translated shader"),
            source: wgpu::ShaderSource::Wgsl(key.program.source.to_string().into()),
        });
        let attributes: Vec<Vec<_>> = key
            .streams
            .iter()
            .map(|s| {
                s.1.iter()
                    .map(|a| wgpu::VertexAttribute {
                        format: a.format.native(),
                        offset: u64::from(a.offset),
                        shader_location: a.location,
                    })
                    .collect()
            })
            .collect();
        let buffers: Vec<_> = key
            .streams
            .iter()
            .zip(&attributes)
            .map(|(s, a)| {
                Some(wgpu::VertexBufferLayout {
                    array_stride: u64::from(s.0),
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: a,
                })
            })
            .collect();
        let k = key.raster;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("programmable pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: topology(k.topology),
                strip_index_format: matches!(
                    k.topology,
                    Topology::TriangleStrip | Topology::LineStrip
                )
                .then_some(wgpu::IndexFormat::Uint32),
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: match k.cull {
                    Cull::None => None,
                    Cull::Clockwise => Some(wgpu::Face::Back),
                    Cull::CounterClockwise => Some(wgpu::Face::Front),
                },
                ..Default::default()
            },
            depth_stencil: k.depth_format.map(|f| wgpu::DepthStencilState {
                format: f.format(),
                depth_write_enabled: Some(k.depth_write && k.depth.is_some()),
                depth_compare: Some(k.depth.map_or(wgpu::CompareFunction::Always, compare)),
                stencil: k.stencil.map(stencil_state).unwrap_or_default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: k.color_blend.map(|c| wgpu::BlendState {
                        color: blend(c),
                        alpha: blend(k.alpha_blend.unwrap_or(c)),
                    }),
                    write_mask: wgpu::ColorWrites::from_bits_truncate(u32::from(k.color_write)),
                })],
            }),
            multiview_mask: None,
            cache: self.programmable.disk.as_ref().map(|d| &d.cache),
        });
        self.check()?;
        let uniform = self.programmable.uniforms.as_ref().unwrap();
        let entry = |binding, offset, size| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: uniform,
                offset,
                size: wgpu::BufferSize::new(size),
            }),
        };
        let constants = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("draw constants"),
            layout: &constants,
            entries: &[
                entry(0, 0, CONSTANT_SIZE),
                entry(1, CONSTANT_SIZE, CONSTANT_SIZE),
                entry(2, CONSTANT_SIZE * 2, PARAMETER_SIZE),
            ],
        });
        let result = Pipeline {
            pipeline,
            constants,
            textures,
        };
        if self.programmable.entries.len() >= self.pipeline_limit
            && let Some(old) = self.programmable.order.pop_front()
        {
            self.programmable.entries.remove(&old);
        }
        self.programmable.order.push_back(key.clone());
        self.programmable.entries.insert(key, result.clone());
        if let Some(disk) = &mut self.programmable.disk {
            disk.changed();
        }
        Ok(result)
    }
    pub fn draw_programmable(
        &mut self,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
    ) -> Result<(), Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.own_target(target)?;
        target.image.writable()?;
        let count = draw
            .indices
            .as_ref()
            .map_or(draw.vertex_count as usize, Vec::len);
        // Reuse all raster checks without decoding or evaluating guest vertices.
        let dummy = Vertex {
            position: [0., 0., 0., 1.],
            color: [1.; 4],
            uv: [0.; 2],
        };
        let probe = match state.topology {
            Topology::Points => 1,
            Topology::Lines | Topology::LineStrip => 2,
            _ => 3,
        };
        validate(target, &vec![dummy; probe], state)?;
        if count == 0
            || count > 0x300000
            || match state.topology {
                Topology::Lines => !count.is_multiple_of(2),
                Topology::Triangles => !count.is_multiple_of(3),
                Topology::LineStrip => count < 2,
                Topology::TriangleStrip => count < 3,
                _ => false,
            }
        {
            return Err(invalid("invalid programmable primitive count"));
        }
        if draw.textures.len() != draw.program.textures.len() {
            return Err(invalid("shader texture bindings mismatch"));
        }
        let last = draw
            .indices
            .as_ref()
            .and_then(|v| v.iter().max().copied())
            .unwrap_or(draw.vertex_count.saturating_sub(1));
        if last >= draw.vertex_count {
            return Err(invalid("shader index outside uploaded vertex span"));
        }
        let mut locations = std::collections::HashSet::new();
        let mut total = 0u64;
        for stream in &draw.streams {
            if stream.stride > 2048
                || !stream.stride.is_multiple_of(4)
                || stream.attributes.is_empty()
            {
                return Err(invalid("invalid programmable vertex stride"));
            }
            for a in &stream.attributes {
                let end = a
                    .offset
                    .checked_add(a.format.size())
                    .ok_or_else(|| invalid("attribute offset overflow"))?;
                if !a.offset.is_multiple_of(4)
                    || a.location >= 16
                    || !locations.insert(a.location)
                    || (stream.stride != 0 && end > stream.stride)
                    || u64::from(last) * u64::from(stream.stride) + u64::from(end)
                        > stream.bytes.len() as u64
                {
                    return Err(invalid("vertex attribute outside uploaded span"));
                }
            }
            total += stream.bytes.len() as u64;
        }
        if total > MAX_UPLOAD {
            return Err(invalid("programmable vertex upload exceeds command limit"));
        }
        for binding in &draw.textures {
            if let Some(image) = &binding.image {
                self.own(image)?;
                if image.id == target.image.id {
                    return Err(invalid("render target cannot sample itself"));
                }
            }
        }
        let mut raster = PipelineKey {
            topology: state.topology,
            cull: state.cull,
            color_blend: state.blend,
            alpha_blend: state.alpha_blend,
            depth: state.depth,
            depth_write: state.depth_write,
            depth_format: target.depth.as_ref().map(|d| d.format()),
            stencil: state.stencil,
            color_write: state.color_write,
        };
        if raster.stencil.is_some()
            && target
                .depth
                .as_ref()
                .is_none_or(|d| !d.format().has_stencil())
        {
            return Err(invalid("stencil state requires stencil attachment"));
        }
        if let Some(s) = &mut raster.stencil {
            s.read_mask &= 255;
            s.write_mask &= 255;
        }
        if target.image.alpha == AlphaMode::Opaque
            && let Some(b) = &mut raster.color_blend
        {
            let f = |f| match f {
                Factor::DestinationAlpha => Factor::One,
                Factor::OneMinusDestinationAlpha | Factor::SourceAlphaSaturated => Factor::Zero,
                f => f,
            };
            b.source = f(b.source);
            b.destination = f(b.destination);
        }
        if self.programmable.uniforms.is_none() {
            self.programmable.uniforms =
                Some(context.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("programmable uniform ring"),
                    size: UNIFORM_STRIDE * UNIFORM_SLOTS,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }
        let pipeline = self.program_pipeline(Key {
            program: draw.program.clone(),
            streams: draw
                .streams
                .iter()
                .map(|s| (s.stride, s.attributes.clone()))
                .collect(),
            raster,
        })?;
        let uniform = self.programmable.uniforms.as_ref().unwrap();
        let slot = self.programmable.cursor;
        let index_size = draw
            .indices
            .as_ref()
            .map_or(0, |v| std::mem::size_of_val(v.as_slice()) as u64);
        if index_size > MAX_UPLOAD {
            return Err(invalid("index upload exceeds command limit"));
        }
        let immediate =
            total > MAX_UPLOAD / UNIFORM_SLOTS || index_size > MAX_UPLOAD / UNIFORM_SLOTS;
        let slots = if immediate { 1 } else { UNIFORM_SLOTS };
        let upload_slot = if immediate { 0 } else { slot };
        let mut pending = context.pending.lock().unwrap();
        if slot == 0
            || immediate
            || pending
                .owner
                .as_ref()
                .is_some_and(|owner| !Arc::ptr_eq(owner, &self.programmable.submissions))
        {
            pending.flush(&context.queue);
        }
        let base = slot * UNIFORM_STRIDE;
        self.programmable.cursor = (self.programmable.cursor + 1) % UNIFORM_SLOTS;
        // A slot is never overwritten until its pending commands are submitted.
        {
            let mut upload = submission
                .context
                .queue
                .write_buffer_with(
                    uniform,
                    base,
                    wgpu::BufferSize::new(CONSTANT_SIZE * 2 + PARAMETER_SIZE).unwrap(),
                )
                .ok_or_else(|| invalid("programmable constant upload rejected"))?;
            let split = CONSTANT_SIZE as usize;
            upload
                .slice(..split)
                .copy_from_slice(bytemuck::bytes_of(&draw.vertex_constants));
            upload
                .slice(split..split * 2)
                .copy_from_slice(bytemuck::bytes_of(&draw.pixel_constants));
            upload
                .slice(split * 2..)
                .copy_from_slice(bytemuck::bytes_of(&draw.parameters));
        }
        let samplers: Vec<_> = draw
            .textures
            .iter()
            .map(|b| self.sampler(b.sampler))
            .collect();
        let mut entries = Vec::new();
        for (n, b) in draw.textures.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: n as u32 * 2,
                resource: wgpu::BindingResource::TextureView(
                    &b.image.as_ref().unwrap_or(&self.white).view,
                ),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: n as u32 * 2 + 1,
                resource: wgpu::BindingResource::Sampler(&samplers[n]),
            });
        }
        let textures = context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("draw textures"),
                layout: &pipeline.textures,
                entries: &entries,
            });
        for (n, stream) in draw.streams.iter().enumerate() {
            let size = stream.bytes.len() as u64;
            if self
                .programmable
                .vertices
                .get(n)
                .is_none_or(|(_, capacity)| *capacity < size * slots)
            {
                let capacity = (size.max(4) * slots).next_power_of_two().min(MAX_UPLOAD);
                let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("raw vertex upload"),
                    size: capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                if n == self.programmable.vertices.len() {
                    self.programmable.vertices.push((buffer, capacity));
                } else {
                    self.programmable.vertices[n] = (buffer, capacity);
                }
                self.stats.vertex_buffer_allocations += 1;
            }
            let (buffer, capacity) = &self.programmable.vertices[n];
            context
                .queue
                .write_buffer(buffer, upload_slot * (capacity / slots), &stream.bytes);
        }
        if let Some(indices) = &draw.indices {
            let size = std::mem::size_of_val(indices.as_slice()) as u64;
            if size > MAX_UPLOAD {
                return Err(invalid("index upload exceeds command limit"));
            }
            if self
                .programmable
                .indices
                .as_ref()
                .is_none_or(|(_, capacity)| *capacity < size * slots)
            {
                let capacity = (size.max(4) * slots).next_power_of_two().min(MAX_UPLOAD);
                self.programmable.indices = Some((
                    context.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("index upload"),
                        size: capacity,
                        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    capacity,
                ));
            }
            let (buffer, capacity) = self.programmable.indices.as_ref().unwrap();
            context.queue.write_buffer(
                buffer,
                upload_slot * (capacity / slots),
                bytemuck::cast_slice(indices),
            );
        }
        let attachment = self.attachment_color(target)?;
        pending.owner = Some(self.programmable.submissions.clone());
        let depth_view = target.depth.as_ref().map(|d| d.view());
        let padded = target.padded_attachments();
        if padded
            || pending.attachments.as_ref().is_none_or(|(color, depth)| {
                color != &attachment.image.render_view || depth.as_ref() != depth_view
            })
        {
            pending.pass = None;
        }
        if pending.pass.is_none() {
            let encoder = pending
                .encoder
                .get_or_insert_with(|| context.device.create_command_encoder(&Default::default()));
            attachment.load(encoder, &target.image);
            let colors = [Some(wgpu::RenderPassColorAttachment {
                view: &attachment.image.render_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })];
            let pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &colors,
                    depth_stencil_attachment: target.depth.as_ref().map(|d| {
                        wgpu::RenderPassDepthStencilAttachment {
                            view: d.view(),
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: d.format().has_stencil().then_some(wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            }),
                        }
                    }),
                    ..Default::default()
                })
                .forget_lifetime();
            pending.pass = Some(pass);
            pending.attachments = Some((attachment.image.render_view.clone(), depth_view.cloned()));
        }
        {
            let pass = pending.pass.as_mut().unwrap();
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &pipeline.constants, &[base as u32; 3]);
            pass.set_bind_group(1, &textures, &[]);
            let v = state.viewport;
            pass.set_viewport(
                v.x,
                v.y,
                v.width,
                v.height,
                v.minimum_depth,
                v.maximum_depth,
            );
            let r = state.scissor.unwrap_or(Rect {
                x: 0,
                y: 0,
                w: target.image.width,
                h: target.image.height,
            });
            pass.set_scissor_rect(r.x, r.y, r.w, r.h);
            if state.stencil.is_some() {
                pass.set_stencil_reference(state.stencil_reference & 255);
            }
            for (n, (b, capacity)) in self
                .programmable
                .vertices
                .iter()
                .take(draw.streams.len())
                .enumerate()
            {
                pass.set_vertex_buffer(n as u32, b.slice(upload_slot * (capacity / slots)..));
            }
            if draw.indices.is_some() {
                let (i, capacity) = self.programmable.indices.as_ref().unwrap();
                pass.set_index_buffer(
                    i.slice(upload_slot * (capacity / slots)..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..count as u32, 0, 0..1);
            } else {
                pass.draw(0..draw.vertex_count, 0..1);
            }
        }
        if padded {
            pending.pass = None;
            attachment.store(pending.encoder.as_mut().unwrap(), &target.image);
        }
        pending.draws += 1;
        if pending.draws == UNIFORM_SLOTS || immediate {
            pending.flush(&context.queue);
        }
        self.stats.draws += 1;
        self.stats.vertices += count as u64;
        self.check()
    }
}
