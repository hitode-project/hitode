use super::*;
use naga::{Expression as E, Handle, Span, Type, TypeInner};
use std::ffi::CString;

#[derive(Clone)]
pub(crate) struct Shader(Arc<ShaderCore>);
struct ShaderCore {
    context: Arc<Context>,
    identity: u64,
    module: vk::ShaderModule,
    entries: Vec<(CString, Stage)>,
    uniforms: Vec<HashMap<u32, u64>>,
    storage_strides: Vec<HashMap<u32, u32>>,
    heap: Option<HeapShader>,
}
impl Drop for ShaderCore {
    fn drop(&mut self) {
        unsafe {
            self.context.device.destroy_shader_module(self.module, None);
        }
    }
}
impl Shader {
    pub(crate) fn entry(&self, name: &str, stage: Stage) -> Result<Entry> {
        let index = self
            .0
            .entries
            .iter()
            .position(|(n, s)| n.as_bytes() == name.as_bytes() && *s == stage)
            .ok_or_else(|| Error::Compile(format!("{stage:?} entry {name:?} not found")))?;
        Ok(Entry {
            shader: crate::Shader {
                identity: self.0.identity,
                inner: self.clone(),
            },
            index: index as u32,
            stage,
        })
    }
    pub(super) fn stage<'a>(
        &'a self,
        entry: &'a Entry,
        layout: Option<&binding::PushLayout>,
    ) -> Result<ShaderStage<'a>> {
        let (module, temporary) = if let (Some(heap), Some(layout)) = (&self.0.heap, layout) {
            let mut module = heap.module.clone();
            for patch in &heap.patches {
                let field = layout.bindings.get(&patch.binding);
                let value = field.map_or(0, |field| match patch.part {
                    Part::Word => field.word,
                    Part::Shift => field.shift,
                    Part::Offset => field.offset.unwrap(),
                });
                module.global_expressions[patch.init] = E::Literal(naga::Literal::U32(value));
            }
            let words = lower(
                &module,
                Some(&naga::back::spv::PipelineOptions {
                    shader_stage: match entry.stage {
                        Stage::Vertex => naga::ShaderStage::Vertex,
                        Stage::Fragment => naga::ShaderStage::Fragment,
                        Stage::Compute => naga::ShaderStage::Compute,
                    },
                    entry_point: self.0.entries[entry.index as usize]
                        .0
                        .to_str()
                        .unwrap()
                        .into(),
                }),
                true,
            )?;
            (
                self.0.context.result(unsafe {
                    self.0.context.device.create_shader_module(
                        &vk::ShaderModuleCreateInfo::default().code(&words),
                        None,
                    )
                })?,
                true,
            )
        } else {
            (self.0.module, false)
        };
        Ok(ShaderStage {
            shader: self,
            entry,
            module,
            temporary,
        })
    }
    pub(super) fn storage_strides(&self, index: u32) -> &HashMap<u32, u32> {
        &self.0.storage_strides[index as usize]
    }
    pub(super) fn uniforms(&self, index: u32) -> &HashMap<u32, u64> {
        &self.0.uniforms[index as usize]
    }
}
pub(super) struct ShaderStage<'a> {
    shader: &'a Shader,
    entry: &'a Entry,
    module: vk::ShaderModule,
    temporary: bool,
}
impl ShaderStage<'_> {
    pub(super) fn info(&self) -> vk::PipelineShaderStageCreateInfo<'_> {
        vk::PipelineShaderStageCreateInfo::default()
            .module(self.module)
            .name(&self.shader.0.entries[self.entry.index as usize].0)
            .stage(match self.entry.stage {
                Stage::Vertex => vk::ShaderStageFlags::VERTEX,
                Stage::Fragment => vk::ShaderStageFlags::FRAGMENT,
                Stage::Compute => vk::ShaderStageFlags::COMPUTE,
            })
    }
}
impl Drop for ShaderStage<'_> {
    fn drop(&mut self) {
        if self.temporary {
            unsafe {
                self.shader
                    .0
                    .context
                    .device
                    .destroy_shader_module(self.module, None);
            }
        }
    }
}
// Resource accesses are prepared at shader creation. The pipeline resolves
// word positions and shifts from its complete binding list before stage lowering.
struct HeapShader {
    module: naga::Module,
    patches: Vec<Patch>,
}
struct Patch {
    init: Handle<E>,
    binding: (u32, u32),
    part: Part,
}
#[derive(Clone, Copy)]
enum Part {
    Word,
    Shift,
    Offset,
}
fn lower(
    module: &naga::Module,
    entry: Option<&naga::back::spv::PipelineOptions>,
    heap: bool,
) -> Result<Vec<u32>> {
    let flags = if heap {
        naga::valid::ValidationFlags::all() - naga::valid::ValidationFlags::BINDINGS
    } else {
        naga::valid::ValidationFlags::all()
    };
    let info = validate(module, flags)?;
    naga::back::spv::write_vec(
        module,
        &info,
        &naga::back::spv::Options {
            lang_version: (1, 3),
            ..Default::default()
        },
        entry,
    )
    .map_err(|e| Error::Compile(format!("SPIR-V lowering: {e}")))
}
#[derive(Clone)]
struct StorageShape {
    stride: u32,
    path: Option<Vec<u32>>,
}
fn storage_shape(
    module: &naga::Module,
    ty: Handle<Type>,
    layouter: &naga::proc::Layouter,
) -> StorageShape {
    fn array(module: &naga::Module, ty: Handle<Type>, path: Vec<u32>) -> Option<(u32, Vec<u32>)> {
        match &module.types[ty].inner {
            TypeInner::Array { stride, .. } => Some((*stride, path)),
            TypeInner::Struct { members, .. } => members.iter().enumerate().find_map(|(i, m)| {
                let mut path = path.clone();
                path.push(i as u32);
                array(module, m.ty, path)
            }),
            _ => None,
        }
    }
    if let Some((stride, path)) = array(module, ty, Vec::new()) {
        StorageShape {
            stride,
            path: Some(path),
        }
    } else {
        StorageShape {
            stride: layouter[ty].alignment.round_up(layouter[ty].size),
            path: None,
        }
    }
}
fn validate(
    module: &naga::Module,
    flags: naga::valid::ValidationFlags,
) -> Result<naga::valid::ModuleInfo> {
    naga::valid::Validator::new(flags, naga::valid::Capabilities::all())
        .validate(module)
        .map_err(|e| Error::Compile(format!("shader validation: {e:?}")))
}
impl Device {
    pub(crate) fn create_shader(&self, desc: ShaderDesc<'_>) -> Result<crate::Shader> {
        self.0.check()?;
        let (mut module, precompiled) = match desc.source {
            Source::Wgsl(text) => (
                naga::front::wgsl::parse_str(text)
                    .map_err(|e| Error::Compile(e.emit_to_string(text)))?,
                None,
            ),
            Source::Ir(module) => (*module, None),
            Source::Precompiled { spirv, .. } => {
                let module = naga::front::spv::Frontend::new(
                    spirv.iter().copied(),
                    &naga::front::spv::Options::default(),
                )
                .parse()
                .map_err(|e| Error::Compile(format!("SPIR-V: {e}")))?;
                (module, Some(spirv))
            }
        };
        let info = validate(&module, naga::valid::ValidationFlags::all())?;
        let entries: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| {
                let stage = match e.stage {
                    naga::ShaderStage::Vertex => Stage::Vertex,
                    naga::ShaderStage::Fragment => Stage::Fragment,
                    naga::ShaderStage::Compute => Stage::Compute,
                    _ => return Err(Error::Compile("unsupported shader stage".into())),
                };
                Ok((
                    CString::new(e.name.as_str())
                        .map_err(|_| Error::Compile("entry name contains NUL".into()))?,
                    stage,
                ))
            })
            .collect::<Result<_>>()?;
        let mut layouter = naga::proc::Layouter::default();
        layouter
            .update(module.to_ctx())
            .map_err(|e| Error::Compile(format!("shader layout: {e}")))?;
        let mut uniforms = vec![HashMap::new(); entries.len()];
        let mut storage_strides = vec![HashMap::new(); entries.len()];
        for (handle, global) in module.global_variables.iter() {
            if let Some(binding) = &global.binding {
                if binding.group > 3 {
                    return Err(Error::Compile("resource group must be 0..3".into()));
                }
                if binding.group == 0 {
                    for (index, uniforms) in uniforms.iter_mut().enumerate() {
                        if !info.get_entry_point(index)[handle].is_empty() {
                            uniforms.insert(binding.binding, u64::from(layouter[global.ty].size));
                        }
                    }
                }
                if matches!(global.space, naga::AddressSpace::Storage { .. }) {
                    let stride = storage_shape(&module, global.ty, &layouter).stride;
                    for (index, strides) in storage_strides.iter_mut().enumerate() {
                        if !info.get_entry_point(index)[handle].is_empty() {
                            strides.insert(binding.binding, stride);
                        }
                    }
                }
            }
        }
        // Build-time SPIR-V already uses this backend's convention. Modules with
        // declared groups are lowered exactly like IR, including heap rewriting.
        let already_lowered = precompiled.is_some()
            && module
                .global_variables
                .iter()
                .all(|(_, g)| g.binding.is_none());
        let mut patches = Vec::new();
        let words = if already_lowered {
            precompiled.unwrap().to_vec()
        } else {
            if let Some(heap) = &self.0.heap {
                patches = rewrite_heap(
                    &mut module,
                    self.caps().limits.push_constant_bytes / 4,
                    self.caps().limits.max_uniform_buffers,
                    heap.counts,
                )?;
            } else {
                for (_, global) in module.global_variables.iter_mut() {
                    if let Some(binding) = &mut global.binding {
                        binding.binding = binding.binding * 4 + binding.group;
                        binding.group = 0;
                    }
                }
            }
            lower(&module, None, self.0.heap.is_some())?
        };
        let heap = self.0.heap.as_ref().map(|_| HeapShader { module, patches });
        let handle = self.0.result(unsafe {
            self.0
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        })?;
        Ok(crate::Shader {
            identity: desc.identity,
            inner: Shader(Arc::new(ShaderCore {
                context: self.0.clone(),
                identity: desc.identity,
                module: handle,
                entries,
                uniforms,
                storage_strides,
                heap,
            })),
        })
    }
}

