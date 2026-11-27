//! Feature-query layouts and the level masks applied to their derived values.
use super::{Coverage, Error, FeatureLevel, FormatSupport};
use adv_abi::types::*;
use adv_gpu::Caps;
use std::collections::BTreeMap;

macro_rules! features {
    ($($variant:ident = $value:literal => $layout:ty, $path:literal);+ $(;)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u32)]
        pub enum Feature { $($variant = $value),+ }

        impl Feature {
            pub const ALL: [Self; 21] = [$(Self::$variant),+];
            pub(super) const COUNT: usize = Self::ALL.len();
            pub const SPEC_TEST: &str = "GFX-CAP-4 / feature_layouts_masks_and_creation_status";
            pub fn size(self) -> usize {
                match self { $(Self::$variant => size_of::<$layout>()),+ }
            }
            pub fn path(self) -> &'static str {
                match self { $(Self::$variant => $path),+ }
            }
            pub(super) fn parse(value: u32) -> Result<Self, Error> {
                match value { $($value => Ok(Self::$variant),)+ _ => Err(Error::InvalidArgument) }
            }
        }
    }
}

features! {
    Threading = 0 => D3D11_FEATURE_DATA_THREADING, "device creation and deferred command lists";
    Doubles = 1 => D3D11_FEATURE_DATA_DOUBLES, "excluded fp64 shader operations";
    FormatSupport = 2 => D3D11_FEATURE_DATA_FORMAT_SUPPORT, "resource backing and format operations";
    FormatSupport2 = 3 => D3D11_FEATURE_DATA_FORMAT_SUPPORT2, "resource backing and storage operations";
    Compute4x = 4 => D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS, "downlevel compute admission and dispatch";
    Options = 5 => D3D11_FEATURE_DATA_D3D11_OPTIONS, "context operations, dynamic maps and integer shader lowering";
    Architecture = 6 => D3D11_FEATURE_DATA_ARCHITECTURE_INFO, "adapter tile-based rendering";
    D3d9Options = 7 => D3D11_FEATURE_DATA_D3D9_OPTIONS, "non-power-of-two texture addressing";
    MinPrecision = 8 => D3D11_FEATURE_DATA_SHADER_MIN_PRECISION_SUPPORT, "full-precision shader lowering";
    D3d9Shadow = 9 => D3D11_FEATURE_DATA_D3D9_SHADOW_SUPPORT, "comparison samplers";
    Options1 = 10 => D3D11_FEATURE_DATA_D3D11_OPTIONS1, "depth ClearView";
    D3d9Instancing = 11 => D3D11_FEATURE_DATA_D3D9_SIMPLE_INSTANCING_SUPPORT, "instanced draw";
    Markers = 12 => D3D11_FEATURE_DATA_MARKER_SUPPORT, "excluded guest profiler markers";
    D3d9Options1 = 13 => D3D11_FEATURE_DATA_D3D9_OPTIONS1, "texture addressing, comparison samplers, instancing and depth cube views";
    Options2 = 14 => D3D11_FEATURE_DATA_D3D11_OPTIONS2, "typed UAV loads and adapter memory architecture";
    Options3 = 15 => D3D11_FEATURE_DATA_D3D11_OPTIONS3, "array-index routing restricted to geometry expansion";
    VirtualAddress = 16 => D3D11_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT, "backend buffer size limit";
    Options4 = 17 => D3D11_FEATURE_DATA_D3D11_OPTIONS4, "excluded extended NV12 sharing";
    ShaderCache = 18 => D3D11_FEATURE_DATA_SHADER_CACHE, "excluded guest-visible shader cache";
    Options5 = 19 => D3D11_FEATURE_DATA_D3D11_OPTIONS5, "excluded shared resource tiers";
    Displayable = 20 => D3D11_FEATURE_DATA_DISPLAYABLE, "excluded displayable surfaces";
}

/// Optional feature bits with a distinct executing path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeaturePath {
    ConcurrentCreates,
    CommandLists,
    Discard,
    UpdateCopyFlags,
    ClearView,
    OverlappingCopy,
    PartialConstantUpdate,
    ConstantOffsets,
    DynamicConstantNoOverwrite,
    DynamicSrvNoOverwrite,
    Sad4,
    FullNonPowerOfTwo,
    ShadowComparison,
    ClearDepthView,
    SimpleInstancing,
    MixedCubeDepthView,
}

