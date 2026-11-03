//! Backend-free host compilation and the Metal lowering shared by runtime shaders.
use crate::pipeline::{Binding, BindingKind, Stages};
use crate::{Error, Result};
use naga::back::msl;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Portable host shader outputs. No device or driver is involved.
pub struct Precompiled {
    pub spirv: Vec<u32>,
    pub metal: String,
    pub identity: u64,
}

/// Compile a host shader using the GPU binding convention. Host stages admit
/// their buffer ranges before encoding, so their generated accesses are unchecked.
pub fn precompile(wgsl: &str, bindings: &[Binding]) -> Result<Precompiled> {
    let module = naga::front::wgsl::parse_str(wgsl)
        .map_err(|e| compile_error("WGSL", e.emit_to_string(wgsl)))?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| compile_error("shader validation", e))?;
    let spirv = naga::back::spv::write_vec(&module, &info, &Default::default(), None)
        .map_err(|e| compile_error("SPIR-V", e))?;
    let metal = metal(
        &module,
        &info,
        Some(bindings),
        Options {
            version: (2, 1),
            textures: 128,
            bounds: naga::proc::BoundsCheckPolicies {
                index: naga::proc::BoundsCheckPolicy::Unchecked,
                buffer: naga::proc::BoundsCheckPolicy::Unchecked,
                ..Default::default()
            },
        },
    )?
    .source;
    let mut identity = DefaultHasher::new();
    spirv.hash(&mut identity);
    metal.hash(&mut identity);
    Ok(Precompiled {
        spirv,
        metal,
        identity: identity.finish(),
    })
}

pub(crate) struct Options {
    pub version: (u8, u8),
    pub textures: u32,
    pub bounds: naga::proc::BoundsCheckPolicies,
}
pub(crate) struct Metal {
    pub source: String,
    #[cfg(all(feature = "backend", target_vendor = "apple"))]
    pub entries: Vec<MetalEntry>,
}
#[cfg(all(feature = "backend", target_vendor = "apple"))]
pub(crate) struct MetalEntry {
    pub name: String,
    pub used: Used,
    pub sizes: Option<Box<[Option<u8>]>>,
}

/// The byte-size table occupies the last Metal buffer argument.
pub(crate) const SIZES: usize = 30;

