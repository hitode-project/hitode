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
    pub(super) fn stage(&self, entry: &Entry) -> vk::PipelineShaderStageCreateInfo<'_> {
        vk::PipelineShaderStageCreateInfo::default()
            .module(self.0.module)
            .name(&self.0.entries[entry.index as usize].0)
            .stage(match entry.stage {
                Stage::Vertex => vk::ShaderStageFlags::VERTEX,
                Stage::Fragment => vk::ShaderStageFlags::FRAGMENT,
                Stage::Compute => vk::ShaderStageFlags::COMPUTE,
            })
    }
    pub(super) fn uniforms(&self, index: u32) -> &HashMap<u32, u64> {
        &self.0.uniforms[index as usize]
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
            }
        }
        // Build-time SPIR-V already uses this backend's convention. Modules with
        // declared groups are lowered exactly like IR, including heap rewriting.
        let already_lowered = precompiled.is_some()
            && module
                .global_variables
                .iter()
                .all(|(_, g)| g.binding.is_none());
        let words = if already_lowered {
            precompiled.unwrap().to_vec()
        } else {
            if let Some(heap) = &self.0.heap {
                rewrite_heap(
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
            let info = if self.0.heap.is_some() {
                // Distinct typed globals intentionally alias the same heap binding.
                // User bindings were checked above; retain all other IR checks here.
                validate(
                    &module,
                    naga::valid::ValidationFlags::all() - naga::valid::ValidationFlags::BINDINGS,
                )?
            } else {
                info
            };
            naga::back::spv::write_vec(
                &module,
                &info,
                &naga::back::spv::Options {
                    lang_version: (1, 3),
                    ..Default::default()
                },
                None,
            )
            .map_err(|e| Error::Compile(format!("SPIR-V lowering: {e}")))?
        };
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
            })),
        })
    }
}

fn rewrite_heap(
    module: &mut naga::Module,
    words: u32,
    uniforms: u32,
    counts: [u32; 4],
) -> Result<()> {
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
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|e| Error::Compile(e.to_string()))?;
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
        let word = binding::word(group, slot);
        if word >= words {
            return Err(Error::Compile(
                "resource slot exceeds push-constant limit".into(),
            ));
        }
        binding.group = 0;
        binding.binding = match group {
            1 => 0,
            2 => 1,
            3 if matches!(module.types[global.ty].inner, TypeInner::Image { .. }) => 2,
            3 => 3,
            _ => unreachable!(),
        };
        let wrapped = matches!(global.space, naga::AddressSpace::Storage { .. })
            && !matches!(module.types[global.ty].inner, TypeInner::Struct { .. });
        let base = if wrapped {
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Struct {
                        members: vec![naga::StructMember {
                            name: None,
                            ty: global.ty,
                            binding: None,
                            offset: 0,
                        }],
                        span: layouter[global.ty].size,
                    },
                },
                Span::UNDEFINED,
            )
        } else {
            global.ty
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
        globals.insert(handle, (word, wrapped));
    }
    for (_, function) in module.functions.iter_mut() {
        rewrite_function(function, &globals, push);
    }
    for entry in &mut module.entry_points {
        rewrite_function(&mut entry.function, &globals, push);
    }
    Ok(())
}
fn rewrite_function(
    function: &mut naga::Function,
    globals: &HashMap<Handle<naga::GlobalVariable>, (u32, bool)>,
    push: Handle<naga::GlobalVariable>,
) {
    let old = std::mem::take(&mut function.expressions);
    let mut replacements = HashMap::new();
    let mut prefix = naga::Block::new();
    for (_, expr) in old.iter() {
        let E::GlobalVariable(global) = *expr else {
            continue;
        };
        let Some(&(word, wrapped)) = globals.get(&global) else {
            continue;
        };
        if replacements.contains_key(&global) {
            continue;
        }
        let arena = &mut function.expressions;
        let base = arena.append(E::GlobalVariable(global), Span::UNDEFINED);
        let pc = arena.append(E::GlobalVariable(push), Span::UNDEFINED);
        let member = arena.append(E::AccessIndex { base: pc, index: 0 }, Span::UNDEFINED);
        let pointer = arena.append(
            E::AccessIndex {
                base: member,
                index: word,
            },
            Span::UNDEFINED,
        );
        let index = arena.append(E::Load { pointer }, Span::UNDEFINED);
        let access = arena.append(E::Access { base, index }, Span::UNDEFINED);
        let access = if wrapped {
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
        prefix.push(
            naga::Statement::Emit(naga::Range::new_from_bounds(member, access)),
            Span::UNDEFINED,
        );
        replacements.insert(global, access);
    }
    let mut map = Vec::new();
    for (_, expr) in old.iter() {
        if let E::GlobalVariable(global) = expr
            && let Some(&replacement) = replacements.get(global)
        {
            map.push(replacement);
            continue;
        }
        let mut expr = expr.clone();
        map_expression(&mut expr, &map);
        map.push(function.expressions.append(expr, Span::UNDEFINED));
    }
    for (_, local) in function.local_variables.iter_mut() {
        if let Some(init) = &mut local.init {
            *init = map[init.index()];
        }
    }
    function.named_expressions.clear();
    map_block(&mut function.body, &map);
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
fn map_block(block: &mut naga::Block, map: &[H]) {
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
                    *range = naga::Range::new_from_bounds(map[a.index()], map[b.index()]);
                }
            }
            S::Block(b) => map_block(b, map),
            S::If {
                condition,
                accept,
                reject,
            } => {
                m(condition);
                map_block(accept, map);
                map_block(reject, map);
            }
            S::Switch { selector, cases } => {
                m(selector);
                for c in cases {
                    map_block(&mut c.body, map);
                }
            }
            S::Loop {
                body,
                continuing,
                break_if,
            } => {
                map_block(body, map);
                map_block(continuing, map);
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
