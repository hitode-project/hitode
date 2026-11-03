//! Binding descriptions shared by stage pipelines and host-shader compilation.
use adv_gpu::pipeline::{Binding, BindingKind, Stages};
use adv_gpu::resource::Dimension;

pub const INGEST: [Binding; 2] = [
    Binding {
        slot: 0,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::FRAGMENT,
    },
    Binding {
        slot: 1,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::VERTEX.union(Stages::FRAGMENT),
    },
];
pub const VIDEO: [Binding; 5] = [
    Binding {
        slot: 0,
        kind: BindingKind::UniformBuffer,
        stages: Stages::VERTEX.union(Stages::FRAGMENT),
    },
    Binding {
        slot: 2,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::VERTEX,
    },
    Binding {
        slot: 0,
        kind: BindingKind::SampledImage {
            dimension: Dimension::D2,
            depth: false,
            multisampled: false,
        },
        stages: Stages::FRAGMENT,
    },
    Binding {
        slot: 1,
        kind: BindingKind::SampledImage {
            dimension: Dimension::D2,
            depth: false,
            multisampled: false,
        },
        stages: Stages::FRAGMENT,
    },
    Binding {
        slot: 0,
        kind: BindingKind::Sampler { comparison: false },
        stages: Stages::FRAGMENT,
    },
];

pub const OUTPUT: [Binding; 3] = [
    Binding {
        slot: 0,
        kind: BindingKind::UniformBuffer,
        stages: Stages::VERTEX.union(Stages::FRAGMENT),
    },
    Binding {
        slot: 2,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::VERTEX,
    },
    Binding {
        slot: 0,
        kind: BindingKind::SampledImage {
            dimension: Dimension::D2,
            depth: false,
            multisampled: false,
        },
        stages: Stages::FRAGMENT,
    },
];
