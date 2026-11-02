use super::*;
use ash::vk::Handle;

pub(super) fn group(kind: BindingKind) -> u32 {
    match kind {
        BindingKind::UniformBuffer => 0,
        BindingKind::SampledImage { .. } => 1,
        BindingKind::Sampler { .. } => 2,
        BindingKind::StorageImage { .. } | BindingKind::StorageBuffer { .. } => 3,
    }
}
pub(super) fn descriptor(kind: BindingKind) -> vk::DescriptorType {
    match kind {
        BindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        BindingKind::SampledImage { .. } => vk::DescriptorType::SAMPLED_IMAGE,
        BindingKind::Sampler { .. } => vk::DescriptorType::SAMPLER,
        BindingKind::StorageImage { .. } => vk::DescriptorType::STORAGE_IMAGE,
        BindingKind::StorageBuffer { .. } => vk::DescriptorType::STORAGE_BUFFER,
    }
}
pub(super) fn stages(stages: Stages) -> vk::ShaderStageFlags {
    let mut flags = vk::ShaderStageFlags::empty();
    for (public, native) in [
        (Stages::VERTEX, vk::ShaderStageFlags::VERTEX),
        (Stages::FRAGMENT, vk::ShaderStageFlags::FRAGMENT),
        (Stages::COMPUTE, vk::ShaderStageFlags::COMPUTE),
    ] {
        if stages.contains(public) {
            flags |= native;
        }
    }
    flags
}
pub(super) fn word(group: u32, slot: u32) -> u32 {
    slot * 3 + group - 1
}

