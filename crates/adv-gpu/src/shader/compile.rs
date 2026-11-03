//! Backend-free host compilation and the binding lowering shared by runtime shaders.
use crate::pipeline::{Binding, BindingKind, Stages};
use crate::{Error, Result};
use naga::back::msl;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Portable host shader outputs. No device or driver is involved.
pub struct Precompiled {
    pub spirv: Vec<u32>,
    pub metal: String,
    pub identity: u64,
    /// Each entry point's WGSL name, Metal function name, workgroup size and
    /// threadgroup argument lengths ([`crate::shader::PrecompiledEntry`]).
    pub entries: Vec<(String, String, [u32; 3], Vec<u32>)>,
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
    let options = naga::back::spv::Options {
        use_storage_input_output_16: false,
        ..Default::default()
    };
    let mut spirv = naga::back::spv::write_vec(&module, &info, &options, None)
        .map_err(|e| compile_error("SPIR-V", e))?;
    legalize(&mut spirv);
    combine_video_sampler(&mut spirv, 4);
    // SIMD-group matrices need MSL 2.3; everything else stays on 2.1.
    let matrix = module
        .types
        .iter()
        .any(|(_, ty)| matches!(ty.inner, naga::TypeInner::CooperativeMatrix { .. }));
    let metal = metal(
        &module,
        &info,
        Some(bindings),
        Options {
            version: if matrix { (2, 3) } else { (2, 1) },
            textures: 128,
            bounds: naga::proc::BoundsCheckPolicies {
                index: naga::proc::BoundsCheckPolicy::Unchecked,
                buffer: naga::proc::BoundsCheckPolicy::Unchecked,
                ..Default::default()
            },
        },
    )?;
    let entries = module
        .entry_points
        .iter()
        .zip(metal.names)
        .zip(metal.threadgroup)
        .map(|((ep, name), threadgroup)| (ep.name.clone(), name, ep.workgroup_size, threadgroup))
        .collect();
    let metal = metal.source;
    let mut identity = DefaultHasher::new();
    spirv.hash(&mut identity);
    metal.hash(&mut identity);
    Ok(Precompiled {
        spirv,
        metal,
        identity: identity.finish(),
        entries,
    })
}

pub(crate) struct Options {
    pub version: (u8, u8),
    pub textures: u32,
    pub bounds: naga::proc::BoundsCheckPolicies,
}
pub(crate) struct Metal {
    pub source: String,
    /// MSL function names, in entry point order.
    pub names: Vec<String>,
    /// Bytes of each entry's threadgroup arguments, which naga declares
    /// without indices in global variable order; encoders set the lengths.
    pub threadgroup: Vec<Vec<u32>>,
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
        (2, false) => 32,
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
    pub samplers: u32,
}
pub(crate) fn sampler_index(slot: u32) -> u32 {
    slot % 16
}
impl Used {
    pub fn check_samplers(self) -> Result<()> {
        if self.samplers as u16 & (self.samplers >> 16) as u16 != 0 {
            return Err(Error::Invalid(
                "sampler slots collide modulo 16 in one stage".into(),
            ));
        }
        Ok(())
    }
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
        Ok(if table == 2 {
            sampler_index(index)
        } else {
            index
        })
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
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|e| compile_error("MSL", e))?;
    let mut threadgroup = Vec::new();
    for (i, ep) in module.entry_points.iter().enumerate() {
        let stage = match ep.stage {
            naga::ShaderStage::Vertex => Stages::VERTEX,
            naga::ShaderStage::Fragment => Stages::FRAGMENT,
            naga::ShaderStage::Compute => Stages::COMPUTE,
            _ => return Err(compile_error(&ep.name, "unsupported stage")),
        };
        let entry_info = info.get_entry_point(i);
        threadgroup.push(
            module
                .global_variables
                .iter()
                .filter(|&(handle, var)| {
                    var.space == naga::AddressSpace::WorkGroup && !entry_info[handle].is_empty()
                })
                .map(|(_, var)| layouter[var.ty].size.next_multiple_of(16))
                .collect(),
        );
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
            let portable = match (binding.group, binding.binding) {
                (4, 0) => naga::ResourceBinding {
                    group: 1,
                    binding: 0,
                },
                (4, 1) => naga::ResourceBinding {
                    group: 2,
                    binding: 0,
                },
                _ => binding,
            };
            let buffer = match (portable.group, var.space, &module.types[var.ty].inner) {
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
                && binding.group != 4
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
            let index = used.add(portable.group, portable.binding, buffer, options.textures)? as u8;
            let mut target = msl::BindTarget::default();
            match (buffer, portable.group) {
                (true, _) => target.buffer = Some(index),
                (false, 2) => target.sampler = Some(msl::BindSamplerTarget::Resource(index)),
                _ => target.texture = Some(index),
            }
            target.mutable = matches!(var.space, naga::AddressSpace::Storage { access } if access.contains(naga::StorageAccess::STORE));
            resources.resources.insert(binding, target);
        }
        used.check_samplers()?;
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
    let names = translation
        .entry_point_names
        .into_iter()
        .enumerate()
        .map(|(i, name)| name.map_err(|e| compile_error(&module.entry_points[i].name, e)))
        .collect::<Result<Vec<_>>>()?;
    #[cfg(all(feature = "backend", target_vendor = "apple"))]
    for (entry, name) in entries.iter_mut().zip(&names) {
        entry.name = name.clone();
    }
    Ok(Metal {
        source,
        names,
        threadgroup,
        #[cfg(all(feature = "backend", target_vendor = "apple"))]
        entries,
    })
}

