//! Draw-time shader specialization. The selected value is part of the pipeline key.

use super::analysis::Reflection;
use super::dxbc::fields::D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL;
use super::ir::{BindingKind, Interpolation, Register, Stage, SystemValue, ValueType};
use adv_device::gpu::{Caps, Features};
use smallvec::SmallVec;

const PACK_VARYINGS: u16 = 1 << 0;
const CLIP_AS_DISCARD: u16 = 1 << 1;
const FORCE_EARLY_TESTS: u16 = 1 << 2;
const COUNT_FRAGMENTS: u16 = 1 << 3;
const PULL: u16 = 1 << 4;
const STORAGE_VARYINGS: u16 = 1 << 5;
const BIN: u16 = 1 << 6;
const CULL_DISTANCES: u16 = 1 << 7;
const WIREFRAME: u16 = 1 << 8;
const COUNT_CLIPPED: u16 = 1 << 9;
const COUNT_EXPANDED: u16 = 1 << 10;
const CLIP_VARYINGS: u16 = 1 << 11;
const CLIP_NATIVE: u16 = 1 << 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bin {
    pub viewport: u16,
    pub first_layer: u16,
    pub layers: u16,
}

#[derive(Default)]
pub struct DrawFacts<'a> {
    /// The linked pixel shader's identity and reflection, for the producer.
    pub fragment: Option<(u64, &'a Reflection)>,
    /// Clip distances written by the raster source, including those unread by the PS.
    pub clip_components: u8,
    /// Identity of the producer whose registers a storage-backed PS reads.
    pub producer_identity: Option<u64>,
    pub bin: Bin,
    pub viewport_count: u32,
    pub layer_count: u32,
    pub wireframe: bool,
    pub geometry: bool,
    pub tessellated: bool,
    pub stream_output: bool,
    pub alpha_to_coverage: bool,
    pub statistics: bool,
    /// Rasterization is disabled by SO without a rasterized stream, or by a
    /// null PS with both depth and stencil disabled.
    pub rasterization_disabled: bool,
    pub storage_formats: &'a [(u32, naga::StorageFormat)],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Variant {
    bits: u16,
    bin: Bin,
    clip_components: u8,
    linkage: Option<u64>,
    storage_formats: SmallVec<[(u32, naga::StorageFormat); 8]>,
}

impl Variant {
    pub fn select(reflection: &Reflection, facts: &DrawFacts<'_>, caps: &Caps) -> Self {
        let fragment = if reflection.stage == Stage::Pixel {
            Some(reflection)
        } else {
            facts.fragment.map(|(_, reflection)| reflection)
        };
        let raster = !facts.rasterization_disabled && reflection.stage != Stage::Compute;
        let producer = matches!(
            reflection.stage,
            Stage::Vertex | Stage::Domain | Stage::Geometry
        );
        let emulate_clip = raster
            && facts.clip_components != 0
            && !caps.features.contains(Features::CLIP_DISTANCES);
        let mut bits = 0;
        let mut set = |flag, enabled| {
            if enabled {
                bits |= flag;
            }
        };
        let clip_varyings = raster
            && facts.clip_components != 0
            && fragment.is_some_and(|ps| {
                ps.signatures
                    .input
                    .iter()
                    .any(|input| input.system_value == SystemValue::ClipDistance)
            });
        let packed = fragment.map_or(0, |ps| {
            packed_components(ps, facts.clip_components, emulate_clip || clip_varyings)
        });
        set(PACK_VARYINGS, raster && fragment.is_some());
        let overflow = raster && packed > caps.limits.max_fragment_input_components;
        set(STORAGE_VARYINGS, overflow);
        set(CLIP_AS_DISCARD, emulate_clip);
        set(CLIP_VARYINGS, clip_varyings);
        set(
            CLIP_NATIVE,
            raster && producer && facts.clip_components != 0 && !emulate_clip,
        );
        let count =
            facts.statistics && !caps.features.contains(Features::PIPELINE_STATISTICS_QUERY);
        let count_fragments = raster && count && reflection.stage == Stage::Pixel;
        set(COUNT_FRAGMENTS, count_fragments);
        let early = reflection.global_flags & D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL != 0;
        let can_force = !reflection.discard
            && !emulate_clip
            && !facts.alpha_to_coverage
            && !reflection.signatures.output.iter().any(|e| {
                matches!(
                    e.system_value,
                    SystemValue::Depth
                        | SystemValue::DepthGreaterEqual
                        | SystemValue::DepthLessEqual
                        | SystemValue::Coverage
                )
            })
            && !reflection
                .writes
                .keys()
                .any(|(register, _)| *register == Register::UnorderedAccessView);
        set(
            FORCE_EARLY_TESTS,
            reflection.stage == Stage::Pixel && (early || count_fragments && can_force),
        );
        let cull = raster
            && producer
            && reflection
                .signatures
                .output
                .iter()
                .any(|e| e.system_value == SystemValue::CullDistance);
        let wireframe = raster && facts.wireframe && !caps.features.contains(Features::WIREFRAME);
        let bin = raster && (facts.viewport_count > 1 || facts.layer_count > 1);
        set(CULL_DISTANCES, cull);
        set(WIREFRAME, wireframe);
        set(BIN, bin);
        set(COUNT_CLIPPED, raster && producer && count);
        set(
            COUNT_EXPANDED,
            reflection.stage == Stage::Geometry && facts.statistics,
        );
        set(
            PULL,
            producer
                && (overflow
                    || cull
                    || wireframe
                    || bin
                    || raster && count
                    || facts.geometry
                    || facts.tessellated
                    || facts.stream_output),
        );

        let mut storage_formats = SmallVec::new();
        for binding in &reflection.bindings {
            if binding.register == Register::UnorderedAccessView
                && matches!(binding.kind, BindingKind::Typed { .. })
                && let Some(format) = facts
                    .storage_formats
                    .iter()
                    .find(|(slot, _)| *slot == binding.slot)
            {
                storage_formats.push(*format);
            }
        }
        storage_formats.sort_unstable_by_key(|&(slot, _)| slot);
        Self {
            bits,
            bin: if bin { facts.bin } else { Bin::default() },
            clip_components: if raster { facts.clip_components } else { 0 },
            linkage: if producer && fragment.is_some() {
                facts.fragment.map(|(id, _)| id)
            } else if overflow {
                facts.producer_identity
            } else {
                None
            },
            storage_formats,
        }
    }

