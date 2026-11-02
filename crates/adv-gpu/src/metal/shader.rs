use super::*;
use naga::back::msl;

#[derive(Clone)]
pub(crate) struct Shader(Arc<ShaderData>);
struct ShaderData {
    identity: u64,
    entries: Vec<Function>,
}
pub(super) struct Function {
    pub name: String,
    pub stage: Stage,
    pub raw: Object<dyn MTLFunction>,
    pub workgroup: [u32; 3],
    /// Native indices the entry reads; `None` for precompiled libraries, whose
    /// pipelines use their declared bindings.
    pub used: Option<Used>,
    /// Storage buffer index of each entry in naga's byte-size table at
    /// `buffer(SIZES)`; `None` for runtime arrays the entry does not use.
    pub sizes: Option<Box<[Option<u8>]>>,
}

/// Native indices of one stage, one bit per index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Used {
    pub buffers: u32,
    pub textures: u128,
    pub samplers: u16,
}

/// Native indices are fixed per slot, like D3D registers, so a binding
/// survives pipeline changes untouched: uniform slot `s` is `buffer(s)`,
/// storage buffer `s` is `buffer(29 - s)`, sampled image `s` is `texture(s)`,
/// storage image `s` is `texture(95 - s)` and sampler `s` is `sampler(s)`.
/// `buffer(30)` holds naga's runtime-array byte sizes. Precompiled libraries
/// declare the same indices. A stage whose two kinds of one table meet is
/// rejected at creation.
pub(super) const SIZES: usize = 30;
pub(super) fn native(group: u32, slot: u32, buffer: bool, textures: u32) -> Result<(usize, u32)> {
    let (table, index, limit) = match (group, buffer) {
        (0, true) => (0, slot, SIZES as u32),
        (3, true) => (0, (SIZES as u32 - 1).wrapping_sub(slot), SIZES as u32),
        (1, false) => (1, slot, textures),
        (3, false) => (1, 95u32.wrapping_sub(slot), 96),
        (2, false) => (2, slot, 16),
        _ => return Err(Error::Invalid("Metal binding group".into())),
    };
    if index >= limit {
        return Err(Error::Invalid(format!(
            "Metal binding {group}:{slot} exceeds its table"
        )));
    }
    Ok((table, index))
}

impl Used {
    /// Adds one binding, rejecting an index another binding already holds.
    pub fn add(&mut self, group: u32, slot: u32, buffer: bool, textures: u32) -> Result<u32> {
        let (table, index) = native(group, slot, buffer, textures)?;
        let fresh = match table {
            0 => {
                let fresh = self.buffers & 1 << index == 0;
                self.buffers |= 1 << index;
                fresh
            }
            1 => {
                let fresh = self.textures & 1 << index == 0;
                self.textures |= 1 << index;
                fresh
            }
            _ => {
                let fresh = self.samplers & 1 << index == 0;
                self.samplers |= 1 << index;
                fresh
            }
        };
        if !fresh {
            return Err(Error::Invalid(format!(
                "Metal binding {group}:{slot} shares index {index} with another kind"
            )));
        }
        Ok(index)
    }
    pub fn contains(self, other: Used) -> bool {
        other.buffers & !self.buffers == 0
            && other.textures & !self.textures == 0
            && other.samplers & !self.samplers == 0
    }
}

pub(super) fn binding_group(kind: BindingKind) -> (u32, bool) {
    match kind {
        BindingKind::UniformBuffer => (0, true),
        BindingKind::SampledImage { .. } => (1, false),
        BindingKind::Sampler { .. } => (2, false),
        BindingKind::StorageImage { .. } => (3, false),
        BindingKind::StorageBuffer { .. } => (3, true),
    }
}

pub(super) fn compile_error(stage: impl std::fmt::Display, error: impl std::fmt::Display) -> Error {
    Error::Compile(format!("{stage}: {error}"))
}

fn version() -> (u8, u8) {
    if objc2::available!(macos = 26.0, ios = 26.0) {
        (4, 0)
    } else if objc2::available!(macos = 15.0, ios = 18.0) {
        (3, 2)
    } else if objc2::available!(macos = 14.0, ios = 17.0) {
        (3, 1)
    } else if objc2::available!(macos = 13.0, ios = 16.0) {
        (3, 0)
    } else if objc2::available!(macos = 12.0, ios = 15.0) {
        (2, 4)
    } else if objc2::available!(macos = 11.0, ios = 14.0) {
        (2, 3)
    } else if objc2::available!(macos = 10.15, ios = 13.0) {
        (2, 2)
    } else {
        (2, 1)
    }
}