impl FeaturePath {
    pub const SPEC_TEST: &str = "GFX-CAP-4 / feature_layouts_masks_and_creation_status";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureAnswer {
    values: [u32; 14],
    length: usize,
}

impl FeatureAnswer {
    pub(super) fn new<const N: usize>(words: [u32; N]) -> Self {
        let mut values = [0; 14];
        values[..N].copy_from_slice(&words);
        Self { values, length: N }
    }

    pub fn words(&self) -> &[u32] {
        &self.values[..self.length]
    }
}

pub(super) fn derive(
    caps: &Caps,
    paths: Coverage<'_>,
    level: FeatureLevel,
    compute: bool,
    formats: &BTreeMap<u32, FormatSupport>,
) -> [FeatureAnswer; Feature::COUNT] {
    use FeaturePath::*;
    let path = |operation| paths.feature(operation);
    let level_10 = level >= FeatureLevel::L10_0;
    let level_11 = level >= FeatureLevel::L11_0;
    // Microsoft's all-or-nothing typed load group added in D3D11.3.
    let additional_loads = [2, 3, 4, 10, 12, 14, 28, 30, 32, 54, 57, 59, 61, 62, 64]
        .into_iter()
        .all(|format| {
            formats
                .get(&format)
                .is_some_and(|row| row.support2 & 0x40 != 0)
        });
    let va_bits = u64::BITS
        - caps
            .limits
            .max_buffer_size
            .saturating_sub(1)
            .leading_zeros();
    Feature::ALL.map(|feature| match feature {
        Feature::Threading => FeatureAnswer::new([path(ConcurrentCreates), path(CommandLists)]),
        Feature::Doubles => FeatureAnswer::new([0]),
        Feature::FormatSupport | Feature::FormatSupport2 => FeatureAnswer::new([0, 0]),
        Feature::Compute4x => FeatureAnswer::new([u32::from(level_10 && compute)]),
        Feature::Options => FeatureAnswer::new([
            0, // Logic operations.
            0, // Forced sample count on UAV-only draws.
            path(Discard),
            path(UpdateCopyFlags),
            path(ClearView),
            path(OverlappingCopy),
            path(PartialConstantUpdate),
            path(ConstantOffsets),
            path(DynamicConstantNoOverwrite),
            u32::from(level_10) * path(DynamicSrvNoOverwrite),
            0, // Forced sample count on bound multisample RTVs.
            u32::from(level_11) * path(Sad4),
            0, // Extended doubles.
            0, // Extended sharing.
        ]),
        Feature::Architecture => FeatureAnswer::new([1]), // All GPUs in scope are tile based.
        Feature::D3d9Options => FeatureAnswer::new([path(FullNonPowerOfTwo)]),
        Feature::MinPrecision => FeatureAnswer::new([0, 0]),
        Feature::D3d9Shadow => FeatureAnswer::new([path(ShadowComparison)]),
        Feature::Options1 => FeatureAnswer::new([0, 0, path(ClearDepthView), 0]),
        Feature::D3d9Instancing => FeatureAnswer::new([path(SimpleInstancing)]),
        Feature::Markers => FeatureAnswer::new([0]),
        Feature::D3d9Options1 => FeatureAnswer::new([
            path(FullNonPowerOfTwo),
            path(ShadowComparison),
            path(SimpleInstancing),
            path(MixedCubeDepthView),
        ]),
        Feature::Options2 => FeatureAnswer::new([
            0, // Pixel shader specified stencil reference.
            u32::from(level_11 && additional_loads),
            0, // Rasterizer ordered views and conservative raster require FL 11_1.
            0,
            0, // Tiled resources, mapping default textures and standard swizzle.
            0,
            0,
            1, // Every GPU in scope has unified memory.
        ]),
        Feature::Options3 => FeatureAnswer::new([0]),
        Feature::VirtualAddress => FeatureAnswer::new([va_bits, va_bits]),
        Feature::Options4 | Feature::ShaderCache | Feature::Options5 => FeatureAnswer::new([0]),
        Feature::Displayable => FeatureAnswer::new([0, 0]),
    })
}