    pub fn pipeline_identity(&self, shader_identity: u64) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        (shader_identity, self).hash(&mut hasher);
        hasher.finish()
    }

    pub fn bin(&self) -> Bin {
        self.bin
    }
    pub(super) fn pull_input(&self) -> Self {
        let mut variant = self.clone();
        variant.bits |= PULL;
        variant
    }

    pub fn pulled(&self) -> bool {
        self.bits & PULL != 0
    }
    pub fn storage_varyings(&self) -> bool {
        self.bits & STORAGE_VARYINGS != 0
    }
    pub(crate) fn count_expanded(&self) -> bool {
        self.bits & COUNT_EXPANDED != 0
    }
    pub fn count_clipped(&self) -> bool {
        self.bits & COUNT_CLIPPED != 0
    }
    pub fn cull_distances(&self) -> bool {
        self.bits & CULL_DISTANCES != 0
    }
    pub fn wireframe(&self) -> bool {
        self.bits & WIREFRAME != 0
    }
    pub fn binned(&self) -> bool {
        self.bits & BIN != 0
    }
    pub(crate) fn pack(&self) -> bool {
        self.bits & PACK_VARYINGS != 0
    }
    pub(crate) fn native_clip(&self) -> bool {
        self.bits & CLIP_NATIVE != 0
    }
    pub(crate) fn clip_varyings(&self) -> bool {
        self.bits & CLIP_VARYINGS != 0
    }
    pub(crate) fn clip_as_discard(&self) -> bool {
        self.bits & CLIP_AS_DISCARD != 0
    }
    pub(crate) fn clip_components(&self) -> u8 {
        self.clip_components
    }
    pub(crate) fn early_tests(&self) -> bool {
        self.bits & FORCE_EARLY_TESTS != 0
    }
    pub(crate) fn count_fragments(&self) -> bool {
        self.bits & COUNT_FRAGMENTS != 0
    }
    pub(crate) fn storage_formats(&self) -> &[(u32, naga::StorageFormat)] {
        &self.storage_formats
    }
}

pub(crate) fn interpolation(
    reflection: &Reflection,
    ty: ValueType,
    register: u32,
) -> Interpolation {
    if ty != ValueType::Float {
        Interpolation::Flat
    } else {
        match reflection.interpolation.get(&register).copied() {
            None | Some(Interpolation::Undefined) => Interpolation::Perspective,
            Some(mode) => mode,
        }
    }
}

fn packed_components(reflection: &Reflection, clip: u8, emulate_clip: bool) -> u32 {
    reflection
        .signatures
        .input
        .iter()
        .filter(|element| element.system_value == SystemValue::None)
        .map(|element| element.mask.count_ones())
        .sum::<u32>()
        + if emulate_clip { u32::from(clip) } else { 0 }
}
