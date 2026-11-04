//! What the device offers beyond the platform baseline (see the crate docs).
//! Optional features select faster paths or enable later front ends.

use crate::Format;

#[derive(Clone, Debug)]
pub struct Caps {
    pub features: Features,
    /// 32-bit shader arithmetic preserves infinities and NaNs.
    pub ieee_f32: bool,
    /// Modern mechanisms the backend uses natively on this device. Everything
    /// absent runs through its stand-in; callers see the same API and
    /// behavior either way. For reports and the flight recorder only.
    pub modern: Modern,
    pub limits: Limits,
    /// Adapter name and driver version, for reports and the flight recorder.
    pub adapter: String,
    pub driver: String,
    /// Filled by the backend at open; read through [`Caps::format`].
    pub(crate) formats: Vec<(Format, FormatCaps)>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Features: u64 {
        /// Half precision arithmetic in shaders, outside buffer and stage
        /// interfaces: phones lack 16-bit storage access (validation layers
        /// catch such a shader in CI).
        const SHADER_F16 = 1 << 0;
        /// Subgroup (SIMD-group) operations and barriers.
        const SUBGROUP = 1 << 1;
        /// 8×8 half precision cooperative or SIMD-group matrices.
        const MATRIX_F16_8X8 = 1 << 2;
        /// Attachments that live only in tile memory (memoryless, lazily allocated).
        const TRANSIENT_ATTACHMENTS = 1 << 3;
        /// GPU timestamps at pass boundaries, for the cost model.
        const TIMESTAMPS = 1 << 5;
        /// Presentable buffers the system compositor can scan out
        /// (AHardwareBuffer or IOSurface).
        const PRESENTABLE_BUFFERS = 1 << 6;
        /// Occlusion queries with exact sample counts.
        const OCCLUSION_QUERIES = 1 << 7;
        /// Pipeline caches that persist across launches.
        const PIPELINE_CACHE = 1 << 8;
        /// BC1–BC3 sampling in hardware. Without it, front ends decode on upload.
        const BC_TEXTURES = 1 << 9;
        /// Indirect draws and dispatches.
        const INDIRECT = 1 << 10;
        /// Multisampled render targets and resolves.
        const MULTISAMPLE = 1 << 11;
        /// 8-bit integer dot products (CuNNy's quantized layers on Adreno,
        /// which has no cooperative matrices).
        const INT8_DOT_PRODUCT = 1 << 12;
        /// Hardware weighted texture sampling (`VK_QCOM_image_processing`).
        const WEIGHTED_SAMPLING = 1 << 13;
        /// Tile shaders and imageblocks (Metal) for passes that stay in tile memory.
        const TILE_SHADERS = 1 << 14;
        /// Sampling decoder output with an external YCbCr format.
        const YCBCR_SAMPLING = 1 << 15;
        // Reserved for D3D10/11 front ends. No backend sets these yet; Metal has
        // no geometry stage, so a front end emulates it with compute plus
        // indirect draws instead of waiting for this bit.
        const GEOMETRY_SHADER = 1 << 32;
        const TESSELLATION = 1 << 33;
        const STREAM_OUTPUT = 1 << 34;
    }
}

bitflags::bitflags! {
    /// Modern Vulkan mechanisms, each chosen independently at [`crate::Device::open`]
    /// (see the crate docs for the stand-ins). A feature is used only when the
    /// driver reports it, the backend's quirk table does not mark that driver
    /// version as broken for it, and [`crate::queue::DeviceOptions::disable`]
    /// does not mask it. Metal reports none: its baseline needs no stand-ins.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Modern: u32 {
        const DYNAMIC_RENDERING = 1 << 0;
        const SYNCHRONIZATION2 = 1 << 1;
        const TIMELINE_SEMAPHORES = 1 << 2;
        /// Descriptor indexing: one bindless heap, indices in push constants.
        const BINDLESS = 1 << 3;
        /// The first stand-in for bindless; without it, cached descriptor sets.
        const PUSH_DESCRIPTORS = 1 << 4;
        /// Extended dynamic state 1 and 2.
        const EXTENDED_DYNAMIC_STATE = 1 << 5;
        const LOAD_STORE_OP_NONE = 1 << 6;
    }
}

impl Modern {
    /// What GPU tests mask: `ADV_GPU_TEST_DISABLE=all` masks everything, so
    /// one test binary covers both tiers, and flag names such as
    /// `BINDLESS | PUSH_DESCRIPTORS` mask a subset. Only tests and probes call it.
    #[doc(hidden)]
    pub fn from_test_env() -> Self {
        match std::env::var("ADV_GPU_TEST_DISABLE").as_deref() {
            Ok("all") => Self::all(),
            Ok(names) => bitflags::parser::from_str(names)
                .unwrap_or_else(|_| panic!("ADV_GPU_TEST_DISABLE: unknown flags {names:?}")),
            Err(_) => Self::empty(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_image_2d: u32,
    pub max_image_3d: u32,
    pub max_image_layers: u32,
    pub max_color_attachments: u32,
    /// Per stage, per binding kind. D3D11 needs 14 uniform buffers, 128 sampled
    /// images, 16 samplers and 8 (11.1: 64) storage bindings.
    pub max_uniform_buffers: u32,
    pub max_sampled_images: u32,
    pub max_samplers: u32,
    pub max_storage_bindings: u32,
    pub max_workgroup_storage: u32,
    pub max_workgroup_invocations: u32,
    pub uniform_offset_alignment: u32,
    pub storage_offset_alignment: u32,
    /// Bitmask of supported sample counts (bit n = 2^n samples).
    pub sample_counts: u32,
    /// Hardware tile or bin size in pixels, for aligning damage tiles
    /// (`VK_QCOM_tile_properties`, Apple's tile size); `(0, 0)` when unknown.
    pub tile: (u32, u32),
    /// Push-constant bytes per draw (Vulkan); bindless indices live there.
    pub push_constant_bytes: u32,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct FormatCaps: u32 {
        const SAMPLED = 1 << 0;
        const FILTERED = 1 << 1;
        const COLOR = 1 << 2;
        const BLEND = 1 << 3;
        const DEPTH_STENCIL = 1 << 4;
        const STORAGE = 1 << 5;
        const PRESENTABLE = 1 << 6;
        /// Presentable buffers of this format are losslessly compressed, so
        /// rendering into them costs what a private image costs.
        const PRESENTABLE_COMPRESSED = 1 << 8;
        /// The display controller can scan this format directly.
        const SCANOUT = 1 << 7;
    }
}

impl Caps {
    pub fn format(&self, format: Format) -> FormatCaps {
        self.formats
            .iter()
            .find(|(f, _)| *f == format)
            .map_or(FormatCaps::empty(), |&(_, caps)| caps)
    }
}
