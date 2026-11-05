use super::pipeline::Compiled;
use super::*;
use arrayvec::ArrayVec;
use ash::vk::Handle;

pub(super) fn group(kind: BindingKind) -> u32 {
    match kind {
        BindingKind::UniformBuffer => 0,
        BindingKind::SampledImage { .. } => 1,
        BindingKind::Sampler { .. } => 2,
        BindingKind::StorageImage { .. } | BindingKind::StorageBuffer { .. } => 3,
    }
}
/// Read-only layouts keep framebuffer compression on tilers that drop it in
/// `GENERAL`; storage images stay in `GENERAL`.
pub(super) fn sampled_layout(format: Format) -> vk::ImageLayout {
    if format.has_depth() {
        vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL
    } else {
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
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
/// Where a heap binding's index, and a storage buffer's element offset, live
/// in the push block. Resolved once per compiled pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Field {
    pub(super) word: u32,
    pub(super) shift: u32,
    pub(super) mask: u32,
    pub(super) offset: Option<u32>,
    pub(super) stride: u32,
}
/// Heap indices share words when they fit in 16 bits. Each storage offset
/// has its own full word; register numbers do not consume gaps.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(super) struct PushLayout {
    /// By group, indexed by slot.
    fields: [Vec<Option<Field>>; 4],
    pub(super) words: usize,
}
impl PushLayout {
    /// `bindings` sorted by `(group, slot)`; `strides` are the storage array
    /// strides the shaders declare, by slot.
    pub(super) fn new(
        bindings: &[Binding],
        heap: &Heap,
        bytes: u32,
        strides: &HashMap<u32, u32>,
    ) -> Result<Self> {
        let mut layout = Self::default();
        let mut half = None;
        for binding in bindings {
            let group = group(binding.kind);
            if group == 0 {
                continue;
            }
            let kind = match binding.kind {
                BindingKind::SampledImage { .. } => 0,
                BindingKind::Sampler { .. } => 1,
                BindingKind::StorageImage { .. } => 2,
                _ => 3,
            };
            let packed = heap.counts[kind] <= 65536;
            let (word, shift) = if packed && let Some(word) = half.take() {
                (word, 16)
            } else {
                let word = layout.words as u32;
                layout.words += 1;
                if packed {
                    half = Some(word);
                }
                (word, 0)
            };
            let fields = &mut layout.fields[group as usize];
            let slot = binding.slot as usize;
            if fields.len() <= slot {
                fields.resize(slot + 1, None);
            }
            fields[slot] = Some(Field {
                word,
                shift,
                mask: if packed { 65535 } else { u32::MAX },
                offset: None,
                // A slot no shader reads has no stride; its offset is unused.
                stride: strides.get(&binding.slot).copied().unwrap_or(1),
            });
        }
        for binding in bindings {
            if matches!(binding.kind, BindingKind::StorageBuffer { .. }) {
                let field = layout.fields[3][binding.slot as usize].as_mut().unwrap();
                field.offset = Some(layout.words as u32);
                layout.words += 1;
            }
        }
        if layout.words * 4 > bytes as usize {
            return Err(Error::Invalid(
                "pipeline bindings exceed push-constant capacity".into(),
            ));
        }
        Ok(layout)
    }
    pub(super) fn field(&self, group: u32, slot: u32) -> Option<&Field> {
        self.fields[group as usize].get(slot as usize)?.as_ref()
    }
}