impl Device {
    pub(crate) fn create_shader(&self, desc: ShaderDesc<'_>) -> Result<crate::Shader> {
        self.check()?;
        // These synchronous calls stay on the caller (the compile pool).
        objc2::rc::autoreleasepool(|_| {
            let module = match desc.source {
                Source::Wgsl(source) => naga::front::wgsl::parse_str(source)
                    .map_err(|e| compile_error("WGSL", e.emit_to_string(source)))?,
                Source::Ir(module) => *module,
                Source::Precompiled { metal, .. } => {
                    let library = if metal.starts_with(b"MTLB") {
                        let file = pipeline::TemporaryFile::new()?;
                        std::fs::write(&file.0, metal).map_err(pipeline::io_error)?;
                        self.0.raw.newLibraryWithURL_error(&file.url())
                    } else {
                        let source =
                            std::str::from_utf8(metal).map_err(|e| compile_error("MSL", e))?;
                        self.library(source)
                    }
                    .map_err(|e| compile_error("Metal library", e))?;
                    let mut entries = Vec::new();
                    for name in library.functionNames() {
                        let raw = library
                            .newFunctionWithName(&name)
                            .ok_or_else(|| compile_error("Metal library", "missing function"))?;
                        let stage = match raw.functionType() {
                            MTLFunctionType::Vertex => Stage::Vertex,
                            MTLFunctionType::Fragment => Stage::Fragment,
                            MTLFunctionType::Kernel => Stage::Compute,
                            _ => continue,
                        };
                        entries.push(Function {
                            name: name.to_string(),
                            stage,
                            raw,
                            // Metal libraries expose no workgroup dimensions.
                            // Precompiled kernels use one thread per group.
                            workgroup: [1; 3],
                            used: None,
                            sizes: None,
                        });
                    }
                    entries.sort_by(|a, b| a.name.cmp(&b.name));
                    return Ok(crate::Shader {
                        identity: desc.identity,
                        inner: Shader(Arc::new(ShaderData {
                            identity: desc.identity,
                            entries,
                        })),
                    });
                }
            };
            let mut capabilities = naga::valid::Capabilities::default()
                | naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32
                | naga::valid::Capabilities::EARLY_DEPTH_TEST;
            if self.caps().features.contains(Features::SHADER_F16) {
                capabilities |= naga::valid::Capabilities::SHADER_FLOAT16;
            }
            if self.caps().features.contains(Features::SUBGROUP) {
                capabilities |= naga::valid::Capabilities::SUBGROUP
                    | naga::valid::Capabilities::SUBGROUP_BARRIER;
            }
            if self.caps().features.contains(Features::MATRIX_F16_8X8) {
                capabilities |= naga::valid::Capabilities::COOPERATIVE_MATRIX;
            }
            let info =
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
                    .validate(&module)
                    .map_err(|e| compile_error("shader validation", e))?;
            let mut entries = Vec::new();
            for ep in &module.entry_points {
                let stage = match ep.stage {
                    naga::ShaderStage::Vertex => Stage::Vertex,
                    naga::ShaderStage::Fragment => Stage::Fragment,
                    naga::ShaderStage::Compute => Stage::Compute,
                    _ => return Err(compile_error(&ep.name, "unsupported stage")),
                };
                let (module, info) = naga::back::pipeline_constants::process_overrides(
                    &module,
                    &info,
                    Some((ep.stage, &ep.name)),
                    &Default::default(),
                )
                .map_err(|e| compile_error(&ep.name, e))?;
                let textures = self.caps().limits.max_sampled_images;
                let mut used = Used::default();
                let mut resources = msl::EntryPointResources::default();
                let mut sizes = Vec::new();
                let entry_info = info.get_entry_point(0);
                for (handle, var) in module.global_variables.iter() {
                    if module.types[var.ty]
                        .inner
                        .needs_host_buffer_byte_size(&module.types)
                    {
                        // Naga includes unused runtime arrays in the size struct.
                        sizes.push(if entry_info[handle].is_empty() {
                            None
                        } else {
                            Some(
                                var.binding
                                    .as_ref()
                                    .ok_or_else(|| {
                                        compile_error(&ep.name, "unbound runtime array")
                                    })?
                                    .binding,
                            )
                        });
                    }
                    if entry_info[handle].is_empty() {
                        continue;
                    }
                    let Some(binding) = var.binding else {
                        continue;
                    };
                    let buffer = match (binding.group, var.space, &module.types[var.ty].inner) {
                        (0, naga::AddressSpace::Uniform, _)
                        | (3, naga::AddressSpace::Storage { .. }, _) => true,
                        (
                            1,
                            _,
                            naga::TypeInner::Image {
                                class:
                                    naga::ImageClass::Sampled { .. } | naga::ImageClass::Depth { .. },
                                ..
                            },
                        )
                        | (
                            3,
                            _,
                            naga::TypeInner::Image {
                                class: naga::ImageClass::Storage { .. },
                                ..
                            },
                        )
                        | (2, _, naga::TypeInner::Sampler { .. }) => false,
                        _ => {
                            return Err(compile_error(
                                &ep.name,
                                "resource does not follow the binding convention",
                            ));
                        }
                    };
                    let index = used.add(binding.group, binding.binding, buffer, textures)? as u8;
                    let mut target = msl::BindTarget::default();
                    match (buffer, binding.group) {
                        (true, _) => target.buffer = Some(index),
                        (false, 2) => {
                            target.sampler = Some(msl::BindSamplerTarget::Resource(index))
                        }
                        _ => target.texture = Some(index),
                    }
                    target.mutable = matches!(var.space, naga::AddressSpace::Storage { access }
                        if access.contains(naga::StorageAccess::STORE));
                    resources.resources.insert(binding, target);
                }
                let sizes = if sizes.iter().any(Option::is_some) {
                    if sizes.len() > SIZES {
                        return Err(compile_error(&ep.name, "too many runtime arrays"));
                    }
                    resources.sizes_buffer = Some(SIZES as u8);
                    Some(
                        sizes
                            .iter()
                            .map(|slot| slot.map(|slot| (SIZES - 1) as u8 - slot as u8))
                            .collect(),
                    )
                } else {
                    None
                };
                let options = msl::Options {
                    lang_version: version(),
                    fake_missing_bindings: false,
                    per_entry_point_map: [(ep.name.clone(), resources)].into(),
                    bounds_check_policies: naga::proc::BoundsCheckPolicies {
                        index: naga::proc::BoundsCheckPolicy::Restrict,
                        buffer: naga::proc::BoundsCheckPolicy::Restrict,
                        image_load: naga::proc::BoundsCheckPolicy::Restrict,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let options_pipeline = msl::PipelineOptions {
                    entry_point: Some((ep.stage, ep.name.clone())),
                    allow_and_force_point_size: true,
                    ..Default::default()
                };
                let (source, translation) =
                    msl::write_string(&module, &info, &options, &options_pipeline)
                        .map_err(|e| compile_error(&ep.name, e))?;
                let name = translation.entry_point_names[0]
                    .as_ref()
                    .map_err(|e| compile_error(&ep.name, e))?;
                let library = self
                    .library(&source)
                    .map_err(|e| compile_error(&ep.name, e))?;
                library.setLabel(Some(&NSString::from_str(desc.label)));
                let raw = library
                    .newFunctionWithName(&NSString::from_str(name))
                    .ok_or_else(|| compile_error(&ep.name, "missing translated entry"))?;
                entries.push(Function {
                    name: ep.name.clone(),
                    stage,
                    raw,
                    workgroup: module.entry_points[0].workgroup_size,
                    used: Some(used),
                    sizes,
                });
            }
            Ok(crate::Shader {
                identity: desc.identity,
                inner: Shader(Arc::new(ShaderData {
                    identity: desc.identity,
                    entries,
                })),
            })
        })
    }
    fn library(
        &self,
        source: &str,
    ) -> std::result::Result<Object<dyn MTLLibrary>, Retained<objc2_foundation::NSError>> {
        let version = version();
        let compile = MTLCompileOptions::new();
        compile.setLanguageVersion(MTLLanguageVersion(
            ((version.0 as usize) << 16) | version.1 as usize,
        ));
        if objc2::available!(macos = 11.0, ios = 13.0) {
            compile.setPreserveInvariance(true);
        }
        self.0
            .raw
            .newLibraryWithSource_options_error(&NSString::from_str(source), Some(&compile))
    }
}
impl Shader {
    pub(crate) fn entry(&self, name: &str, stage: Stage) -> Result<Entry> {
        let index = self
            .0
            .entries
            .iter()
            .position(|f| f.name == name && f.stage == stage)
            .ok_or_else(|| {
                compile_error(format!("{stage:?}"), format!("entry {name} not found"))
            })?;
        Ok(Entry {
            shader: crate::Shader {
                identity: self.0.identity,
                inner: self.clone(),
            },
            index: index as u32,
            stage,
        })
    }
    pub(super) fn function(&self, entry: &Entry) -> Result<&Function> {
        self.0
            .entries
            .get(entry.index as usize)
            .filter(|f| f.stage == entry.stage)
            .ok_or_else(|| {
                compile_error(format!("{:?}", entry.stage), "invalid entry index or stage")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slots_hold_fixed_indices_and_kinds_meeting_in_a_table_are_rejected() {
        let mut used = Used::default();
        assert_eq!(used.add(0, 3, true, 96).unwrap(), 3);
        assert_eq!(used.add(3, 0, true, 96).unwrap(), 29);
        assert_eq!(used.add(1, 95, false, 96).unwrap(), 95);
        assert_eq!(used.add(3, 1, false, 96).unwrap(), 94);
        assert_eq!(used.add(2, 15, false, 96).unwrap(), 15);
        assert!(used.add(3, 0, false, 96).is_err());
        assert!(used.add(0, 29, true, 96).is_err());
        assert!(used.add(0, 30, true, 96).is_err());
        assert!(used.add(1, 96, false, 96).is_err());
        assert!(used.add(2, 16, false, 96).is_err());
    }
}