/// Naga has no combined-image global in its IR. Restore the reserved video
/// pair only for import; pipeline lowering combines it again with the same
/// function used by host compilation.
#[cfg(any(test, all(feature = "backend", not(target_vendor = "apple"))))]
pub(crate) fn parse_spirv(words: &[u32]) -> Result<naga::Module> {
    if words.len() < 5 || words[0] != 0x07230203 {
        return Err(compile_error("SPIR-V", "invalid header"));
    }
    let mut at = 5;
    while at < words.len() {
        let count = (words[at] >> 16) as usize;
        if count == 0 || count > words.len() - at {
            return Err(compile_error("SPIR-V", "invalid instruction length"));
        }
        at += count;
    }
    let mut import = words.to_vec();
    let code: Vec<_> = instructions(words).collect();
    let image = code.iter().find_map(|i| {
        (i.len() == 4 && i[0] & 0xffff == 71 && i[2] == 34 && i[3] == 4).then(|| i[1])
    });
    if let Some(image) = image {
        let pointee = code
            .iter()
            .find_map(|i| (i.len() == 4 && i[0] & 0xffff == 59 && i[2] == image).then(|| i[1]));
        let sampled = code.iter().find_map(|i| {
            (i.len() == 4 && i[0] & 0xffff == 32 && Some(i[1]) == pointee).then(|| i[3])
        });
        let image_type = code.iter().find_map(|i| {
            (i.len() == 3 && i[0] & 0xffff == 27 && Some(i[1]) == sampled).then(|| i[2])
        });
        if let Some(image_type) = image_type {
            let sampled = sampled.unwrap();
            let missing = || compile_error("SPIR-V", "incomplete combined video declaration");
            let image_pointer = code
                .iter()
                .find(|i| i.len() == 4 && i[0] & 0xffff == 32 && i[2] == 0 && i[3] == image_type)
                .ok_or_else(missing)?[1];
            let sampler_type = code
                .iter()
                .find(|i| i.len() == 2 && i[0] & 0xffff == 26)
                .ok_or_else(missing)?[1];
            let sampler_pointer = code
                .iter()
                .find(|i| i.len() == 4 && i[0] & 0xffff == 32 && i[2] == 0 && i[3] == sampler_type)
                .ok_or_else(missing)?[1];
            let mut next = words[3];
            let sampler = next;
            next += 1;
            import.truncate(5);
            let mut loads = HashMap::new();
            for i in code {
                match i[0] & 0xffff {
                    71 if i.len() == 4 && i[1] == image && i[2] == 34 => {
                        import.extend_from_slice(i);
                        import.extend([(4 << 16) | 71, sampler, 34, 4]);
                        import.extend([(4 << 16) | 71, sampler, 33, 1]);
                    }
                    32 if i.len() == 4 && i[1] == sampler_pointer => {
                        import.extend_from_slice(i);
                        import.extend([(4 << 16) | 59, sampler_pointer, sampler, 0]);
                    }
                    59 if i.len() == 4 && i[2] == image => {
                        import.extend([(4 << 16) | 59, image_pointer, image, 0]);
                    }
                    61 if i.len() >= 4 && i[3] == image => {
                        let (y, s) = (next, next + 1);
                        next += 2;
                        import.extend([(4 << 16) | 61, image_type, y, image]);
                        import.extend([(4 << 16) | 61, sampler_type, s, sampler]);
                        import.extend([(5 << 16) | 86, sampled, i[2], y, s]);
                        loads.insert(i[2], (y, s));
                    }
                    83 if i.len() == 4 && i[1] == sampled && loads.contains_key(&i[3]) => {
                        let &(y, s) = &loads[&i[3]];
                        import.extend([(5 << 16) | 86, sampled, i[2], y, s]);
                        loads.insert(i[2], (y, s));
                    }
                    _ => import.extend_from_slice(i),
                }
            }
            import[3] = next;
        }
    }
    naga::front::spv::Frontend::new(import.into_iter(), &Default::default())
        .parse()
        .map_err(|e| compile_error("SPIR-V", e))
}

