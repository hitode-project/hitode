//! Shader admission against a device's feature level and executing stages.
use super::Error;
use crate::{
    d3d11::caps::{FeatureLevel, Report, Requirement},
    shader::{
        dxbc,
        ir::{BindingKind, Model, Module, Register, Stage, Statement},
        wgsl,
    },
};

pub struct Shader {
    module: Module,
    translation: wgsl::Translation,
}

impl Shader {
    pub fn new(
        stage: Stage,
        bytecode: &[u8],
        has_linkage: bool,
        report: &Report,
    ) -> Result<Self, Error> {
        if has_linkage || matches!(stage, Stage::Geometry | Stage::Hull | Stage::Domain) {
            return Err(Error::InvalidArgument);
        }
        let module = dxbc::parse(bytecode).map_err(|_| Error::InvalidArgument)?;
        if module.stage != stage || !accepts(stage, module.model, report) {
            return Err(Error::InvalidArgument);
        }
        let translation =
            wgsl::emit(&module, wgsl::Options::default()).map_err(|_| Error::InvalidArgument)?;
        if stage == Stage::Compute {
            let level = report.level;
            let threads = translation
                .reflection
                .workgroup_size
                .ok_or(Error::InvalidArgument)?;
            let maximum = u64::from(level.limit(Requirement::ComputeThreads));
            let shared = u64::from(level.limit(Requirement::ComputeSharedBytes));
            if threads
                .map(u64::from)
                .into_iter()
                .try_fold(1u64, u64::checked_mul)
                .is_none_or(|size| size == 0 || size > maximum)
                || shared_bytes(&module) > shared
                || (level < FeatureLevel::L11_0 && threads[2] != 1)
                || translation.reflection.bindings.iter().any(|binding| {
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
            translation,
        })
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn translation(&self) -> &wgsl::Translation {
        &self.translation
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
        Stage::Geometry | Stage::Hull | Stage::Domain => false,
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