pub(super) struct Heap {
    device: ash::Device,
    pub(super) counts: [u32; 4],
    pub(super) layout: vk::DescriptorSetLayout,
    pub(super) uniform_layout: vk::DescriptorSetLayout,
    pub(super) set: vk::DescriptorSet,
    pool: vk::DescriptorPool,
    uniforms: vk::DescriptorPool,
    state: Mutex<HeapState>,
}
struct HeapState {
    free: [Vec<u32>; 4],
}
impl Heap {
    pub(super) fn new(
        instance: &ash::Instance,
        physical: vk::PhysicalDevice,
        device: &ash::Device,
    ) -> Result<Self> {
        let mut indexing = vk::PhysicalDeviceDescriptorIndexingProperties::default();
        unsafe {
            instance.get_physical_device_properties2(
                physical,
                &mut vk::PhysicalDeviceProperties2::default().push_next(&mut indexing),
            );
        }
        let counts = [
            indexing
                .max_descriptor_set_update_after_bind_sampled_images
                .min(indexing.max_per_stage_descriptor_update_after_bind_sampled_images),
            indexing
                .max_descriptor_set_update_after_bind_samplers
                .min(indexing.max_per_stage_descriptor_update_after_bind_samplers),
            indexing
                .max_descriptor_set_update_after_bind_storage_images
                .min(indexing.max_per_stage_descriptor_update_after_bind_storage_images),
            indexing
                .max_descriptor_set_update_after_bind_storage_buffers
                .min(indexing.max_per_stage_descriptor_update_after_bind_storage_buffers),
        ]
        .map(|n| {
            n.min(1024)
                .min(indexing.max_per_stage_update_after_bind_resources / 4)
        });
        let kinds = [
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLER,
            vk::DescriptorType::STORAGE_IMAGE,
            vk::DescriptorType::STORAGE_BUFFER,
        ];
        let bindings: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i as u32)
                    .descriptor_type(kind)
                    .descriptor_count(counts[i])
                    .stage_flags(vk::ShaderStageFlags::ALL)
            })
            .collect();
        let flags = [vk::DescriptorBindingFlags::UPDATE_AFTER_BIND
            | vk::DescriptorBindingFlags::PARTIALLY_BOUND
            | vk::DescriptorBindingFlags::UPDATE_UNUSED_WHILE_PENDING; 4];
        let mut flags_info =
            vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&flags);
        let mut heap = Self {
            device: device.clone(),
            counts,
            layout: vk::DescriptorSetLayout::null(),
            uniform_layout: vk::DescriptorSetLayout::null(),
            set: vk::DescriptorSet::null(),
            pool: vk::DescriptorPool::null(),
            uniforms: vk::DescriptorPool::null(),
            state: Mutex::new(HeapState {
                free: counts.map(|n| (0..n).rev().collect()),
            }),
        };
        unsafe {
            heap.layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default()
                        .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
                        .bindings(&bindings)
                        .push_next(&mut flags_info),
                    None,
                )
                .map_err(vk_error)?;
            let sizes: Vec<_> = kinds
                .iter()
                .enumerate()
                .map(|(i, &ty)| vk::DescriptorPoolSize {
                    ty,
                    descriptor_count: counts[i],
                })
                .collect();
            heap.pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND)
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(vk_error)?;
            heap.set = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(heap.pool)
                        .set_layouts(&[heap.layout]),
                )
                .map_err(vk_error)?[0];
            // Uniforms use one dynamic descriptor written once per buffer/block size.
            // Separate uniform sets let offsets change without descriptor allocation or
            // updates; descriptor indexing forbids dynamic descriptors in the heap.
            heap.uniform_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                        vk::DescriptorSetLayoutBinding::default()
                            .binding(0)
                            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                            .descriptor_count(1)
                            .stage_flags(vk::ShaderStageFlags::ALL),
                    ]),
                    None,
                )
                .map_err(vk_error)?;
            heap.uniforms = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(4096)
                        .pool_sizes(&[vk::DescriptorPoolSize {
                            ty: vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC,
                            descriptor_count: 4096,
                        }]),
                    None,
                )
                .map_err(vk_error)?;
            Ok(heap)
        }
    }
    pub(super) fn insert(
        &self,
        context: &Context,
        kind: usize,
        buffer: Option<vk::DescriptorBufferInfo>,
        image: Option<vk::DescriptorImageInfo>,
    ) -> Result<u32> {
        let mut state = self.state.lock().unwrap();
        let index = state.free[kind].pop().ok_or(Error::OutOfMemory)?;
        let mut write = vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(kind as u32)
            .dst_array_element(index)
            .descriptor_type(
                [
                    vk::DescriptorType::SAMPLED_IMAGE,
                    vk::DescriptorType::SAMPLER,
                    vk::DescriptorType::STORAGE_IMAGE,
                    vk::DescriptorType::STORAGE_BUFFER,
                ][kind],
            );
        let buffers = buffer.into_iter().collect::<Vec<_>>();
        let images = image.into_iter().collect::<Vec<_>>();
        if !buffers.is_empty() {
            write = write.buffer_info(&buffers);
        } else {
            write = write.image_info(&images);
        }
        unsafe {
            context.device.update_descriptor_sets(&[write], &[]);
        }
        Ok(index)
    }
    pub(super) fn free(&self, kind: usize, index: u32) {
        self.state.lock().unwrap().free[kind].push(index);
    }
    pub(super) fn uniform(
        &self,
        context: &Context,
        buffer: vk::Buffer,
        size: u64,
    ) -> Result<vk::DescriptorSet> {
        let _lock = self.state.lock().unwrap();
        let set = context.result(unsafe {
            context.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.uniforms)
                    .set_layouts(&[self.uniform_layout]),
            )
        })?[0];
        let info = [vk::DescriptorBufferInfo::default()
            .buffer(buffer)
            .range(size)];
        unsafe {
            context.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                    .buffer_info(&info)],
                &[],
            );
        }
        Ok(set)
    }
    pub(super) fn free_uniform(&self, context: &Context, set: vk::DescriptorSet) {
        let _lock = self.state.lock().unwrap();
        unsafe {
            let _ = context.device.free_descriptor_sets(self.uniforms, &[set]);
        }
    }
}
impl Drop for Heap {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_descriptor_pool(self.uniforms, None);
            self.device
                .destroy_descriptor_set_layout(self.uniform_layout, None);
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Bound {
    Buffer { handle: u64, offset: u64, size: u64 },
    Image { handle: u64, layout: i32 },
    Sampler(u64),
}
impl Bound {
    fn buffer_info(&self) -> vk::DescriptorBufferInfo {
        match *self {
            Self::Buffer {
                handle,
                offset,
                size,
            } => vk::DescriptorBufferInfo::default()
                .buffer(vk::Buffer::from_raw(handle))
                .offset(offset)
                .range(size),
            _ => vk::DescriptorBufferInfo::default(),
        }
    }
    fn image_info(&self) -> vk::DescriptorImageInfo {
        match *self {
            Self::Image { handle, layout } => vk::DescriptorImageInfo::default()
                .image_view(vk::ImageView::from_raw(handle))
                .image_layout(vk::ImageLayout::from_raw(layout)),
            Self::Sampler(handle) => {
                vk::DescriptorImageInfo::default().sampler(vk::Sampler::from_raw(handle))
            }
            _ => vk::DescriptorImageInfo::default(),
        }
    }
}
#[derive(Default)]
pub(super) struct Bindings {
    values: HashMap<(u32, u32), Bound>,
    changed: bool,
    dirty: std::collections::HashSet<(u32, u32)>,
    pushed_layout: vk::PipelineLayout,
    layout: vk::PipelineLayout,
    heap_bound: bool,
    uniform_bound: HashMap<u32, (vk::DescriptorSet, u32)>,
    uniforms: HashMap<u32, (Buffer, u32, u64)>,
}
impl Bindings {
    pub(super) fn pipeline(&mut self, layout: vk::PipelineLayout) {
        if self.layout != layout {
            self.layout = layout;
            self.changed = true;
        }
    }
    pub(super) fn bind(
        &mut self,
        encoder: &mut Encoder,
        pipeline: &pipeline::PipelineCore,
        slot: u32,
        resource: Bind<'_>,
        preamble: Option<vk::CommandBuffer>,
    ) {
        if !encoder.ready() {
            return;
        }
        let (group, bound) = match resource {
            Bind::Uniform(slice) | Bind::StorageBuffer(slice) => {
                encoder.resources.buffers.push(slice.buffer.inner.clone());
                (
                    if matches!(resource, Bind::Uniform(_)) {
                        0
                    } else {
                        3
                    },
                    Bound::Buffer {
                        handle: slice.buffer.inner.0.handle.as_raw(),
                        offset: slice.offset,
                        size: slice.size,
                    },
                )
            }
            Bind::Sampled(view) | Bind::StorageImage(view) => {
                let sampled = matches!(resource, Bind::Sampled(_));
                if sampled && view.inner.0.image.inner.core.conversion.is_some() {
                    encoder.error = Some(Error::Unavailable(
                        "YCbCr sampling requires a combined immutable sampler".into(),
                    ));
                    return;
                }
                let depth = view.inner.0.format.has_depth();
                let layout = if sampled && depth {
                    vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL
                } else {
                    vk::ImageLayout::GENERAL
                };
                let command = encoder.command;
                if let Some(preamble) = preamble {
                    encoder.command = preamble;
                }
                let needs_transition = view
                    .inner
                    .0
                    .image
                    .inner
                    .core
                    .states
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|s| s.layout != layout);
                if needs_transition {
                    encoder.view_transition(
                        &view.inner,
                        layout,
                        vk::AccessFlags2::SHADER_READ
                            | if sampled {
                                vk::AccessFlags2::empty()
                            } else {
                                vk::AccessFlags2::SHADER_WRITE
                            },
                        vk::PipelineStageFlags2::ALL_GRAPHICS
                            | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    );
                }
                encoder.command = command;
                encoder.resources.views.push(view.inner.clone());
                (
                    if sampled { 1 } else { 3 },
                    Bound::Image {
                        handle: view.inner.0.handle.as_raw(),
                        layout: layout.as_raw(),
                    },
                )
            }
            Bind::Sampler(sampler) => {
                encoder.resources.samplers.push(sampler.inner.clone());
                (2, Bound::Sampler(sampler.inner.0.handle.as_raw()))
            }
        };
        if self.values.get(&(group, slot)) == Some(&bound) {
            return;
        }
        self.values.insert((group, slot), bound);
        self.dirty.insert((group, slot));
        self.changed = true;
        let context = &encoder.shared.context;
        if context.heap.is_some() {
            let result = match resource {
                Bind::Uniform(slice) => {
                    self.uniforms.insert(
                        slot,
                        (slice.buffer.inner.clone(), slice.offset as u32, slice.size),
                    );
                    Ok(())
                }
                _ => {
                    let index = match resource {
                        Bind::Sampled(view) => Ok(view.inner.0.sampled.unwrap()),
                        Bind::Sampler(sampler) => Ok(sampler.inner.0.index.unwrap()),
                        Bind::StorageImage(view) => Ok(view.inner.0.storage.unwrap()),
                        Bind::StorageBuffer(slice) => {
                            slice.buffer.inner.heap_index(slice.offset, slice.size)
                        }
                        Bind::Uniform(_) => unreachable!(),
                    };
                    index.map(|index| unsafe {
                        context.device.cmd_push_constants(
                            encoder.command,
                            pipeline.layout,
                            vk::ShaderStageFlags::ALL,
                            word(group, slot) * 4,
                            &index.to_ne_bytes(),
                        );
                    })
                }
            };
            if let Err(error) = result {
                encoder.error = Some(error);
            }
        }
    }
    pub(super) fn flush(
        &mut self,
        encoder: &mut Encoder,
        pipeline: &pipeline::PipelineCore,
        point: vk::PipelineBindPoint,
    ) -> bool {
        if !encoder.ready() {
            return false;
        }
        let context = encoder.shared.context.clone();
        if let Some(heap) = &context.heap {
            if !self.heap_bound {
                unsafe {
                    context.device.cmd_bind_descriptor_sets(
                        encoder.command,
                        point,
                        pipeline.layout,
                        0,
                        &[heap.set],
                        &[],
                    );
                }
                self.heap_bound = true;
            }
            for binding in &pipeline.bindings {
                if binding.kind != BindingKind::UniformBuffer {
                    continue;
                }
                let slot = binding.slot;
                let Some((buffer, offset, bound_size)) = self.uniforms.get(&slot) else {
                    encoder.error = Some(Error::Invalid("missing uniform binding".into()));
                    return false;
                };
                let size = pipeline
                    .uniform_sizes
                    .get(&slot)
                    .copied()
                    .unwrap_or(*bound_size);
                let mut sets = buffer.0.uniforms.lock().unwrap();
                let set = if let Some(&set) = sets.get(&size) {
                    set
                } else {
                    match heap.uniform(&context, buffer.0.handle, size) {
                        Ok(set) => {
                            sets.insert(size, set);
                            set
                        }
                        Err(error) => {
                            encoder.error = Some(error);
                            return false;
                        }
                    }
                };
                if self.uniform_bound.get(&slot) != Some(&(set, *offset)) {
                    unsafe {
                        context.device.cmd_bind_descriptor_sets(
                            encoder.command,
                            point,
                            pipeline.layout,
                            slot + 1,
                            &[set],
                            &[*offset],
                        );
                    }
                    self.uniform_bound.insert(slot, (set, *offset));
                }
            }
            return true;
        }
        if !self.changed || pipeline.bindings.is_empty() {
            return true;
        }
        let values: Vec<_> = pipeline
            .bindings
            .iter()
            .map(|b| self.values.get(&(group(b.kind), b.slot)).cloned())
            .collect();
        if values.iter().any(Option::is_none) {
            encoder.error = Some(Error::Invalid("missing pipeline binding".into()));
            return false;
        }
        let values: Vec<_> = values.into_iter().flatten().collect();
        let buffers: Vec<_> = values.iter().map(Bound::buffer_info).collect();
        let images: Vec<_> = values.iter().map(Bound::image_info).collect();
        let push = context.caps.modern.contains(Modern::PUSH_DESCRIPTORS);
        let set = if push {
            vk::DescriptorSet::null()
        } else {
            match encoder.descriptor_set(pipeline.set_layout, &pipeline.bindings, &values) {
                Ok((set, false)) => {
                    unsafe {
                        context.device.cmd_bind_descriptor_sets(
                            encoder.command,
                            point,
                            pipeline.layout,
                            0,
                            &[set],
                            &[],
                        );
                    }
                    self.changed = false;
                    return true;
                }
                Ok((set, true)) => set,
                Err(error) => {
                    encoder.error = Some(error);
                    return false;
                }
            }
        };
        let writes: Vec<_> = pipeline
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                !push
                    || self.pushed_layout != pipeline.layout
                    || self.dirty.contains(&(group(b.kind), b.slot))
            })
            .map(|(i, b)| {
                let write = vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(b.slot * 4 + group(b.kind))
                    .descriptor_type(descriptor(b.kind));
                if matches!(values[i], Bound::Buffer { .. }) {
                    write.buffer_info(&buffers[i..i + 1])
                } else {
                    write.image_info(&images[i..i + 1])
                }
            })
            .collect();
        unsafe {
            if push {
                context.push.cmd_push_descriptor_set(
                    encoder.command,
                    point,
                    pipeline.layout,
                    0,
                    &writes,
                );
            } else {
                context.device.update_descriptor_sets(&writes, &[]);
                context.device.cmd_bind_descriptor_sets(
                    encoder.command,
                    point,
                    pipeline.layout,
                    0,
                    &[set],
                    &[],
                );
            }
        }
        self.changed = false;
        self.dirty.clear();
        self.pushed_layout = pipeline.layout;
        true
    }
}

