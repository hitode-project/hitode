//! Mandatory feature-level rows, including the optional SM4 compute subset.
use super::{Coverage, Error};
use adv_gpu::{Caps, Features};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FeatureLevel {
    L9_1 = 0x9100,
    L9_2 = 0x9200,
    L9_3 = 0x9300,
    L10_0 = 0xa000,
    L10_1 = 0xa100,
    L11_0 = 0xb000,
}

impl FeatureLevel {
    pub const ALL: [Self; 6] = [
        Self::L9_1,
        Self::L9_2,
        Self::L9_3,
        Self::L10_0,
        Self::L10_1,
        Self::L11_0,
    ];
    pub const DEFAULT: [u32; 6] = [0xb000, 0xa100, 0xa000, 0x9300, 0x9200, 0x9100];

    pub(in crate::d3d11) fn column(self) -> usize {
        match self {
            Self::L9_1 => 0,
            Self::L9_2 => 1,
            Self::L9_3 => 2,
            Self::L10_0 => 3,
            Self::L10_1 => 4,
            Self::L11_0 => 5,
        }
    }

    pub(super) fn requested(value: u32) -> Result<Option<Self>, Error> {
        match value {
            0x9100 => Ok(Some(Self::L9_1)),
            0x9200 => Ok(Some(Self::L9_2)),
            0x9300 => Ok(Some(Self::L9_3)),
            0xa000 => Ok(Some(Self::L10_0)),
            0xa100 => Ok(Some(Self::L10_1)),
            0xb000 => Ok(Some(Self::L11_0)),
            0xb100 | 0xc000 | 0xc100 | 0xc200 => Ok(None),
            _ => Err(Error::InvalidArgument),
        }
    }

    /// Admission limits for a requested level, even on a higher-capability GPU.
    pub fn limit(self, requirement: Requirement) -> u32 {
        requirements(self)
            .iter()
            .find(|&&(entry, _)| entry == requirement)
            .unwrap()
            .1
    }
}

macro_rules! requirements {
    ($($name:ident $key:literal => $path:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Requirement { $($name),+ }

        impl Requirement {
            pub const ALL: &'static [Self] = &[$(Self::$name),+];
            pub const SPEC_TEST: &str = "GFX-CAP-4 / mandatory_rows_follow_device_and_coverage";

            fn parse(key: &str) -> Self {
                match key {
                    $($key => Self::$name,)+
                    _ => panic!("unknown feature-level requirement {key}"),
                }
            }

            pub fn path(self) -> &'static str {
                match self { $(Self::$name => $path,)+ }
            }
        }
    };
}