/// The one mapping from portable slots to Metal argument tables and indices.
/// Encoding calls this for admitted slots; shader creation checks the limits.
pub(crate) fn index(group: u32, slot: u32, buffer: bool) -> (usize, u32) {
    match (group, buffer) {
        (0, true) => (0, slot),
        (3, true) => (0, (SIZES as u32 - 1).wrapping_sub(slot)),
        (1, false) => (1, slot),
        (3, false) => (1, 95u32.wrapping_sub(slot)),
        (2, false) => (2, slot),
        _ => panic!("invalid Metal binding group"),
    }
}
pub(crate) fn native(group: u32, slot: u32, buffer: bool, textures: u32) -> Result<(usize, u32)> {
    let limit = match (group, buffer) {
        (0 | 3, true) => SIZES as u32,
        (1, false) => textures,
        (3, false) => 96,
        (2, false) => 16,
        _ => return Err(Error::Invalid("Metal binding group".into())),
    };
    let (table, index) = index(group, slot, buffer);
    if index >= limit {
        return Err(Error::Invalid(format!(
            "Metal binding {group}:{slot} exceeds its table"
        )));
    }
    Ok((table, index))
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Used {
    pub buffers: u32,
    pub textures: u128,
    pub samplers: u16,
}
impl Used {
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
    #[cfg(all(feature = "backend", target_vendor = "apple"))]
    pub fn contains(self, other: Used) -> bool {
        other.buffers & !self.buffers == 0
            && other.textures & !self.textures == 0
            && other.samplers & !self.samplers == 0
    }
}
pub(crate) fn binding_group(kind: BindingKind) -> (u32, bool) {
    match kind {
        BindingKind::UniformBuffer => (0, true),
        BindingKind::SampledImage { .. } => (1, false),
        BindingKind::Sampler { .. } => (2, false),
        BindingKind::StorageImage { .. } => (3, false),
        BindingKind::StorageBuffer { .. } => (3, true),
    }
}
pub(crate) fn compile_error(stage: impl std::fmt::Display, error: impl std::fmt::Display) -> Error {
    Error::Compile(format!("{stage}: {error}"))
}

pub(crate) fn metal(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    bindings: Option<&[Binding]>,
    options: Options,
) -> Result<Metal> {
    let mut msl_options = msl::Options {
        lang_version: options.version,
        fake_missing_bindings: false,
        bounds_check_policies: options.bounds,
        ..Default::default()
    };
    #[cfg(all(feature = "backend", target_vendor = "apple"))]
    let mut entries = Vec::new();
    for (i, ep) in module.entry_points.iter().enumerate() {
        let stage = match ep.stage {
            naga::ShaderStage::Vertex => Stages::VERTEX,
            naga::ShaderStage::Fragment => Stages::FRAGMENT,
            naga::ShaderStage::Compute => Stages::COMPUTE,
            _ => return Err(compile_error(&ep.name, "unsupported stage")),
        };
        let entry_info = info.get_entry_point(i);
        let mut resources = msl::EntryPointResources::default();
        let mut used = Used::default();
        let mut sizes = Vec::new();
        for (handle, var) in module.global_variables.iter() {
            if module.types[var.ty]
                .inner
                .needs_host_buffer_byte_size(&module.types)
            {
                sizes.push(if entry_info[handle].is_empty() {
                    None
                } else {
                    Some(
                        var.binding
                            .as_ref()
                            .ok_or_else(|| compile_error(&ep.name, "unbound runtime array"))?
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
                        class: naga::ImageClass::Sampled { .. } | naga::ImageClass::Depth { .. },
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
            if let Some(bindings) = bindings
                && !bindings.iter().any(|b| {
                    b.slot == binding.binding
                        && b.stages.contains(stage)
                        && binding_group(b.kind) == (binding.group, buffer)
                })
            {
                return Err(compile_error(
                    &ep.name,
                    format!("undeclared binding {}:{}", binding.group, binding.binding),
                ));
            }
            let index = used.add(binding.group, binding.binding, buffer, options.textures)? as u8;
            let mut target = msl::BindTarget::default();
            match (buffer, binding.group) {
                (true, _) => target.buffer = Some(index),
                (false, 2) => target.sampler = Some(msl::BindSamplerTarget::Resource(index)),
                _ => target.texture = Some(index),
            }
            target.mutable = matches!(var.space, naga::AddressSpace::Storage { access } if access.contains(naga::StorageAccess::STORE));
            resources.resources.insert(binding, target);
        }
        if !sizes.is_empty() {
            resources.sizes_buffer = Some(SIZES as u8);
        }
        let reads_sizes = sizes.iter().any(Option::is_some);
        if reads_sizes && sizes.len() > SIZES {
            return Err(compile_error(&ep.name, "too many runtime arrays"));
        }
        #[cfg(all(feature = "backend", target_vendor = "apple"))]
        entries.push(MetalEntry {
            name: String::new(),
            used,
            sizes: reads_sizes.then(|| {
                sizes
                    .iter()
                    .map(|slot| slot.map(|slot| index(3, slot, true).1 as u8))
                    .collect()
            }),
        });
        msl_options
            .per_entry_point_map
            .insert(ep.name.clone(), resources);
    }
    let pipeline = msl::PipelineOptions {
        allow_and_force_point_size: true,
        ..Default::default()
    };
    let (source, translation) = msl::write_string(module, info, &msl_options, &pipeline)
        .map_err(|e| compile_error("MSL", e))?;
    for (i, name) in translation.entry_point_names.into_iter().enumerate() {
        let name = name.map_err(|e| compile_error(&module.entry_points[i].name, e))?;
        #[cfg(all(feature = "backend", target_vendor = "apple"))]
        {
            entries[i].name = name;
        }
        #[cfg(not(all(feature = "backend", target_vendor = "apple")))]
        let _ = name;
    }
    Ok(Metal {
        source,
        #[cfg(all(feature = "backend", target_vendor = "apple"))]
        entries,
    })
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
    #[test]
    fn host_compilation_checks_bindings_and_hashes_its_outputs() {
        let source = "@group(3) @binding(0) var<storage,read> data:array<u32>; @compute @workgroup_size(1) fn main() { let x = data[0]; }";
        let binding = Binding {
            slot: 0,
            kind: BindingKind::StorageBuffer { writable: false },
            stages: Stages::COMPUTE,
        };
        assert!(precompile(source, &[]).is_err());
        let a = precompile(source, &[binding]).unwrap();
        let b = precompile(source, &[binding]).unwrap();
        assert_eq!(a.identity, b.identity);
        assert_eq!(a.spirv[0], 0x07230203);
        assert!(a.metal.contains("kernel"));
        assert!(!a.metal.contains("_buffer_sizes."));
        assert_ne!(
            a.identity,
            precompile(&source.replace("data[0]", "data[1]"), &[binding])
                .unwrap()
                .identity
        );
    }
}