/// Heap entries per kind. Sampled images follow guest content, so they take
/// the most a 16-bit packed index addresses; samplers, storage images and
/// storage buffers are host-owned and few.
const CAPACITY: [u32; 4] = [65536, 1024, 1024, 1024];

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
        let limits = [
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
        ];
        let counts: [u32; 4] = std::array::from_fn(|kind| {
            limits[kind]
                .min(CAPACITY[kind])
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
    #[cfg(test)]
    pub(super) fn used(&self, kind: usize) -> usize {
        self.counts[kind] as usize - self.state.lock().unwrap().free[kind].len()
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

/// One bound resource: the descriptor the fallback tiers write and the heap
/// index and offset the bindless tier pushes. A zero handle is unbound.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) struct Bound {
    handle: u64,
    offset: u64,
    size: u64,
    layout: i32,
    index: u32,
}
impl Bound {
    fn buffer_info(&self) -> vk::DescriptorBufferInfo {
        vk::DescriptorBufferInfo::default()
            .buffer(vk::Buffer::from_raw(self.handle))
            .offset(self.offset)
            .range(self.size)
    }
    fn image_info(&self, kind: BindingKind) -> vk::DescriptorImageInfo {
        if matches!(kind, BindingKind::Sampler { .. }) {
            return vk::DescriptorImageInfo::default().sampler(vk::Sampler::from_raw(self.handle));
        }
        vk::DescriptorImageInfo::default()
            .image_view(vk::ImageView::from_raw(self.handle))
            .image_layout(vk::ImageLayout::from_raw(self.layout))
    }
}

/// Bindings of one pass. Slots persist across pipelines, as D3D registers
/// do. `bind` and `flush` hash nothing, clone nothing and lock only when a
/// slot changes to a resource that needs a layout transition or a new
/// uniform descriptor.
#[derive(Default)]
pub(super) struct Bindings {
    /// By group, indexed by slot.
    slots: [Vec<Bound>; 4],
    /// The pipeline layout `push` follows.
    layout: vk::PipelineLayout,
    // Bindless tier.
    heap_bound: bool,
    push: Vec<u32>,
    push_dirty: (usize, usize),
    uniform_sets: Vec<vk::DescriptorSet>,
    uniform_dirty: u64,
    // Push-descriptor and cached-set tiers.
    dirty: [u128; 4],
    changed: bool,
    pushed_layout: vk::PipelineLayout,
    values: Vec<Bound>,
}
const CLEAN: (usize, usize) = (usize::MAX, 0);
impl Bindings {
    pub(super) fn pipeline(&mut self, compiled: &Compiled) {
        if self.layout == compiled.layout {
            return;
        }
        self.layout = compiled.layout;
        self.changed = true;
        self.push.clear();
        self.push.resize(compiled.push.words, 0);
        for binding in &compiled.bindings {
            let group = group(binding.kind);
            let bound = self.slots[group as usize]
                .get(binding.slot as usize)
                .copied();
            if let (Some(field), Some(bound)) = (compiled.push.field(group, binding.slot), bound) {
                self.encode(field, &bound);
            }
        }
        self.push_dirty = (0, self.push.len());
    }
    fn set_word(&mut self, word: usize, value: u32) {
        if self.push[word] != value {
            self.push[word] = value;
            self.push_dirty = (self.push_dirty.0.min(word), self.push_dirty.1.max(word + 1));
        }
    }
    fn encode(&mut self, field: &Field, bound: &Bound) {
        debug_assert!(bound.index <= field.mask);
        let word = field.word as usize;
        self.set_word(
            word,
            (self.push[word] & !(field.mask << field.shift)) | (bound.index << field.shift),
        );
        if let Some(word) = field.offset {
            let stride = u64::from(field.stride);
            debug_assert!(
                bound.offset.is_multiple_of(stride),
                "storage offset {} is not a multiple of the array stride {stride}",
                bound.offset
            );
            self.set_word(word as usize, (bound.offset / stride) as u32);
        }
    }
    pub(super) fn bind(
        &mut self,
        encoder: &mut Encoder,
        compiled: Option<&Compiled>,
        slot: u32,
        resource: Bind<'_>,
        preamble: Option<vk::CommandBuffer>,
    ) {
        if !encoder.ready() {
            return;
        }
        let (group, bound) = match resource {
            Bind::Uniform(slice) | Bind::StorageBuffer(slice) => {
                let core = &slice.buffer.inner.0;
                let storage = matches!(resource, Bind::StorageBuffer(_));
                debug_assert!(slice.offset.is_multiple_of(u64::from(if storage {
                    encoder.shared.context.caps.limits.storage_offset_alignment
                } else {
                    encoder.shared.context.caps.limits.uniform_offset_alignment
                })));
                (
                    if storage { 3 } else { 0 },
                    Bound {
                        handle: core.handle.as_raw(),
                        offset: slice.offset,
                        size: slice.size,
                        layout: 0,
                        index: if storage {
                            core.storage.unwrap_or(0)
                        } else {
                            0
                        },
                    },
                )
            }
            Bind::Sampled(view) | Bind::StorageImage(view) => {
                let sampled = matches!(resource, Bind::Sampled(_));
                let core = &view.inner.0;
                debug_assert!(
                    !sampled || core.image.inner.core.conversion.is_none(),
                    "YCbCr images need a combined immutable sampler"
                );
                let layout = if sampled {
                    sampled_layout(core.format)
                } else {
                    vk::ImageLayout::GENERAL
                };
                let index = if sampled {
                    match core.sampled_index() {
                        Ok(index) => index,
                        Err(error) => {
                            encoder.error = Some(error);
                            return;
                        }
                    }
                } else {
                    core.storage.unwrap_or(0)
                };
                (
                    if sampled { 1 } else { 3 },
                    Bound {
                        handle: core.handle.as_raw(),
                        offset: 0,
                        size: 0,
                        layout: layout.as_raw(),
                        index,
                    },
                )
            }
            Bind::Sampler(sampler) => (
                2,
                Bound {
                    handle: sampler.inner.0.handle.as_raw(),
                    index: sampler.inner.0.index.unwrap_or(0),
                    ..Bound::default()
                },
            ),
        };
        debug_assert!(slot < 128, "slot {slot}");
        let slots = &mut self.slots[group as usize];
        if slots.len() <= slot as usize {
            slots.resize(slot as usize + 1, Bound::default());
        }
        let old = std::mem::replace(&mut slots[slot as usize], bound);
        if old == bound {
            return;
        }
        if let Bind::Sampled(view) | Bind::StorageImage(view) = resource {
            let layout = vk::ImageLayout::from_raw(bound.layout);
            let desc = &view.inner.0.desc;
            let core = &view.inner.0.image.inner.core;
            let states = core.states.lock().unwrap();
            let stale = desc.layers.clone().any(|layer| {
                desc.mips
                    .clone()
                    .any(|mip| states[(layer * core.desc.mips + mip) as usize].layout != layout)
            });
            drop(states);
            if stale {
                // Inside a render pass the barrier goes to the pass preamble.
                let command = encoder.command;
                if let Some(preamble) = preamble {
                    encoder.command = preamble;
                }
                let write = if group == 1 {
                    vk::AccessFlags2::empty()
                } else {
                    vk::AccessFlags2::SHADER_WRITE
                };
                encoder.view_transition(
                    &view.inner,
                    layout,
                    vk::AccessFlags2::SHADER_READ | write,
                    vk::PipelineStageFlags2::ALL_GRAPHICS | vk::PipelineStageFlags2::COMPUTE_SHADER,
                );
                encoder.command = command;
            }
        }
        let context = &encoder.shared.context;
        let Some(heap) = &context.heap else {
            self.dirty[group as usize] |= 1 << slot;
            self.changed = true;
            return;
        };
        if let Bind::Uniform(slice) = resource {
            // Ring uploads move the offset every draw; the descriptor depends
            // only on the buffer and the block size.
            if old.handle != bound.handle || old.size != bound.size {
                let mut sets = slice.buffer.inner.0.uniforms.lock().unwrap();
                let set = match sets.get(&bound.size) {
                    Some(&set) => set,
                    None => match heap.uniform(context, slice.buffer.inner.0.handle, bound.size) {
                        Ok(set) => *sets.entry(bound.size).or_insert(set),
                        Err(error) => {
                            drop(sets);
                            encoder.error = Some(error);
                            return;
                        }
                    },
                };
                if self.uniform_sets.len() <= slot as usize {
                    self.uniform_sets
                        .resize(slot as usize + 1, vk::DescriptorSet::null());
                }
                self.uniform_sets[slot as usize] = set;
            }
            debug_assert!(slot < 64);
            self.uniform_dirty |= 1 << slot;
        } else if let Some(field) = compiled.and_then(|c| c.push.field(group, slot)) {
            self.encode(field, &bound);
        }
    }
    /// Makes the bindings visible to the next draw or dispatch. False if the
    /// encoder failed.
    pub(super) fn flush(
        &mut self,
        encoder: &mut Encoder,
        compiled: &Compiled,
        point: vk::PipelineBindPoint,
    ) -> bool {
        if !encoder.ready() {
            return false;
        }
        debug_assert!(
            compiled
                .bindings
                .iter()
                .all(|b| self.slots[group(b.kind) as usize]
                    .get(b.slot as usize)
                    .is_some_and(|v| v.handle != 0)),
            "a pipeline binding has no resource"
        );
        if encoder.shared.context.heap.is_some() {
            self.flush_heap(&encoder.shared.context, encoder.command, compiled, point);
            return true;
        }
        if !self.changed || compiled.bindings.is_empty() {
            return true;
        }
        if let Err(error) = self.flush_sets(encoder, compiled, point) {
            encoder.error = Some(error);
            return false;
        }
        true
    }
    fn flush_heap(
        &mut self,
        context: &Context,
        command: vk::CommandBuffer,
        compiled: &Compiled,
        point: vk::PipelineBindPoint,
    ) {
        let device = &context.device;
        unsafe {
            if !self.heap_bound {
                let set = context.heap.as_ref().unwrap().set;
                device.cmd_bind_descriptor_sets(command, point, compiled.layout, 0, &[set], &[]);
                self.heap_bound = true;
            }
            let mut uniforms = self.uniform_dirty & compiled.uniforms;
            self.uniform_dirty &= !uniforms;
            while uniforms != 0 {
                let slot = uniforms.trailing_zeros();
                uniforms &= uniforms - 1;
                let offset = self.slots[0][slot as usize].offset;
                debug_assert!(offset <= u64::from(u32::MAX));
                device.cmd_bind_descriptor_sets(
                    command,
                    point,
                    compiled.layout,
                    slot + 1,
                    &[self.uniform_sets[slot as usize]],
                    &[offset as u32],
                );
            }
            let (start, end) = std::mem::replace(&mut self.push_dirty, CLEAN);
            if start < end {
                let words = &self.push[start..end];
                // SAFETY: initialized u32 words, copied by the driver now.
                let bytes = std::slice::from_raw_parts(
                    words.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(words),
                );
                device.cmd_push_constants(
                    command,
                    compiled.layout,
                    vk::ShaderStageFlags::ALL,
                    (start * 4) as u32,
                    bytes,
                );
            }
        }
    }
    fn flush_sets(
        &mut self,
        encoder: &mut Encoder,
        compiled: &Compiled,
        point: vk::PipelineBindPoint,
    ) -> Result<()> {
        let push = encoder
            .shared
            .context
            .caps
            .modern
            .contains(Modern::PUSH_DESCRIPTORS);
        let all = !push || self.pushed_layout != compiled.layout;
        let set = if push {
            vk::DescriptorSet::null()
        } else {
            // The set layout leads the key, so one arena serves every pipeline.
            self.values.clear();
            self.values.push(Bound {
                handle: compiled.set_layout.as_raw(),
                ..Bound::default()
            });
            for b in &compiled.bindings {
                self.values
                    .push(self.slots[group(b.kind) as usize][b.slot as usize]);
            }
            let (set, new) = encoder.descriptor_set(compiled.set_layout, &self.values)?;
            if !new {
                unsafe {
                    encoder.shared.context.device.cmd_bind_descriptor_sets(
                        encoder.command,
                        point,
                        compiled.layout,
                        0,
                        &[set],
                        &[],
                    );
                }
                self.changed = false;
                return Ok(());
            }
            set
        };
        let context = &encoder.shared.context;
        let written = compiled
            .bindings
            .iter()
            .filter(|b| all || self.dirty[group(b.kind) as usize] & (1 << b.slot) != 0);
        // Fixed-size batches keep the descriptor infos on the stack.
        let mut buffers = ArrayVec::<vk::DescriptorBufferInfo, 32>::new();
        let mut images = ArrayVec::<vk::DescriptorImageInfo, 32>::new();
        let mut batch = ArrayVec::<&Binding, 32>::new();
        let mut written = written.peekable();
        while written.peek().is_some() {
            batch.clear();
            buffers.clear();
            images.clear();
            batch.extend(written.by_ref().take(32));
            for b in &batch {
                let bound = &self.slots[group(b.kind) as usize][b.slot as usize];
                match b.kind {
                    BindingKind::UniformBuffer | BindingKind::StorageBuffer { .. } => {
                        buffers.push(bound.buffer_info())
                    }
                    kind => images.push(bound.image_info(kind)),
                }
            }
            let (mut nb, mut ni) = (0, 0);
            let writes: ArrayVec<_, 32> = batch
                .iter()
                .map(|b| {
                    let write = vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(b.slot * 4 + group(b.kind))
                        .descriptor_type(descriptor(b.kind));
                    match b.kind {
                        BindingKind::UniformBuffer | BindingKind::StorageBuffer { .. } => {
                            nb += 1;
                            write.buffer_info(&buffers[nb - 1..nb])
                        }
                        _ => {
                            ni += 1;
                            write.image_info(&images[ni - 1..ni])
                        }
                    }
                })
                .collect();
            unsafe {
                if push {
                    context.push.cmd_push_descriptor_set(
                        encoder.command,
                        point,
                        compiled.layout,
                        0,
                        &writes,
                    );
                } else {
                    context.device.update_descriptor_sets(&writes, &[]);
                }
            }
        }
        if !push {
            unsafe {
                context.device.cmd_bind_descriptor_sets(
                    encoder.command,
                    point,
                    compiled.layout,
                    0,
                    &[set],
                    &[],
                );
            }
        }
        self.changed = false;
        self.dirty = [0; 4];
        self.pushed_layout = compiled.layout;
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct DescriptorArena {
    pools: Vec<vk::DescriptorPool>,
    /// Keyed by the set layout followed by the bound values.
    sets: HashMap<Vec<Bound>, vk::DescriptorSet>,
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
        key: &[Bound],
    ) -> Result<(vk::DescriptorSet, bool)> {
        if let Some(&set) = self.sets.get(key) {
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
                    self.sets.insert(key.to_vec(), sets[0]);
                    return Ok((sets[0], true));
                }
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL) => {}
                Err(error) => return context.result(Err(error)),
            }
        }
        // Every pool holds every kind: a set may only come from a pool sized
        // for its descriptor types, whichever layout first needed a pool.
        let sizes = [
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLER,
            vk::DescriptorType::STORAGE_IMAGE,
            vk::DescriptorType::STORAGE_BUFFER,
        ]
        .map(|ty| vk::DescriptorPoolSize {
            ty,
            descriptor_count: 1024,
        });
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
        self.sets.insert(key.to_vec(), set);
        Ok((set, true))
    }
}