#[derive(Default)]
pub(super) struct DescriptorArena {
    pools: Vec<vk::DescriptorPool>,
    sets: HashMap<(vk::DescriptorSetLayout, Vec<Bound>), vk::DescriptorSet>,
}
impl DescriptorArena {
    pub(super) fn reset(&mut self, context: &Context) -> Result<()> {
        for &pool in &self.pools {
            context.result(unsafe {
                context
                    .device
                    .reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty())
            })?;
        }
        self.sets.clear();
        Ok(())
    }
    pub(super) fn destroy(&mut self, context: &Context) {
        for pool in self.pools.drain(..) {
            unsafe {
                context.device.destroy_descriptor_pool(pool, None);
            }
        }
    }
    pub(super) fn get(
        &mut self,
        context: &Context,
        layout: vk::DescriptorSetLayout,
        bindings: &[Binding],
        values: &[Bound],
    ) -> Result<(vk::DescriptorSet, bool)> {
        let key = (layout, values.to_vec());
        if let Some(&set) = self.sets.get(&key) {
            return Ok((set, false));
        }
        let allocate = |pool| unsafe {
            context.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&[layout]),
            )
        };
        for &pool in &self.pools {
            match allocate(pool) {
                Ok(sets) => {
                    self.sets.insert(key, sets[0]);
                    return Ok((sets[0], true));
                }
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL) => {}
                Err(error) => return context.result(Err(error)),
            }
        }
        let mut counts = HashMap::new();
        for b in bindings {
            *counts.entry(descriptor(b.kind)).or_insert(0) += 256;
        }
        let sizes: Vec<_> = counts
            .into_iter()
            .map(|(ty, descriptor_count)| vk::DescriptorPoolSize {
                ty,
                descriptor_count,
            })
            .collect();
        let pool = context.result(unsafe {
            context.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(256)
                    .pool_sizes(&sizes),
                None,
            )
        })?;
        self.pools.push(pool);
        let set = context.result(allocate(pool))?[0];
        self.sets.insert(key, set);
        Ok((set, true))
    }
}
