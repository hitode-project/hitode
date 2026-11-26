//! Shader admission against a device's feature level and executing stages.
use super::Error;
use crate::{
    d3d11::caps::{FeatureLevel, Report, Requirement},
    shader::{
        analysis::Reflection,
        dxbc,
        ir::{BindingKind, Model, Module, Register, Stage, Statement},
        wgsl,
    },
};
use std::sync::Arc;

#[derive(Clone)]
pub struct Shader {
    module: Module,
    reflection: Reflection,
    pub stage: Stage,
    pub linkage: Option<Arc<dxbc::linkage::Linkage>>,
    pub stream_output: Option<StreamOutput>,
}

#[derive(Clone)]
pub struct StreamOutput {
    pub rasterized: Option<u32>,
    pub targets: [Option<StreamTarget>; 4],
}

#[derive(Clone)]
pub struct StreamTarget {
    pub stream: u32,
    pub elements: Vec<Option<(u32, u8)>>,
}

impl Shader {
    pub fn new(
        stage: Stage,
        bytecode: &[u8],
        has_linkage: bool,
        report: &Report,
    ) -> Result<Self, Error> {
        let module = dxbc::parse(bytecode).map_err(|_| Error::InvalidArgument)?;
        if module.stage != stage || !accepts(stage, module.model, report) {
            return Err(Error::InvalidArgument);
        }
        let reflection = crate::shader::analysis::analyze(&module);
        let linkage = if has_linkage {
            dxbc::linkage::Linkage::decode(bytecode)
                .map_err(|_| Error::InvalidArgument)?
                .map(Arc::new)
        } else {
            None
        };
        if module.body.iter().any(|stmt| {
            matches!(stmt,
            Statement::Declare(decl) if decl.opcode == crate::shader::ir::Opcode::DclInterface)
        }) && linkage.is_none()
        {
            return Err(Error::InvalidArgument);
        }
        match if matches!(stage, Stage::Geometry | Stage::Hull | Stage::Domain) || linkage.is_some()
        {
            Ok(())
        } else {
            wgsl::emit(&module, wgsl::Options::default()).map(|_| ())
        } {
            Ok(_)
            | Err(
                crate::shader::ir::Error::MissingStorageFormat(_)
                | crate::shader::ir::Error::MissingTypedBufferFormat(_)
                | crate::shader::ir::Error::MissingTypedBufferElements(..)
                | crate::shader::ir::Error::MissingStorageExtent(_),
            ) => (),
            Err(_) => return Err(Error::InvalidArgument),
        }
        if stage != Stage::Compute
            && reflection.bindings.iter().any(|binding| {
                binding.register == Register::UnorderedAccessView
                    && (stage != Stage::Pixel
                        || binding.slot >= report.level.limit(Requirement::OutputMergerUavSlots))
            })
        {
            return Err(Error::InvalidArgument);
        }
        if stage == Stage::Compute {
            let level = report.level;
            let threads = reflection.workgroup_size.ok_or(Error::InvalidArgument)?;
            let maximum = u64::from(level.limit(Requirement::ComputeThreads));
            let shared = u64::from(level.limit(Requirement::ComputeSharedBytes));
            if threads
                .map(u64::from)
                .into_iter()
                .try_fold(1u64, u64::checked_mul)
                .is_none_or(|size| size == 0 || size > maximum)
                || shared_bytes(&module) > shared
                || (level < FeatureLevel::L11_0 && threads[2] != 1)
                || reflection.bindings.iter().any(|binding| {
                    binding.register == Register::UnorderedAccessView
                        && (binding.slot >= level.limit(Requirement::ComputeUavSlots)
                            || (level < FeatureLevel::L11_0
                                && matches!(binding.kind, BindingKind::Typed { .. })))
                })
            {
                return Err(Error::InvalidArgument);
            }
        }
        Ok(Self {
            module,
            reflection,
            stage,
            linkage,
            stream_output: None,
        })
    }

