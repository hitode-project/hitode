//! Types of the generated model tables, shared with the build script, which
//! precompiles every listed shader with the bindings derived here.
pub use adv_gpu::Format;
use adv_gpu::pipeline::{Binding, BindingKind, Stages};
use adv_gpu::resource::Dimension;

/// What a pass binds at a slot. Slots keep the generated binding numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Canvas dimensions and the region origin.
    Uniform,
    /// The composed canvas.
    Source,
    /// A tensor the pass reads.
    Tensor(usize),
    /// A tensor the pass writes.
    Store(usize),
    /// The presentable buffer the final layer writes.
    Output,
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug)]
pub struct Pass {
    /// Path below `stages/upscale`, the key of its precompiled forms.
    pub shader: &'static str,
    pub bindings: &'static [(u32, Role)],
    /// Output pixels per workgroup along each axis.
    pub block: u32,
    /// 2 for the final layer, whose output is the doubled image.
    pub scale: u32,
    /// Input pixels a change reaches through this pass.
    pub radius: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// Intermediate tensors, at canvas size, shared after their last reader.
    pub tensors: &'static [Format],
    pub passes: &'static [Pass],
}

/// The format the final layer declares for the presentable buffer it writes.
pub const OUTPUT: Format = Format::Rgba8Unorm;

impl Pass {
    pub fn layout(&self, model: &Model) -> Vec<Binding> {
        let sampled = BindingKind::SampledImage {
            dimension: Dimension::D2,
            depth: false,
            multisampled: false,
        };
        self.bindings
            .iter()
            .map(|&(slot, role)| Binding {
                slot,
                kind: match role {
                    Role::Uniform => BindingKind::UniformBuffer,
                    Role::Source | Role::Tensor(_) => sampled,
                    Role::Store(i) => BindingKind::StorageImage {
                        format: model.tensors[i],
                        writable: true,
                    },
                    Role::Output => BindingKind::StorageImage {
                        format: OUTPUT,
                        writable: true,
                    },
                    Role::Nearest | Role::Linear => BindingKind::Sampler { comparison: false },
                },
                stages: Stages::COMPUTE,
            })
            .collect()
    }
}