requirements! {
    VsPsProfile "vs_ps_profile" => "DXBC vertex and pixel translation",
    GeometryShader "geometry_shader" => "geometry expansion",
    GeometryShaderInstancing "geometry_shader_instancing" => "geometry expansion instances",
    StreamOutputStreams "stream_output_streams" => "expansion stream output",
    HullDomainShaders "hull_domain_shaders" => "tessellation expansion",
    MaxTessellationFactor "max_tessellation_factor" => "fixed-function tessellator",
    ClassLinkage "class_linkage" => "DXBC class specialization",
    ComputeProfile "compute_profile" => "DXBC compute translation",
    ComputeThreads "compute_threads" => "compute workgroup admission",
    ComputeSharedBytes "compute_shared_bytes" => "groupshared allocation",
    IaSlots "ia_slots" => "input assembler streams",
    PsInputRegisters "ps_input_registers" => "packed or pulled pixel varyings",
    ConstantBufferSlots "constant_buffer_slots_per_stage" => "constant ring binding",
    ConstantBufferBytes "constant_buffer_bytes" => "constant ring windows",
    ShaderResourceSlots "shader_resource_slots_per_stage" => "view descriptor binding",
    SamplerSlots "sampler_slots_per_stage" => "sampler descriptor binding",
    ComputeUavSlots "compute_uav_slots" => "compute unordered bindings",
    OutputMergerUavSlots "output_merger_uav_slots" => "fragment unordered bindings",
    Texture1d "texture_1d_extent" => "one-dimensional image creation",
    Texture2d "texture_2d_extent" => "two-dimensional image creation",
    Texture3d "texture_3d_extent" => "volume image creation",
    TextureCube "texture_cube_extent" => "cube image creation",
    TextureArrayLayers "texture_array_layers" => "image array creation",
    CubeArrays "cube_arrays" => "cube array sampling",
    TextureRepeat "texture_repeat" => "texture address calculation",
    Anisotropy "anisotropy" => "anisotropic sampling",
    InputPrimitiveCount "input_primitive_count" => "draw range validation",
    VertexIndex "vertex_index" => "indexed draw input",
    RenderTargets "render_targets" => "color attachment binding",
    DualSourceBlend "dual_source_blend" => "dual-source output merger",
    IndependentWriteMasks "independent_write_masks" => "per-target write masks",
    IndependentBlend "independent_blend" => "per-target blend state",
    SeparateAlphaBlend "separate_alpha_blend" => "separate alpha blend state",
    AlphaToCoverage "alpha_to_coverage" => "multisample alpha coverage",
    MirrorOnce "mirror_once" => "native or portable sampler addressing",
    OverlappingVertexElements "overlapping_vertex_elements" => "vertex fetch",
    InstancedDraw "instanced_draw" => "instanced draw encoding",
    IndirectDraw "indirect_draw" => "indirect draw encoding",
    IndirectDispatch "indirect_dispatch" => "indirect compute encoding",
    DrawAuto "draw_auto" => "stream-output draw count",
    Viewports "viewports" => "native viewports or expansion bins",
    Scissors "scissors" => "native scissors or expansion bins",
    DepthClipDisable "depth_clip_disable" => "depth clamp raster state",
    DepthBiasClamp "depth_bias_clamp" => "clamped depth bias",
    SampleInterpolation "sample_interpolation" => "per-sample interpolation",
    PixelShaderCoverage "pixel_shader_coverage" => "pixel shader sample mask",
    FullNonPowerOfTwo "full_non_power_of_two" => "non-power-of-two texture addressing",
    Bc1ThroughBc3 "bc1_through_bc3" => "BC1–3 hardware sampling or upload decode",
    Bc4Bc5 "bc4_bc5" => "BC4–5 hardware sampling or upload decode",
    Bc6hBc7 "bc6h_bc7" => "BC6H–7 hardware sampling or upload decode",
    TypedUavStores "typed_uav_stores" => "typed storage writes",
    R32TypedUavLoads "r32_typed_uav_loads" => "R32 image or buffer loads",
    R32UavAtomics "r32_uav_atomics" => "R32 image or buffer atomics",
    UavCounters "uav_counters_append_consume" => "buffer counters and append/consume",
    QueryOcclusion "query_occlusion" => "exact occlusion query",
    QueryPipelineStatistics "query_pipeline_statistics" => "pipeline query or counting variant",
    QueryStreamOutputStatistics "query_stream_output_statistics" => "expansion stream counters",
    QueryEvent "query_event" => "submission completion",
    QueryTimestampAndDisjoint "query_timestamp_and_disjoint" => "timestamp or disjoint query",
    ColorBytesPerSample "color_bytes_per_sample" => "attachment storage per sample",
    FramebufferLayers "framebuffer_layers" => "native layers or expansion windows",
    Wireframe "wireframe" => "native raster state or wireframe expansion",
    ClipDistances "clip_distances" => "native clip distances or pixel discard",
    CullDistances "cull_distances" => "cull-distance expansion",
    MultisampleRenderTargets "multisample_render_targets" => "multisample color and depth attachments",
    ImmediateContext "immediate_context" => "immediate recording",
    DeferredContext "deferred_context" => "command-list recording and execution",
}

impl Requirement {
    pub(super) fn optional(self, level: FeatureLevel) -> bool {
        level < FeatureLevel::L11_0
            && matches!(
                self,
                Self::ComputeProfile
                    | Self::ComputeThreads
                    | Self::ComputeSharedBytes
                    | Self::ComputeUavSlots
            )
    }