fn instructions(words: &[u32]) -> impl Iterator<Item = &[u32]> {
    let mut rest = &words[5..];
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (instruction, tail) = rest.split_at((rest[0] >> 16) as usize);
        rest = tail;
        Some(instruction)
    })
}

/// Naga's SPIR-V breaks two Vulkan rules our shaders never rely on:
/// - It declares 16-bit buffer access with every half precision type, but
///   `SHADER_F16` covers arithmetic only and phones lack the access features.
/// - It gives every array an `ArrayStride`, which is invalid on arrays that
///   only workgroup, private or function variables hold.
pub(crate) fn legalize(words: &mut Vec<u32>) {
    const STORAGE_BUFFER_16: u32 = 4433;
    const UNIFORM_AND_STORAGE_BUFFER_16: u32 = 4434;
    const ARRAY_STRIDE: u32 = 6;
    // Uniform, PushConstant, StorageBuffer, PhysicalStorageBuffer.
    const EXPLICIT: [u32; 4] = [2, 9, 12, 5349];
    let mut parts = std::collections::HashMap::new();
    let mut explicit = Vec::new();
    for i in instructions(words) {
        match i[0] & 0xffff {
            // OpTypeArray, OpTypeRuntimeArray: the element; OpTypeStruct: members.
            28 | 29 => {
                parts.insert(i[1], &i[2..3]);
            }
            30 => {
                parts.insert(i[1], &i[2..]);
            }
            // OpTypePointer
            32 if EXPLICIT.contains(&i[2]) => explicit.push(i[3]),
            _ => {}
        }
    }
    let mut laid = std::collections::HashSet::new();
    while let Some(id) = explicit.pop() {
        if laid.insert(id) {
            explicit.extend(parts.get(&id).into_iter().flat_map(|p| p.iter()));
        }
    }
    let mut kept = words[..5].to_vec();
    for i in instructions(words) {
        let drop = match i[0] & 0xffff {
            17 => matches!(i[1], STORAGE_BUFFER_16 | UNIFORM_AND_STORAGE_BUFFER_16),
            71 => i[2] == ARRAY_STRIDE && !laid.contains(&i[1]),
            _ => false,
        };
        if !drop {
            kept.extend_from_slice(i);
        }
    }
    *words = kept;
}

