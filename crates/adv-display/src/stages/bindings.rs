//! Binding descriptions shared by stage pipelines and host-shader compilation.
use adv_gpu::Format;
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

const fn sampled(slot: u32) -> Binding {
    Binding {
        slot,
        kind: BindingKind::SampledImage {
            dimension: Dimension::D2,
            depth: false,
            multisampled: false,
        },
        stages: Stages::COMPUTE,
    }
}

const fn storage(slot: u32, writable: bool) -> Binding {
    Binding {
        slot,
        kind: BindingKind::StorageBuffer { writable },
        stages: Stages::COMPUTE,
    }
}

/// Anchors: both inputs, the frame, anchors, state, the pass graph and the
/// facts.
pub const UPSCALE_ANCHOR: [Binding; 7] = [
    sampled(0),
    sampled(1),
    storage(0, false),
    storage(1, true),
    storage(2, true),
    storage(3, false),
    storage(4, true),
];
/// Compare: both inputs, the frame, the facts, state, the pass graph and the
/// anchors.
pub const UPSCALE_DELTA: [Binding; 7] = [
    sampled(0),
    sampled(1),
    storage(0, false),
    storage(1, true),
    storage(2, true),
    storage(3, false),
    storage(4, false),
];
/// Bookkeeping: the frame, the facts, state, the pass graph, the lists and
/// anchors.
pub const UPSCALE_FILTER: [Binding; 6] = [
    storage(0, false),
    storage(1, false),
    storage(2, true),
    storage(3, false),
    storage(4, true),
    storage(5, true),
];
/// Copies: the image read, the entries, the frame, state, the pass graph and
/// the image written.
pub const UPSCALE_COPY: [Binding; 6] = [
    sampled(0),
    storage(0, false),
    storage(1, false),
    storage(2, false),
    storage(3, false),
    Binding {
        slot: 4,
        kind: BindingKind::StorageImage {
            format: Format::Rgba8Unorm,
            writable: true,
        },
        stages: Stages::COMPUTE,
    },
];

/// GPU diagnostic history, shared by compute and presentation draws.
pub const DAMAGE: [Binding; 5] = [
    Binding {
        slot: 1,
        kind: BindingKind::UniformBuffer,
        stages: Stages::COMPUTE.union(Stages::FRAGMENT),
    },
    Binding {
        slot: 4,
        kind: BindingKind::StorageBuffer { writable: true },
        stages: Stages::COMPUTE.union(Stages::FRAGMENT),
    },
    Binding {
        slot: 5,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::COMPUTE,
    },
    Binding {
        slot: 6,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::COMPUTE,
    },
    Binding {
        slot: 7,
        kind: BindingKind::StorageBuffer { writable: false },
        stages: Stages::COMPUTE,
    },
];