    pub fn with_stream_output(
        bytecode: &[u8],
        report: &Report,
        entries: &[(Option<Vec<u8>>, adv_abi::types::D3D11_SO_DECLARATION_ENTRY)],
        strides: &[u32],
        rasterized: u32,
    ) -> Result<Self, Error> {
        let module = dxbc::parse(bytecode).map_err(|_| Error::InvalidArgument)?;
        if report.level < FeatureLevel::L10_0
            || !matches!(
                module.stage,
                Stage::Geometry | Stage::Vertex | Stage::Domain
            )
            || !accepts(module.stage, module.model, report)
            || entries.len() > 512
            || strides.len() > 4
        {
            return Err(Error::InvalidArgument);
        }
        let streams = report.level.limit(Requirement::StreamOutputStreams);
        let rasterized = match rasterized {
            u32::MAX => None,
            stream if stream < streams => Some(stream),
            _ => return Err(Error::InvalidArgument),
        };
        let mut targets: [Option<StreamTarget>; 4] = Default::default();
        let mut components = [0; 4];
        for (name, entry) in entries {
            let start = entry.StartComponent;
            let end = start
                .checked_add(entry.ComponentCount)
                .ok_or(Error::InvalidArgument)?;
            if entry.Stream >= streams
                || (module.stage != Stage::Geometry && entry.Stream != 0)
                || entry.ComponentCount == 0
                || end > 4
            {
                return Err(Error::InvalidArgument);
            }
            let target = targets
                .get_mut(usize::from(entry.OutputSlot))
                .ok_or(Error::InvalidArgument)?
                .get_or_insert_with(|| StreamTarget {
                    stream: entry.Stream,
                    elements: Vec::new(),
                });
            components[entry.Stream as usize] += u32::from(entry.ComponentCount);
            if target.stream != entry.Stream || components[entry.Stream as usize] > 128 {
                return Err(Error::InvalidArgument);
            }
            if let Some(name) = name {
                let element = module
                    .signatures
                    .output
                    .iter()
                    .find(|element| {
                        element.stream == entry.Stream
                            && element.semantic_index == entry.SemanticIndex
                            && element.semantic.as_bytes().eq_ignore_ascii_case(name)
                    })
                    .ok_or(Error::InvalidArgument)?;
                for lane in start..end {
                    if element.mask & (1 << lane) == 0 {
                        return Err(Error::InvalidArgument);
                    }
                    if target.elements.contains(&Some((element.register, lane))) {
                        return Err(Error::InvalidArgument);
                    }
                    target.elements.push(Some((element.register, lane)));
                }
            } else {
                target.elements.extend((start..end).map(|_| None));
            }
        }
        for (slot, target) in targets.iter_mut().enumerate() {
            if let Some(target) = target {
                let minimum = target.elements.len() as u32 * 4;
                let stride = strides.get(slot).copied().unwrap_or(minimum);
                if stride < minimum || stride % 4 != 0 || stride > 2048 {
                    return Err(Error::InvalidArgument);
                }
                target.elements.resize((stride / 4) as usize, None);
            }
        }
        let reflection = crate::shader::analysis::analyze(&module);
        Ok(Self {
            module,
            reflection,
            stage: Stage::Geometry,
            linkage: None,
            stream_output: Some(StreamOutput {
                rasterized,
                targets,
            }),
        })
    }