fn rewrite_heap(
    module: &mut naga::Module,
    words: u32,
    uniforms: u32,
    counts: [u32; 4],
) -> Result<Vec<Patch>> {
    let mut patches = Vec::new();
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|e| Error::Compile(e.to_string()))?;
    let shapes: HashMap<_, _> = module
        .global_variables
        .iter()
        .filter(|(_, g)| matches!(g.space, naga::AddressSpace::Storage { .. }))
        .map(|(h, g)| (h, storage_shape(module, g.ty, &layouter)))
        .collect();
    let uint = module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Scalar(naga::Scalar::U32),
        },
        Span::UNDEFINED,
    );
    let array = module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Array {
                base: uint,
                size: naga::ArraySize::Constant(std::num::NonZeroU32::new(words).unwrap()),
                stride: 4,
            },
        },
        Span::UNDEFINED,
    );
    let ty = module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Struct {
                members: vec![naga::StructMember {
                    name: None,
                    ty: array,
                    binding: None,
                    offset: 0,
                }],
                span: words * 4,
            },
        },
        Span::UNDEFINED,
    );
    let push = module.global_variables.append(
        naga::GlobalVariable {
            name: Some("binding_indices".into()),
            space: naga::AddressSpace::Immediate,
            binding: None,
            ty,
            init: None,
            memory_decorations: Default::default(),
        },
        Span::UNDEFINED,
    );
    let mut globals = HashMap::new();
    for (handle, global) in module.global_variables.iter_mut() {
        let Some(binding) = &mut global.binding else {
            continue;
        };
        let group = binding.group;
        let slot = binding.binding;
        if group == 0 {
            if slot >= uniforms {
                return Err(Error::Compile(
                    "uniform slot exceeds device descriptor-set limit".into(),
                ));
            }
            binding.group = slot + 1;
            binding.binding = 0;
            continue;
        }
        let key = (group, slot);
        let mut constant = |part| {
            let init = module
                .global_expressions
                .append(E::Literal(naga::Literal::U32(0)), Span::UNDEFINED);
            patches.push(Patch {
                init,
                binding: key,
                part,
            });
            module.constants.append(
                naga::Constant {
                    name: None,
                    ty: uint,
                    init,
                },
                Span::UNDEFINED,
            )
        };
        let word = constant(Part::Word);
        let shift = constant(Part::Shift);
        let offset = shapes.get(&handle).map(|_| constant(Part::Offset));
        binding.group = 0;
        binding.binding = match group {
            1 => 0,
            2 => 1,
            3 if matches!(module.types[global.ty].inner, TypeInner::Image { .. }) => 2,
            3 => 3,
            _ => unreachable!(),
        };
        let shape = shapes.get(&handle).cloned();
        let element = shape.as_ref().is_some_and(|s| s.path.is_none());
        let wrapped = shape.is_some()
            && (element || !matches!(module.types[global.ty].inner, TypeInner::Struct { .. }));
        let base_type = if element {
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Array {
                        base: global.ty,
                        size: naga::ArraySize::Dynamic,
                        stride: shape.as_ref().unwrap().stride,
                    },
                },
                Span::UNDEFINED,
            )
        } else if matches!(module.types[global.ty].inner, TypeInner::Array { .. }) {
            let TypeInner::Array { base, stride, .. } = module.types[global.ty].inner else {
                unreachable!()
            };
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Array {
                        base,
                        size: naga::ArraySize::Dynamic,
                        stride,
                    },
                },
                Span::UNDEFINED,
            )
        } else {
            global.ty
        };
        let base = if wrapped {
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Struct {
                        members: vec![naga::StructMember {
                            name: None,
                            ty: base_type,
                            binding: None,
                            offset: 0,
                        }],
                        span: if element {
                            shape.as_ref().unwrap().stride
                        } else {
                            layouter[global.ty].size
                        },
                    },
                },
                Span::UNDEFINED,
            )
        } else {
            base_type
        };
        let array = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::BindingArray {
                    base,
                    size: naga::ArraySize::Constant(
                        std::num::NonZeroU32::new(counts[binding.binding as usize]).unwrap(),
                    ),
                },
            },
            Span::UNDEFINED,
        );
        global.ty = array;
        globals.insert(
            handle,
            Global {
                word,
                shift,
                offset,
                mask: if counts[binding.binding as usize] <= 65536 {
                    65535
                } else {
                    u32::MAX
                },
                wrapped,
                shape,
            },
        );
    }
    for (_, function) in module.functions.iter_mut() {
        rewrite_function(function, &globals, push);
    }
    for entry in &mut module.entry_points {
        rewrite_function(&mut entry.function, &globals, push);
    }
    Ok(patches)
}
struct Global {
    word: Handle<naga::Constant>,
    shift: Handle<naga::Constant>,
    offset: Option<Handle<naga::Constant>>,
    mask: u32,
    wrapped: bool,
    shape: Option<StorageShape>,
}
fn rewrite_function(
    function: &mut naga::Function,
    globals: &HashMap<Handle<naga::GlobalVariable>, Global>,
    push: Handle<naga::GlobalVariable>,
) {
    let old = std::mem::take(&mut function.expressions);
    let mut replacements = HashMap::new();
    let mut offsets = HashMap::new();
    let mut prefix = naga::Block::new();
    for (_, expr) in old.iter() {
        let E::GlobalVariable(global) = *expr else {
            continue;
        };
        let Some(resource) = globals.get(&global) else {
            continue;
        };
        if replacements.contains_key(&global) {
            continue;
        }
        let arena = &mut function.expressions;
        let base = arena.append(E::GlobalVariable(global), Span::UNDEFINED);
        let pc = arena.append(E::GlobalVariable(push), Span::UNDEFINED);
        let word = arena.append(E::Constant(resource.word), Span::UNDEFINED);
        let shift = arena.append(E::Constant(resource.shift), Span::UNDEFINED);
        let mask = arena.append(
            E::Literal(naga::Literal::U32(resource.mask)),
            Span::UNDEFINED,
        );
        let offset_word = resource
            .offset
            .map(|constant| arena.append(E::Constant(constant), Span::UNDEFINED));
        let start = arena.len();
        let member = arena.append(E::AccessIndex { base: pc, index: 0 }, Span::UNDEFINED);
        let pointer = arena.append(
            E::Access {
                base: member,
                index: word,
            },
            Span::UNDEFINED,
        );
        let index = arena.append(E::Load { pointer }, Span::UNDEFINED);
        let index = arena.append(
            E::Binary {
                op: naga::BinaryOperator::ShiftRight,
                left: index,
                right: shift,
            },
            Span::UNDEFINED,
        );
        let index = arena.append(
            E::Binary {
                op: naga::BinaryOperator::And,
                left: index,
                right: mask,
            },
            Span::UNDEFINED,
        );
        let access = arena.append(E::Access { base, index }, Span::UNDEFINED);
        let mut access = if resource.wrapped {
            arena.append(
                E::AccessIndex {
                    base: access,
                    index: 0,
                },
                Span::UNDEFINED,
            )
        } else {
            access
        };
        if let Some(word) = offset_word {
            let pointer = arena.append(
                E::Access {
                    base: member,
                    index: word,
                },
                Span::UNDEFINED,
            );
            let offset = arena.append(E::Load { pointer }, Span::UNDEFINED);
            offsets.insert(global, offset);
            if resource.shape.as_ref().unwrap().path.is_none() {
                access = arena.append(
                    E::Access {
                        base: access,
                        index: offset,
                    },
                    Span::UNDEFINED,
                );
            }
        }
        prefix.push(
            naga::Statement::Emit(naga::Range::from_index_range(
                start as u32..arena.len() as u32,
                arena,
            )),
            Span::UNDEFINED,
        );
        replacements.insert(global, access);
    }
    // Track the original pointers to each outer storage array. Nested indices
    // retain their original meaning; the span offset is applied exactly once.
    let mut paths: HashMap<H, (Handle<naga::GlobalVariable>, Vec<u32>)> = HashMap::new();
    let mut arrays = HashMap::new();
    let mut literals = HashMap::new();
    for (handle, expr) in old.iter() {
        let path = match *expr {
            E::GlobalVariable(global) if offsets.contains_key(&global) => {
                Some((global, Vec::new()))
            }
            E::AccessIndex { base, index } => paths.get(&base).map(|(g, path)| {
                let mut path = path.clone();
                path.push(index);
                (*g, path)
            }),
            _ => None,
        };
        if let Some((global, path)) = path {
            if globals[&global].shape.as_ref().unwrap().path.as_ref() == Some(&path) {
                arrays.insert(handle, global);
            }
            paths.insert(handle, (global, path));
        }
        if let E::AccessIndex { base, index } = *expr
            && arrays.contains_key(&base)
        {
            literals.insert(
                handle,
                function
                    .expressions
                    .append(E::Literal(naga::Literal::U32(index)), Span::UNDEFINED),
            );
        }
    }
    let mut map = Vec::new();
    let mut starts = Vec::new();
    for (handle, expr) in old.iter() {
        if let E::GlobalVariable(global) = expr
            && let Some(&replacement) = replacements.get(global)
        {
            map.push(replacement);
            starts.push(replacement.index() as u32);
            continue;
        }
        let start = function.expressions.len() as u32;
        let mut expr = expr.clone();
        map_expression(&mut expr, &map);
        match old[handle] {
            E::Access { base, .. } | E::AccessIndex { base, .. } if arrays.contains_key(&base) => {
                let index = match expr {
                    E::Access { index, .. } => index,
                    _ => literals[&handle],
                };
                let index = function.expressions.append(
                    E::Binary {
                        op: naga::BinaryOperator::Add,
                        left: index,
                        right: offsets[&arrays[&base]],
                    },
                    Span::UNDEFINED,
                );
                expr = E::Access {
                    base: map[base.index()],
                    index,
                };
            }
            E::ArrayLength(base) if arrays.contains_key(&base) => {
                let length = function.expressions.append(expr, Span::UNDEFINED);
                expr = E::Binary {
                    op: naga::BinaryOperator::Subtract,
                    left: length,
                    right: offsets[&arrays[&base]],
                };
            }
            _ => {}
        }
        starts.push(start);
        map.push(function.expressions.append(expr, Span::UNDEFINED));
    }
    for (_, local) in function.local_variables.iter_mut() {
        if let Some(init) = &mut local.init {
            *init = map[init.index()];
        }
    }
    function.named_expressions.clear();
    map_block(&mut function.body, &map, &starts, &function.expressions);
    prefix.extend_block(std::mem::take(&mut function.body));
    function.body = prefix;
}
type H = Handle<E>;
fn map_expression(expr: &mut E, map: &[H]) {
    let m = |h: &mut H| *h = map[h.index()];
    let o = |h: &mut Option<H>| {
        if let Some(h) = h {
            m(h);
        }
    };
    match expr {
        E::Compose { components, .. } => {
            for h in components {
                m(h);
            }
        }
        E::Access { base, index } => {
            m(base);
            m(index);
        }
        E::AccessIndex { base, .. } => m(base),
        E::Splat { value, .. } => m(value),
        E::Swizzle { vector, .. } => m(vector),
        E::Load { pointer } => m(pointer),
        E::ImageSample {
            image,
            sampler,
            coordinate,
            array_index,
            offset,
            level,
            depth_ref,
            ..
        } => {
            m(image);
            m(sampler);
            m(coordinate);
            o(array_index);
            o(offset);
            o(depth_ref);
            match level {
                naga::SampleLevel::Exact(h) | naga::SampleLevel::Bias(h) => m(h),
                naga::SampleLevel::Gradient { x, y } => {
                    m(x);
                    m(y);
                }
                _ => {}
            }
        }
        E::ImageLoad {
            image,
            coordinate,
            array_index,
            sample,
            level,
        } => {
            m(image);
            m(coordinate);
            o(array_index);
            o(sample);
            o(level);
        }
        E::ImageQuery { image, query } => {
            m(image);
            if let naga::ImageQuery::Size { level } = query {
                o(level);
            }
        }
        E::Unary { expr, .. }
        | E::Derivative { expr, .. }
        | E::As { expr, .. }
        | E::ArrayLength(expr) => m(expr),
        E::Binary { left, right, .. } => {
            m(left);
            m(right);
        }
        E::Select {
            condition,
            accept,
            reject,
        } => {
            m(condition);
            m(accept);
            m(reject);
        }
        E::Relational { argument, .. } => m(argument),
        E::Math {
            arg,
            arg1,
            arg2,
            arg3,
            ..
        } => {
            m(arg);
            o(arg1);
            o(arg2);
            o(arg3);
        }
        E::RayQueryGetIntersection { query, .. } | E::RayQueryVertexPositions { query, .. } => {
            m(query)
        }
        E::CooperativeLoad { data, .. } => {
            m(&mut data.pointer);
            m(&mut data.stride);
        }
        E::CooperativeMultiplyAdd { a, b, c } => {
            m(a);
            m(b);
            m(c);
        }
        _ => {}
    }
}
fn map_block(block: &mut naga::Block, map: &[H], starts: &[u32], arena: &naga::Arena<E>) {
    let m = |h: &mut H| *h = map[h.index()];
    let o = |h: &mut Option<H>| {
        if let Some(h) = h {
            m(h);
        }
    };
    for stmt in block.iter_mut() {
        use naga::Statement as S;
        match stmt {
            S::Emit(range) => {
                if let Some((a, b)) = range.first_and_last() {
                    *range = naga::Range::from_index_range(
                        starts[a.index()]..map[b.index()].index() as u32 + 1,
                        arena,
                    );
                }
            }
            S::Block(b) => map_block(b, map, starts, arena),
            S::If {
                condition,
                accept,
                reject,
            } => {
                m(condition);
                map_block(accept, map, starts, arena);
                map_block(reject, map, starts, arena);
            }
            S::Switch { selector, cases } => {
                m(selector);
                for c in cases {
                    map_block(&mut c.body, map, starts, arena);
                }
            }
            S::Loop {
                body,
                continuing,
                break_if,
            } => {
                map_block(body, map, starts, arena);
                map_block(continuing, map, starts, arena);
                o(break_if);
            }
            S::Return { value } => o(value),
            S::Store { pointer, value } => {
                m(pointer);
                m(value);
            }
            S::ImageStore {
                image,
                coordinate,
                array_index,
                value,
            }
            | S::ImageAtomic {
                image,
                coordinate,
                array_index,
                value,
                ..
            } => {
                m(image);
                m(coordinate);
                o(array_index);
                m(value);
            }
            S::Atomic {
                pointer,
                fun,
                value,
                result,
            } => {
                m(pointer);
                m(value);
                o(result);
                if let naga::AtomicFunction::Exchange { compare } = fun {
                    o(compare);
                }
            }
            S::WorkGroupUniformLoad { pointer, result } => {
                m(pointer);
                m(result);
            }
            S::Call {
                arguments, result, ..
            } => {
                for h in arguments {
                    m(h);
                }
                o(result);
            }
            S::RayQuery { query, fun } => {
                m(query);
                match fun {
                    naga::RayQueryFunction::Initialize {
                        acceleration_structure,
                        descriptor,
                    } => {
                        m(acceleration_structure);
                        m(descriptor);
                    }
                    naga::RayQueryFunction::Proceed { result } => m(result),
                    naga::RayQueryFunction::GenerateIntersection { hit_t } => m(hit_t),
                    _ => {}
                }
            }
            S::SubgroupBallot { result, predicate } => {
                m(result);
                o(predicate);
            }
            S::SubgroupCollectiveOperation {
                argument, result, ..
            } => {
                m(argument);
                m(result);
            }
            S::SubgroupGather {
                mode,
                argument,
                result,
            } => {
                m(argument);
                m(result);
                match mode {
                    naga::GatherMode::Broadcast(h)
                    | naga::GatherMode::Shuffle(h)
                    | naga::GatherMode::ShuffleDown(h)
                    | naga::GatherMode::ShuffleUp(h)
                    | naga::GatherMode::ShuffleXor(h)
                    | naga::GatherMode::QuadBroadcast(h) => m(h),
                    _ => {}
                }
            }
            S::CooperativeStore { target, data } => {
                m(target);
                m(&mut data.pointer);
                m(&mut data.stride);
            }
            S::RayPipelineFunction(naga::RayPipelineFunction::TraceRay {
                acceleration_structure,
                descriptor,
                payload,
            }) => {
                m(acceleration_structure);
                m(descriptor);
                m(payload);
            }
            _ => {}
        }
    }
}