/// Naga emits separate WGSL textures and samplers. External-format sampling
/// requires an OpTypeSampledImage variable in one combined descriptor instead.
/// Only the reserved video pair is rewritten; the binding tiers are unchanged.
pub(crate) fn combine_video_sampler(words: &mut Vec<u32>, set: u32) {
    let mut sets = HashMap::new();
    let mut bindings = HashMap::new();
    for i in instructions(words) {
        if i[0] & 0xffff == 71 {
            // OpDecorate
            match i[2] {
                34 => {
                    sets.insert(i[1], i[3]);
                } // DescriptorSet
                33 => {
                    bindings.insert(i[1], i[3]);
                } // Binding
                _ => {}
            }
        }
    }
    let variable = |binding| {
        bindings
            .iter()
            .find_map(|(&id, &b)| (b == binding && sets.get(&id) == Some(&set)).then_some(id))
    };
    let Some(image) = variable(0) else {
        return;
    }; // An entry may not use video.
    let sampler = variable(1).expect("video sampler declaration");
    let pointer = instructions(words)
        .find(|i| i[0] & 0xffff == 59 && i[2] == image)
        .unwrap()[1];
    let image_type = instructions(words)
        .find(|i| i[0] & 0xffff == 32 && i[1] == pointer)
        .unwrap()[3];
    let sampled_type = instructions(words)
        .find(|i| i[0] & 0xffff == 27 && i[2] == image_type)
        .unwrap()[1];
    let combined_pointer = words[3];
    words[3] += 1;
    let mut output = words[..5].to_vec();
    let mut loaded_images = Vec::new();
    for i in instructions(words) {
        let op = i[0] & 0xffff;
        match op {
            5 | 71 if i[1] == sampler => {}  // OpName / OpDecorate
            59 if i[2] == sampler => {}      // OpVariable
            27 if i[1] == sampled_type => {} // Its type must precede the variable.
            59 if i[2] == image => {
                output.extend([(3 << 16) | 27, sampled_type, image_type]);
                output.extend([(4 << 16) | 32, combined_pointer, 0, sampled_type]); // OpTypePointer, UniformConstant
                output.extend([(4 << 16) | 59, combined_pointer, image, 0]);
            }
            61 if i[3] == sampler => {} // OpLoad
            61 if i[3] == image => {
                loaded_images.push(i[2]);
                output.extend([(4 << 16) | 61, sampled_type, i[2], image]);
            }
            86 if loaded_images.contains(&i[3]) => {
                // OpSampledImage -> OpCopyObject
                output.extend([(4 << 16) | 83, sampled_type, i[2], i[3]]);
            }
            _ => output.extend_from_slice(i),
        }
    }
    *words = output;
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
        assert_eq!(used.add(2, 16, false, 96).unwrap(), 0);
        assert!(used.add(2, 32, false, 96).is_err());
    }
    #[test]
    fn host_samplers_share_native_indices_only_across_stages() {
        let source = r#"
            @group(1) @binding(0) var image: texture_2d<f32>;
            @group(2) @binding(16) var vertex_sampler: sampler;
            @group(2) @binding(0) var fragment_sampler: sampler;
            @vertex fn vs() -> @builtin(position) vec4<f32> {
                return textureSampleLevel(image, vertex_sampler, vec2(0.5), 0.);
            }
            @fragment fn fs() -> @location(0) vec4<f32> {
                return textureSampleLevel(image, fragment_sampler, vec2(0.5), 0.);
            }
        "#;
        let bindings = [
            Binding {
                slot: 0,
                kind: BindingKind::SampledImage {
                    dimension: crate::resource::Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::VERTEX | Stages::FRAGMENT,
            },
            Binding {
                slot: 16,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::VERTEX,
            },
            Binding {
                slot: 0,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::FRAGMENT,
            },
        ];
        let compiled = precompile(source, &bindings).unwrap();
        assert_eq!(compiled.metal.matches("[[sampler(0)]]").count(), 2);
        let collision = source.replace("return textureSampleLevel(image, fragment_sampler, vec2(0.5), 0.);", "return textureSampleLevel(image, fragment_sampler, vec2(0.5), 0.) + textureSampleLevel(image, vertex_sampler, vec2(0.5), 0.);");
        let mut bindings = bindings;
        bindings[1].stages |= Stages::FRAGMENT;
        assert!(precompile(&collision, &bindings).is_err());
    }
    #[test]
    fn precompiled_video_is_combined() {
        let compiled = precompile(
            r#"
            @group(4) @binding(0) var video: texture_2d<f32>;
            @group(4) @binding(1) var filtering: sampler;
            @fragment fn fs() -> @location(0) vec4<f32> {
                return textureSampleLevel(video, filtering, vec2(0.5), 0.);
            }
        "#,
            &[],
        )
        .unwrap();
        assert_eq!(
            instructions(&compiled.spirv)
                .filter(|i| i[0] & 0xffff == 71 && i[2] == 34 && i[3] == 4)
                .count(),
            1
        );
        let module = parse_spirv(&compiled.spirv).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
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
        assert_eq!(a.entries.len(), 1);
        let (name, function, workgroup, threadgroup) = &a.entries[0];
        assert_eq!((name.as_str(), *workgroup), ("main", [1, 1, 1]));
        assert!(threadgroup.is_empty());
        assert!(a.metal.contains(&format!("kernel void {function}(")));
        assert_ne!(function, "main", "Metal reserves `main`");
        assert!(!a.metal.contains("_buffer_sizes."));
        assert_ne!(
            a.identity,
            precompile(&source.replace("data[0]", "data[1]"), &[binding])
                .unwrap()
                .identity
        );
    }
}