    pub fn specialize(&self, classes: &[&[u8]]) -> Result<Self, Error> {
        let tables = self
            .linkage
            .as_ref()
            .and_then(|linkage| linkage.tables(classes))
            .ok_or(Error::InvalidArgument)?;
        let module = crate::shader::ir::passes::specialize_interfaces(&self.module, &tables)
            .map_err(|_| Error::InvalidArgument)?;
        let reflection = crate::shader::analysis::analyze(&module);
        Ok(Self {
            module,
            reflection,
            ..self.clone()
        })
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn reflection(&self) -> &Reflection {
        &self.reflection
    }
}

fn accepts(stage: Stage, model: Model, report: &Report) -> bool {
    use FeatureLevel::*;
    match stage {
        Stage::Vertex | Stage::Pixel => match model {
            Model::Level91 | Model::Level93 => true,
            Model::Sm40 => report.level >= L10_0,
            Model::Sm41 => report.level >= L10_1,
            Model::Sm50 => report.level >= L11_0,
        },
        Stage::Compute => match model {
            Model::Level91 | Model::Level93 => false,
            Model::Sm40 => report.level >= L11_0 || (report.level >= L10_0 && report.compute_4x),
            Model::Sm41 => report.level >= L11_0 || (report.level >= L10_1 && report.compute_4x),
            Model::Sm50 => report.level >= L11_0,
        },
        Stage::Geometry => match model {
            Model::Sm40 => report.level >= L10_0,
            Model::Sm41 => report.level >= L10_1,
            Model::Sm50 => report.level >= L11_0,
            Model::Level91 | Model::Level93 => false,
        },
        Stage::Hull | Stage::Domain => model == Model::Sm50 && report.level >= L11_0,
    }
}

fn shared_bytes(module: &Module) -> u64 {
    module
        .body
        .iter()
        .filter_map(|statement| {
            let Statement::Declare(declaration) = statement else {
                return None;
            };
            declaration
                .binding
                .filter(|binding| binding.register == Register::ThreadGroupSharedMemory)
        })
        .map(|binding| match binding.kind {
            BindingKind::Raw { bytes } => u64::from(bytes),
            BindingKind::Structured { stride, elements } => u64::from(stride) * u64::from(elements),
            BindingKind::ConstantBuffer { .. }
            | BindingKind::Sampler(_)
            | BindingKind::Typed { .. } => 0,
        })
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::d3d11::caps::tests::fixture_report;

    macro_rules! shader {
        ($profile:literal) => {
            include_bytes!(concat!(
                "../../../tests/fixtures/shader-core/",
                $profile,
                ".dxbc"
            ))
        };
    }

    // spec: GFX-CAP-1 GFX-CAP-4
    #[test]
    fn windows_profiles_admit_through_the_parser_and_emitter() {
        use FeatureLevel::*;
        let shaders: &[(Stage, &[u8], FeatureLevel)] = &[
            (Stage::Vertex, shader!("vs_4_0_level_9_3"), L9_1),
            (Stage::Pixel, shader!("ps_4_0_level_9_1"), L9_1),
            (Stage::Pixel, shader!("ps_4_0_level_9_3"), L9_1),
            (Stage::Vertex, shader!("vs_4_0"), L10_0),
            (Stage::Geometry, shader!("gs_4_0"), L10_0),
            (Stage::Geometry, shader!("gs_4_1"), L10_1),
            (Stage::Geometry, shader!("gs_5_0"), L11_0),
            (Stage::Hull, shader!("hs_5_0"), L11_0),
            (Stage::Domain, shader!("ds_5_0"), L11_0),
            (Stage::Pixel, shader!("ps_4_1"), L10_1),
            (Stage::Compute, shader!("cs_4_0"), L10_0),
            (Stage::Compute, shader!("cs_4_1"), L10_1),
            (Stage::Vertex, shader!("vs_5_0"), L11_0),
            (Stage::Pixel, shader!("ps_5_0"), L11_0),
            (Stage::Compute, shader!("cs_5_0"), L11_0),
        ];
        for level in FeatureLevel::ALL {
            let report = fixture_report(level);
            for &(stage, bytes, minimum) in shaders {
                assert_eq!(
                    Shader::new(stage, bytes, false, &report).is_ok(),
                    level >= minimum,
                    "{stage:?} at {level:?}, minimum {minimum:?}"
                );
            }
        }
    }
}