    pub(super) fn available(self, caps: &Caps, paths: Coverage<'_>, value: u32) -> bool {
        if value == 0 {
            return true;
        }
        if paths.limit(self) < value {
            return false;
        }
        let limits = &caps.limits;
        let features = caps.features;
        let native = match self {
            Self::GeometryShader
            | Self::GeometryShaderInstancing
            | Self::StreamOutputStreams
            | Self::HullDomainShaders
            | Self::DrawAuto
            | Self::IndirectDraw
            | Self::IndirectDispatch => features.contains(Features::INDIRECT),
            Self::ComputeThreads => limits.max_workgroup_invocations >= value,
            Self::ComputeSharedBytes => limits.max_workgroup_storage >= value,
            Self::PsInputRegisters => limits.max_fragment_input_components / 4 >= value,
            Self::ConstantBufferSlots => limits.max_uniform_buffers >= value,
            Self::ConstantBufferBytes => limits.max_uniform_buffer_bytes >= value,
            Self::ShaderResourceSlots => limits.max_sampled_images >= value,
            Self::SamplerSlots => limits.max_samplers >= value,
            Self::ComputeUavSlots | Self::OutputMergerUavSlots => {
                limits.max_storage_bindings >= value
            }
            Self::Texture1d | Self::Texture2d | Self::TextureCube => limits.max_image_2d >= value,
            Self::Texture3d => limits.max_image_3d >= value,
            Self::TextureArrayLayers => limits.max_image_layers >= value,
            Self::RenderTargets => limits.max_color_attachments >= value,
            Self::DualSourceBlend => features.contains(Features::DUAL_SOURCE_BLEND),
            Self::Viewports | Self::Scissors => limits.max_viewports >= value,
            Self::DepthClipDisable => features.contains(Features::DEPTH_CLAMP),
            Self::Bc1ThroughBc3 | Self::Bc4Bc5 | Self::Bc6hBc7 => {
                features.contains(Features::BC_TEXTURES)
            }
            Self::R32UavAtomics => features.contains(Features::IMAGE_ATOMICS),
            Self::QueryOcclusion => features.contains(Features::OCCLUSION_QUERIES),
            Self::QueryTimestampAndDisjoint => features.contains(Features::TIMESTAMPS),
            Self::ColorBytesPerSample => limits.max_color_bytes_per_sample >= value,
            Self::FramebufferLayers => limits.max_framebuffer_layers >= value,
            Self::MultisampleRenderTargets => limits.sample_counts & value != 0,
            Self::VsPsProfile
            | Self::MaxTessellationFactor
            | Self::ClassLinkage
            | Self::ComputeProfile
            | Self::IaSlots
            | Self::CubeArrays
            | Self::TextureRepeat
            | Self::Anisotropy
            | Self::InputPrimitiveCount
            | Self::VertexIndex
            | Self::IndependentWriteMasks
            | Self::IndependentBlend
            | Self::SeparateAlphaBlend
            | Self::AlphaToCoverage
            | Self::MirrorOnce
            | Self::OverlappingVertexElements
            | Self::InstancedDraw
            | Self::DepthBiasClamp
            | Self::SampleInterpolation
            | Self::PixelShaderCoverage
            | Self::FullNonPowerOfTwo
            | Self::TypedUavStores
            | Self::R32TypedUavLoads
            | Self::UavCounters
            | Self::QueryPipelineStatistics
            | Self::QueryStreamOutputStatistics
            | Self::QueryEvent
            | Self::Wireframe
            | Self::ClipDistances
            | Self::CullDistances
            | Self::ImmediateContext
            | Self::DeferredContext => true,
        };
        native || paths.emulated.contains(&self)
    }
}

pub(super) fn requirements(level: FeatureLevel) -> &'static [(Requirement, u32)] {
    const DATA: &str = include_str!("../../../../../data/gfx/feature-levels.tsv");
    static TABLE: OnceLock<[Vec<(Requirement, u32)>; 6]> = OnceLock::new();
    &TABLE.get_or_init(|| {
        let mut rows = std::array::from_fn(|_| Vec::new());
        for line in DATA.lines().filter(|line| !line.starts_with('#')).skip(2) {
            let mut fields = line.split('\t');
            let requirement = Requirement::parse(fields.next().unwrap());
            for (column, field) in fields.enumerate() {
                let value = match field {
                    "-" => 0,
                    "4_0_level_9_1" => 1,
                    "4_0_level_9_3" => 2,
                    "4_0" | "optional_4_0" => 3,
                    "4_1" | "optional_4_1" => 4,
                    "5_0" => 5,
                    value => value.parse().unwrap(),
                };
                rows[column].push((requirement, value));
            }
        }
        rows
    })[level.column()]
}
