//! Vulkan extension dependency resolution, progressive fallback, and persistent
//! crash-sentinel / driver-health profiles (GFX-CACHE-4, GFX-CACHE-9).
//!
//! Kept free of `ash` types so extension rules, fallback ladders, and crash
//! recovery run natively in unit tests on every host target.

use crate::caps::Modern;
use crate::{Error, Result};
use std::collections::HashMap;
use std::ffi::CStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

pub(crate) const VK_1_1: u32 = (1 << 22) | (1 << 12);
pub(crate) const VK_1_2: u32 = (1 << 22) | (2 << 12);
pub(crate) const VK_1_3: u32 = (1 << 22) | (3 << 12);
pub(crate) const VK_1_4: u32 = (1 << 22) | (4 << 12);

const ARCHIVE_FILE: &str = "vulkan-health.bin";
const SENTINEL_FILE: &str = "vulkan-probe.bin";
const GUEST_SENTINEL_FILE: &str = "vulkan-guest-probe.bin";
const ARCHIVE_MAGIC: &[u8; 8] = b"ADVVKHL1";
const SENTINEL_MAGIC: &[u8; 8] = b"ADVVKPR1";
const ARCHIVE_VERSION: u32 = 1;
const MAX_PARTITIONS: usize = 4;
const MAX_BLOCKED_COMPILES: usize = 32;
const MAX_PROBE_LEN: usize = 128;

bitflags::bitflags! {
    /// Non-baseline Vulkan capabilities that can be individually masked when an
    /// ICD misreports dependencies, rejects creation, or fails a startup canary.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub(crate) struct Optional: u32 {
        const WEIGHTED_SAMPLING = 1 << 0;
        const MATRIX            = 1 << 1;
        const INT8_DOT          = 1 << 2;
        const SHADER_F16        = 1 << 3;
        const IEEE_F32          = 1 << 4;
        const TILE_PROPERTIES   = 1 << 5;
        const FORMAT_4444       = 1 << 6;
        const AHB_UBWC          = 1 << 7;
        const YCBCR             = 1 << 8;
        const MIRROR_CLAMP      = 1 << 9;

        const DEVICE_EXTENSIONS = Self::WEIGHTED_SAMPLING.bits()
            | Self::MATRIX.bits()
            | Self::INT8_DOT.bits()
            | Self::SHADER_F16.bits()
            | Self::IEEE_F32.bits()
            | Self::TILE_PROPERTIES.bits()
            | Self::FORMAT_4444.bits()
            | Self::YCBCR.bits()
            | Self::MIRROR_CLAMP.bits();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Mask {
    pub(crate) modern: Modern,
    pub(crate) optional: Optional,
}

impl Mask {
    #[cfg(test)]
    pub(crate) const fn empty() -> Self {
        Self {
            modern: Modern::empty(),
            optional: Optional::empty(),
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.modern.is_empty() && self.optional.is_empty()
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            modern: self.modern | other.modern,
            optional: self.optional | other.optional,
        }
    }
}

struct Quirk {
    vendor: u32,
    versions: std::ops::RangeInclusive<u32>,
    mask: Mask,
}

const QUIRKS: &[Quirk] = &[];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ProbeStep {
    OpenDevice = 1,
    CanaryRender = 2,
    CanaryPack4444 = 3,
    CanaryCompute = 4,
    CanaryPresent = 5,
    StartupInit = 6,
    GuestPipeline = 7,
}

impl ProbeStep {
    fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::OpenDevice),
            2 => Some(Self::CanaryRender),
            3 => Some(Self::CanaryPack4444),
            4 => Some(Self::CanaryCompute),
            5 => Some(Self::CanaryPresent),
            6 => Some(Self::StartupInit),
            7 => Some(Self::GuestPipeline),
            _ => None,
        }
    }
}

/// Physical-device extension and feature availability gathered before building
/// `VkDeviceCreateInfo`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QueriedSupport {
    pub(crate) api: u32,
    pub(crate) vendor_id: u32,
    pub(crate) driver_version: u32,
    pub(crate) android: bool,

    pub(crate) ext_ahb: bool,
    pub(crate) ext_semaphore_fd: bool,
    pub(crate) ext_foreign_queue: bool,
    pub(crate) ext_dynamic_rendering: bool,
    pub(crate) ext_depth_stencil_resolve: bool,
    pub(crate) ext_create_renderpass2: bool,
    pub(crate) ext_sync2: bool,
    pub(crate) ext_timeline: bool,
    pub(crate) ext_descriptor_indexing: bool,
    pub(crate) ext_push_descriptor: bool,
    pub(crate) ext_eds1: bool,
    pub(crate) ext_eds2: bool,
    pub(crate) ext_load_store_op_none_khr: bool,
    pub(crate) ext_load_store_op_none_ext: bool,
    pub(crate) ext_float_controls: bool,
    pub(crate) ext_float16_int8: bool,
    pub(crate) ext_int8_dot: bool,
    pub(crate) ext_vulkan_memory_model: bool,
    pub(crate) ext_coop_matrix_khr: bool,
    pub(crate) ext_coop_matrix_nv: bool,
    pub(crate) ext_qcom_image_processing: bool,
    pub(crate) ext_format_feature_flags2: bool,
    pub(crate) ext_4444_formats: bool,
    pub(crate) ext_mirror_clamp: bool,
    pub(crate) ext_qcom_tile_properties: bool,

    pub(crate) feat_ycbcr: bool,
    pub(crate) feat_dynamic_rendering: bool,
    pub(crate) feat_sync2: bool,
    pub(crate) feat_timeline: bool,
    pub(crate) feat_bindless: bool,
    pub(crate) feat_eds1: bool,
    pub(crate) feat_eds2: bool,
    pub(crate) feat_ieee_f32: bool,
    pub(crate) feat_shader_f16: bool,
    pub(crate) feat_int8_dot: bool,
    pub(crate) feat_memory_model: bool,
    pub(crate) feat_coop_matrix_khr_8x8_f16: bool,
    pub(crate) feat_coop_matrix_nv_8x8_f16: bool,
    pub(crate) feat_weighted_sampling: bool,
    pub(crate) feat_4444: bool,
    pub(crate) feat_tile_properties: bool,
    pub(crate) feat_imageless_framebuffer: bool,
}

/// Exact extensions, modern flags, and feature chains to pass to `vkCreateDevice`.
#[derive(Clone, Debug)]
pub(crate) struct DevicePlan {
    pub(crate) extensions: Vec<&'static CStr>,
    pub(crate) modern: Modern,
    pub(crate) active_optional: Optional,
    pub(crate) chain_eds_features: bool,
    pub(crate) use_ycbcr: bool,
    pub(crate) use_ieee_f32: bool,
    pub(crate) use_shader_f16: bool,
    pub(crate) use_int8_dot: bool,
    pub(crate) use_coop_matrix_khr: bool,
    pub(crate) use_coop_matrix_nv: bool,
    pub(crate) use_memory_model: bool,
    pub(crate) use_weighted_sampling: bool,
    pub(crate) use_4444: bool,
    pub(crate) use_mirror_clamp: bool,
    pub(crate) use_tile_properties: bool,
    #[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
    pub(crate) use_ahb_ubwc: bool,
}

impl DevicePlan {
    pub(crate) fn active_mask(&self) -> Mask {
        Mask {
            modern: self.modern,
            optional: self.active_optional,
        }
    }
}

pub(crate) fn select_device_plan(
    support: &QueriedSupport,
    caller_disable: Modern,
    health_mask: Mask,
) -> Result<DevicePlan> {
    if support.api < VK_1_1 {
        return Err(Error::Unavailable("Vulkan 1.1".into()));
    }
    let mut masked = Mask {
        modern: caller_disable | health_mask.modern,
        optional: health_mask.optional,
    };
    for quirk in QUIRKS {
        if quirk.vendor == support.vendor_id && quirk.versions.contains(&support.driver_version) {
            masked = masked.union(quirk.mask);
        }
    }

    fn push_ext(extensions: &mut Vec<&'static CStr>, ext: &'static CStr) {
        if !extensions.contains(&ext) {
            extensions.push(ext);
        }
    }
    let mut extensions = Vec::new();

    if support.android {
        for (present, name) in [
            (
                support.ext_ahb,
                c"VK_ANDROID_external_memory_android_hardware_buffer",
            ),
            (support.ext_semaphore_fd, c"VK_KHR_external_semaphore_fd"),
            (support.ext_foreign_queue, c"VK_EXT_queue_family_foreign"),
        ] {
            if !present {
                return Err(Error::Unavailable(name.to_string_lossy().into()));
            }
            push_ext(&mut extensions, name);
        }
    }

    let api = support.api;
    let mut modern = Modern::empty();
    let mut enable_modern = |extensions: &mut Vec<&'static CStr>,
                             flag: Modern,
                             ok: bool,
                             ext: &'static CStr,
                             promoted: u32|
     -> bool {
        if ok && !masked.modern.contains(flag) {
            modern |= flag;
            if api < promoted {
                push_ext(extensions, ext);
            }
            true
        } else {
            false
        }
    };

    let can_dynamic = support.feat_dynamic_rendering
        && (api >= VK_1_3
            || (support.ext_dynamic_rendering
                && (api >= VK_1_2
                    || (support.ext_depth_stencil_resolve && support.ext_create_renderpass2))));
    if enable_modern(
        &mut extensions,
        Modern::DYNAMIC_RENDERING,
        can_dynamic,
        c"VK_KHR_dynamic_rendering",
        VK_1_3,
    ) && api < VK_1_2
    {
        push_ext(&mut extensions, c"VK_KHR_create_renderpass2");
        push_ext(&mut extensions, c"VK_KHR_depth_stencil_resolve");
    }

    enable_modern(
        &mut extensions,
        Modern::SYNCHRONIZATION2,
        support.feat_sync2 && (api >= VK_1_3 || support.ext_sync2),
        c"VK_KHR_synchronization2",
        VK_1_3,
    );
    enable_modern(
        &mut extensions,
        Modern::TIMELINE_SEMAPHORES,
        support.feat_timeline && (api >= VK_1_2 || support.ext_timeline),
        c"VK_KHR_timeline_semaphore",
        VK_1_2,
    );
    enable_modern(
        &mut extensions,
        Modern::BINDLESS,
        support.feat_bindless && (api >= VK_1_2 || support.ext_descriptor_indexing),
        c"VK_EXT_descriptor_indexing",
        VK_1_2,
    );
    // `ash::khr::push_descriptor::Device` loads the KHR symbol, so always enable the extension.
    enable_modern(
        &mut extensions,
        Modern::PUSH_DESCRIPTORS,
        support.ext_push_descriptor,
        c"VK_KHR_push_descriptor",
        u32::MAX,
    );

    let can_eds = api >= VK_1_3
        || (support.feat_eds1 && support.feat_eds2 && support.ext_eds1 && support.ext_eds2);
    let use_eds = enable_modern(
        &mut extensions,
        Modern::EXTENDED_DYNAMIC_STATE,
        can_eds,
        c"VK_EXT_extended_dynamic_state",
        VK_1_3,
    );
    let chain_eds_features = use_eds && api < VK_1_3;
    if chain_eds_features {
        push_ext(&mut extensions, c"VK_EXT_extended_dynamic_state2");
    }

    let load_store_ext = if support.ext_load_store_op_none_khr {
        c"VK_KHR_load_store_op_none"
    } else {
        c"VK_EXT_load_store_op_none"
    };
    enable_modern(
        &mut extensions,
        Modern::LOAD_STORE_OP_NONE,
        api >= VK_1_4 || support.ext_load_store_op_none_khr || support.ext_load_store_op_none_ext,
        load_store_ext,
        VK_1_4,
    );

    let mut active_optional = Optional::empty();
    let mut mark_opt = |flag: Optional, cond: bool| -> bool {
        if cond && !masked.optional.contains(flag) {
            active_optional |= flag;
            true
        } else {
            false
        }
    };

    // Sampler YCbCr conversion is 1.1 core as an extension surface, though the
    // feature bit itself is optional in Vulkan 1.1.
    let use_ycbcr = mark_opt(Optional::YCBCR, support.feat_ycbcr);

    let use_ieee_f32 = mark_opt(
        Optional::IEEE_F32,
        support.feat_ieee_f32 && (api >= VK_1_2 || support.ext_float_controls),
    );
    if use_ieee_f32 && api < VK_1_2 {
        push_ext(&mut extensions, c"VK_KHR_shader_float_controls");
    }

    let use_shader_f16 = mark_opt(
        Optional::SHADER_F16,
        support.feat_shader_f16 && (api >= VK_1_2 || support.ext_float16_int8),
    );
    if use_shader_f16 && api < VK_1_2 {
        push_ext(&mut extensions, c"VK_KHR_shader_float16_int8");
    }

    let use_int8_dot = mark_opt(
        Optional::INT8_DOT,
        support.feat_int8_dot && (api >= VK_1_3 || support.ext_int8_dot),
    );
    if use_int8_dot && api < VK_1_3 {
        push_ext(&mut extensions, c"VK_KHR_shader_integer_dot_product");
    }

    let can_memory_model =
        support.feat_memory_model && (api >= VK_1_2 || support.ext_vulkan_memory_model);
    let has_khr_matrix = support.feat_coop_matrix_khr_8x8_f16 && support.ext_coop_matrix_khr;
    let has_nv_matrix = support.feat_coop_matrix_nv_8x8_f16 && support.ext_coop_matrix_nv;
    let use_any_matrix = mark_opt(
        Optional::MATRIX,
        use_shader_f16 && can_memory_model && (has_khr_matrix || has_nv_matrix),
    );
    let use_coop_matrix_khr = use_any_matrix && has_khr_matrix;
    let use_coop_matrix_nv = use_any_matrix && !has_khr_matrix && has_nv_matrix;
    if use_coop_matrix_khr {
        push_ext(&mut extensions, c"VK_KHR_cooperative_matrix");
    } else if use_coop_matrix_nv {
        push_ext(&mut extensions, c"VK_NV_cooperative_matrix");
    }
    let use_memory_model = use_any_matrix;
    if use_memory_model && api < VK_1_2 {
        push_ext(&mut extensions, c"VK_KHR_vulkan_memory_model");
    }

    // `VK_QCOM_image_processing` requires `VK_KHR_format_feature_flags2` (promoted in 1.3).
    let use_weighted_sampling = mark_opt(
        Optional::WEIGHTED_SAMPLING,
        support.feat_weighted_sampling
            && support.ext_qcom_image_processing
            && (api >= VK_1_3 || support.ext_format_feature_flags2),
    );
    if use_weighted_sampling {
        if api < VK_1_3 {
            push_ext(&mut extensions, c"VK_KHR_format_feature_flags2");
        }
        push_ext(&mut extensions, c"VK_QCOM_image_processing");
    }

    let use_4444 = mark_opt(
        Optional::FORMAT_4444,
        support.feat_4444 && support.ext_4444_formats,
    );
    if use_4444 {
        push_ext(&mut extensions, c"VK_EXT_4444_formats");
    }

    let use_mirror_clamp = mark_opt(Optional::MIRROR_CLAMP, support.ext_mirror_clamp);
    if use_mirror_clamp {
        push_ext(&mut extensions, c"VK_KHR_sampler_mirror_clamp_to_edge");
    }

    let use_tile_properties = mark_opt(
        Optional::TILE_PROPERTIES,
        api >= VK_1_2
            && support.ext_qcom_tile_properties
            && support.feat_tile_properties
            && support.feat_imageless_framebuffer,
    );
    if use_tile_properties {
        push_ext(&mut extensions, c"VK_QCOM_tile_properties");
    }

    let use_ahb_ubwc = mark_opt(Optional::AHB_UBWC, support.android);

    Ok(DevicePlan {
        extensions,
        modern,
        active_optional,
        chain_eds_features,
        use_ycbcr,
        use_ieee_f32,
        use_shader_f16,
        use_int8_dot,
        use_coop_matrix_khr,
        use_coop_matrix_nv,
        use_memory_model,
        use_weighted_sampling,
        use_4444,
        use_mirror_clamp,
        use_tile_properties,
        use_ahb_ubwc,
    })
}

/// Computes the next strictly stronger mask after `step` fails or crashes, or
/// `None` once the baseline Vulkan 1.1 floor is already reached.
pub(crate) fn next_fallback(step: ProbeStep, active: Mask, disabled: Mask) -> Option<Mask> {
    let add_opt = |flags: Optional| -> Option<Mask> {
        let next = disabled.union(Mask {
            modern: Modern::empty(),
            optional: flags,
        });
        (next != disabled).then_some(next)
    };
    let add_mod = |flags: Modern| -> Option<Mask> {
        let next = disabled.union(Mask {
            modern: flags,
            optional: Optional::empty(),
        });
        (next != disabled).then_some(next)
    };

    match step {
        ProbeStep::OpenDevice => add_opt(Optional::DEVICE_EXTENSIONS)
            .or_else(|| add_mod(Modern::BINDLESS))
            .or_else(|| add_mod(Modern::EXTENDED_DYNAMIC_STATE))
            .or_else(|| add_mod(Modern::DYNAMIC_RENDERING | Modern::LOAD_STORE_OP_NONE))
            .or_else(|| add_mod(Modern::PUSH_DESCRIPTORS))
            .or_else(|| add_mod(Modern::all()))
            .or_else(|| add_opt(Optional::AHB_UBWC)),
        ProbeStep::CanaryPack4444 => {
            if active.modern.contains(Modern::DYNAMIC_RENDERING)
                && let Some(next) = add_mod(Modern::DYNAMIC_RENDERING)
            {
                return Some(next);
            }
            add_opt(Optional::FORMAT_4444)
        }
        ProbeStep::CanaryRender => {
            let render_opts = active.optional & (Optional::IEEE_F32 | Optional::WEIGHTED_SAMPLING);
            if !render_opts.is_empty()
                && let Some(next) = add_opt(render_opts)
            {
                return Some(next);
            }
            for step_mod in [
                Modern::BINDLESS,
                Modern::EXTENDED_DYNAMIC_STATE,
                Modern::DYNAMIC_RENDERING | Modern::LOAD_STORE_OP_NONE,
                Modern::PUSH_DESCRIPTORS,
                Modern::SYNCHRONIZATION2 | Modern::TIMELINE_SEMAPHORES,
            ] {
                let live = active.modern & step_mod;
                if !live.is_empty()
                    && let Some(next) = add_mod(live)
                {
                    return Some(next);
                }
            }
            None
        }
        ProbeStep::CanaryCompute => {
            let compute_opts = active.optional
                & (Optional::MATRIX
                    | Optional::INT8_DOT
                    | Optional::SHADER_F16
                    | Optional::IEEE_F32);
            if !compute_opts.is_empty()
                && let Some(next) = add_opt(compute_opts)
            {
                return Some(next);
            }
            for step_mod in [
                Modern::BINDLESS,
                Modern::PUSH_DESCRIPTORS,
                Modern::SYNCHRONIZATION2 | Modern::TIMELINE_SEMAPHORES,
            ] {
                let live = active.modern & step_mod;
                if !live.is_empty()
                    && let Some(next) = add_mod(live)
                {
                    return Some(next);
                }
            }
            None
        }
        ProbeStep::CanaryPresent => {
            if active.optional.contains(Optional::AHB_UBWC)
                && let Some(next) = add_opt(Optional::AHB_UBWC)
            {
                return Some(next);
            }
            let sync = active.modern & (Modern::SYNCHRONIZATION2 | Modern::TIMELINE_SEMAPHORES);
            if !sync.is_empty() {
                return add_mod(sync);
            }
            None
        }
        ProbeStep::StartupInit => {
            // Post-open display pipelines fail first on bindless fragment storage
            // (Adreno) before dynamic state, dynamic rendering, push descriptors,
            // shader float controls, sync2/timeline, or optional YCbCr/AHB bits.
            for step_mod in [
                Modern::BINDLESS,
                Modern::EXTENDED_DYNAMIC_STATE,
                Modern::DYNAMIC_RENDERING | Modern::LOAD_STORE_OP_NONE,
                Modern::PUSH_DESCRIPTORS,
            ] {
                let live = active.modern & step_mod;
                if !live.is_empty()
                    && let Some(next) = add_mod(live)
                {
                    return Some(next);
                }
            }
            let render_opts = active.optional & (Optional::IEEE_F32 | Optional::WEIGHTED_SAMPLING);
            if !render_opts.is_empty()
                && let Some(next) = add_opt(render_opts)
            {
                return Some(next);
            }
            let sync = active.modern & (Modern::SYNCHRONIZATION2 | Modern::TIMELINE_SEMAPHORES);
            if !sync.is_empty()
                && let Some(next) = add_mod(sync)
            {
                return Some(next);
            }
            if !active.optional.is_empty() {
                return add_opt(active.optional);
            }
            None
        }
        ProbeStep::GuestPipeline => {
            for step_mod in [
                Modern::BINDLESS,
                Modern::EXTENDED_DYNAMIC_STATE,
                Modern::DYNAMIC_RENDERING | Modern::LOAD_STORE_OP_NONE,
                Modern::PUSH_DESCRIPTORS,
            ] {
                let live = active.modern & step_mod;
                if !live.is_empty()
                    && let Some(next) = add_mod(live)
                {
                    return Some(next);
                }
            }
            let render_opts = active.optional & (Optional::IEEE_F32 | Optional::WEIGHTED_SAMPLING);
            if !render_opts.is_empty() {
                return add_opt(render_opts);
            }
            None
        }
    }
}

/// Formats `AdapterIdentity::tier`: preserves exact `"bindless"`,
/// `"push_descriptors"`, or `"descriptor_sets"` when healthy, and appends
/// degraded feature annotations for diagnostics when fallback occurred.
pub(crate) fn format_tier(modern: Modern, degraded: Mask, crashed: bool) -> String {
    let base = if modern.contains(Modern::BINDLESS) {
        "bindless"
    } else if modern.contains(Modern::PUSH_DESCRIPTORS) {
        "push_descriptors"
    } else {
        "descriptor_sets"
    };
    if degraded.is_empty() && !crashed {
        return base.into();
    }
    let mut parts = Vec::new();
    for (flag, name) in [
        (Modern::BINDLESS, "bindless"),
        (Modern::PUSH_DESCRIPTORS, "push_descriptors"),
        (Modern::DYNAMIC_RENDERING, "dynamic_rendering"),
        (Modern::EXTENDED_DYNAMIC_STATE, "eds"),
        (Modern::LOAD_STORE_OP_NONE, "op_none"),
        (Modern::SYNCHRONIZATION2, "sync2"),
        (Modern::TIMELINE_SEMAPHORES, "timeline"),
    ] {
        if degraded.modern.contains(flag) {
            parts.push(name);
        }
    }
    for (flag, name) in [
        (Optional::WEIGHTED_SAMPLING, "qcom_image_processing"),
        (Optional::MATRIX, "coop_matrix"),
        (Optional::INT8_DOT, "int8_dot"),
        (Optional::SHADER_F16, "f16"),
        (Optional::IEEE_F32, "ieee_f32"),
        (Optional::TILE_PROPERTIES, "qcom_tile"),
        (Optional::FORMAT_4444, "4444"),
        (Optional::AHB_UBWC, "ubwc"),
        (Optional::YCBCR, "ycbcr"),
        (Optional::MIRROR_CLAMP, "mirror_clamp"),
    ] {
        if degraded.optional.contains(flag) {
            parts.push(name);
        }
    }
    let tag = if crashed { "crash-masked" } else { "masked" };
    if parts.is_empty() {
        format!("{base} ({tag})")
    } else {
        format!("{base} ({tag}: {})", parts.join(","))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AdapterHealth {
    pub(crate) degraded: Mask,
    pub(crate) verified: bool,
    pub(crate) crashed: bool,
    pub(crate) blocked_compiles: Vec<String>,
}

#[derive(Clone, Debug)]
struct ArchiveEntry {
    prefix: [u8; 28],
    health: AdapterHealth,
}

#[derive(Clone, Debug)]
enum Sentinel {
    Probe {
        prefix: [u8; 28],
        step: ProbeStep,
        active: Mask,
        disabled: Mask,
    },
    Compile {
        prefix: [u8; 28],
        probe: String,
    },
}

type ProcessCache = HashMap<([u8; 28], u32), AdapterHealth>;

#[derive(Default)]
struct StoreState {
    active_probe: HashMap<PathBuf, usize>,
    active_guest: HashMap<PathBuf, usize>,
}

static STORE_LOCK: LazyLock<Mutex<StoreState>> =
    LazyLock::new(|| Mutex::new(StoreState::default()));
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static PROCESS_CACHE: LazyLock<Mutex<ProcessCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

fn read_archive(dir: &Path) -> Vec<ArchiveEntry> {
    let Ok(bytes) = std::fs::read(dir.join(ARCHIVE_FILE)) else {
        return Vec::new();
    };
    parse_archive(&bytes).unwrap_or_default()
}

fn parse_archive(bytes: &[u8]) -> Option<Vec<ArchiveEntry>> {
    if bytes.len() < 24 || &bytes[..8] != ARCHIVE_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    if version != ARCHIVE_VERSION {
        return None;
    }
    let expected = u64::from_le_bytes(bytes[12..20].try_into().ok()?);
    let payload = &bytes[20..];
    if checksum(payload) != expected {
        return None;
    }
    let mut at = 0usize;
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let end = at.checked_add(n)?;
        let slice = payload.get(*at..end)?;
        *at = end;
        Some(slice)
    };
    let count = u32::from_le_bytes(take(&mut at, 4)?.try_into().ok()?) as usize;
    if count > MAX_PARTITIONS {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let prefix: [u8; 28] = take(&mut at, 28)?.try_into().ok()?;
        let modern_bits = u32::from_le_bytes(take(&mut at, 4)?.try_into().ok()?);
        let optional_bits = u32::from_le_bytes(take(&mut at, 4)?.try_into().ok()?);
        let flags = take(&mut at, 2)?;
        let verified = flags[0] != 0;
        let crashed = flags[1] != 0;
        let blocked_len = u16::from_le_bytes(take(&mut at, 2)?.try_into().ok()?) as usize;
        if blocked_len > MAX_BLOCKED_COMPILES {
            return None;
        }
        let mut blocked_compiles = Vec::with_capacity(blocked_len);
        for _ in 0..blocked_len {
            let slen = u16::from_le_bytes(take(&mut at, 2)?.try_into().ok()?) as usize;
            if slen > MAX_PROBE_LEN {
                return None;
            }
            let s = std::str::from_utf8(take(&mut at, slen)?).ok()?.to_owned();
            blocked_compiles.push(s);
        }
        entries.push(ArchiveEntry {
            prefix,
            health: AdapterHealth {
                degraded: Mask {
                    modern: Modern::from_bits_retain(modern_bits) & Modern::all(),
                    optional: Optional::from_bits_retain(optional_bits) & Optional::all(),
                },
                verified,
                crashed,
                blocked_compiles,
            },
        });
    }
    if at != payload.len() {
        return None;
    }
    Some(entries)
}

fn write_archive(dir: &Path, entries: &[ArchiveEntry]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let start = entries.len().saturating_sub(MAX_PARTITIONS);
    let kept = &entries[start..];
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(&(kept.len() as u32).to_le_bytes());
    for entry in kept {
        payload.extend_from_slice(&entry.prefix);
        payload.extend_from_slice(&entry.health.degraded.modern.bits().to_le_bytes());
        payload.extend_from_slice(&entry.health.degraded.optional.bits().to_le_bytes());
        payload.push(u8::from(entry.health.verified));
        payload.push(u8::from(entry.health.crashed));
        let blocked = &entry.health.blocked_compiles;
        let count = blocked.len().min(MAX_BLOCKED_COMPILES);
        payload.extend_from_slice(&(count as u16).to_le_bytes());
        for item in blocked.iter().take(count) {
            let b = &item.as_bytes()[..item.len().min(MAX_PROBE_LEN)];
            payload.extend_from_slice(&(b.len() as u16).to_le_bytes());
            payload.extend_from_slice(b);
        }
    }
    let mut out = Vec::with_capacity(20 + payload.len());
    out.extend_from_slice(ARCHIVE_MAGIC);
    out.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
    out.extend_from_slice(&checksum(&payload).to_le_bytes());
    out.extend_from_slice(&payload);

    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{ARCHIVE_FILE}.{}.{seq}.tmp", std::process::id()));
    let write_ok = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(&out)?;
        f.sync_all()
    });
    if write_ok.is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(ARCHIVE_FILE));
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn encode_sentinel(sentinel: &Sentinel) -> Vec<u8> {
    let mut payload = Vec::with_capacity(64);
    match sentinel {
        Sentinel::Probe {
            prefix,
            step,
            active,
            disabled,
        } => {
            payload.push(1);
            payload.extend_from_slice(prefix);
            payload.push(*step as u8);
            payload.extend_from_slice(&active.modern.bits().to_le_bytes());
            payload.extend_from_slice(&active.optional.bits().to_le_bytes());
            payload.extend_from_slice(&disabled.modern.bits().to_le_bytes());
            payload.extend_from_slice(&disabled.optional.bits().to_le_bytes());
        }
        Sentinel::Compile { prefix, probe } => {
            payload.push(2);
            payload.extend_from_slice(prefix);
            let bytes = &probe.as_bytes()[..probe.len().min(MAX_PROBE_LEN)];
            payload.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(bytes);
        }
    }
    let mut out = Vec::with_capacity(16 + payload.len());
    out.extend_from_slice(SENTINEL_MAGIC);
    out.extend_from_slice(&checksum(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

fn parse_sentinel(bytes: &[u8]) -> Option<Sentinel> {
    if bytes.len() < 17 || &bytes[..8] != SENTINEL_MAGIC {
        return None;
    }
    let expected = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let payload = &bytes[16..];
    if checksum(payload) != expected {
        return None;
    }
    match *payload.first()? {
        1 if payload.len() == 1 + 28 + 1 + 16 => {
            let prefix: [u8; 28] = payload[1..29].try_into().ok()?;
            let step = ProbeStep::from_u8(payload[29])?;
            let am = u32::from_le_bytes(payload[30..34].try_into().ok()?);
            let ao = u32::from_le_bytes(payload[34..38].try_into().ok()?);
            let dm = u32::from_le_bytes(payload[38..42].try_into().ok()?);
            let do_ = u32::from_le_bytes(payload[42..46].try_into().ok()?);
            Some(Sentinel::Probe {
                prefix,
                step,
                active: Mask {
                    modern: Modern::from_bits_retain(am) & Modern::all(),
                    optional: Optional::from_bits_retain(ao) & Optional::all(),
                },
                disabled: Mask {
                    modern: Modern::from_bits_retain(dm) & Modern::all(),
                    optional: Optional::from_bits_retain(do_) & Optional::all(),
                },
            })
        }
        2 if payload.len() >= 1 + 28 + 2 => {
            let prefix: [u8; 28] = payload[1..29].try_into().ok()?;
            let len = u16::from_le_bytes(payload[29..31].try_into().ok()?) as usize;
            if len > MAX_PROBE_LEN || payload.len() != 31 + len {
                return None;
            }
            let probe = std::str::from_utf8(&payload[31..]).ok()?.to_owned();
            Some(Sentinel::Compile { prefix, probe })
        }
        _ => None,
    }
}

fn arm_sentinel_at(dir: &Path, file: &str, sentinel: &Sentinel) {
    let bytes = encode_sentinel(sentinel);
    let path = dir.join(file);
    let write = || std::fs::File::create(&path).and_then(|mut f| f.write_all(&bytes));
    if let Err(err) = write()
        && err.kind() == std::io::ErrorKind::NotFound
        && std::fs::create_dir_all(dir).is_ok()
    {
        let _ = write();
    }
}

fn arm_sentinel(dir: &Path, sentinel: &Sentinel) {
    arm_sentinel_at(dir, SENTINEL_FILE, sentinel);
}

fn disarm_sentinel_at(dir: &Path, file: &str) {
    let _ = std::fs::remove_file(dir.join(file));
}

fn touch_entry<'a>(entries: &'a mut Vec<ArchiveEntry>, prefix: &[u8; 28]) -> &'a mut AdapterHealth {
    if let Some(pos) = entries.iter().position(|e| &e.prefix == prefix) {
        let entry = entries.remove(pos);
        entries.push(entry);
    } else {
        if entries.len() >= MAX_PARTITIONS {
            entries.remove(0);
        }
        entries.push(ArchiveEntry {
            prefix: *prefix,
            health: AdapterHealth::default(),
        });
    }
    &mut entries.last_mut().unwrap().health
}

fn reap_sentinel_file(dir: &Path, file: &str, entries: &mut Vec<ArchiveEntry>) -> bool {
    let path = dir.join(file);
    let Ok(bytes) = std::fs::read(&path) else {
        return false;
    };
    let _ = std::fs::remove_file(&path);
    let Some(sentinel) = parse_sentinel(&bytes) else {
        return false;
    };
    match sentinel {
        Sentinel::Probe {
            prefix,
            step,
            active,
            disabled,
        } => {
            let penalty = next_fallback(step, active, disabled).unwrap_or(disabled);
            let health = touch_entry(entries, &prefix);
            health.degraded = health.degraded.union(penalty);
            health.crashed = true;
            health.verified = false;
            true
        }
        Sentinel::Compile { prefix, probe } => {
            let health = touch_entry(entries, &prefix);
            health.crashed = true;
            if !health.blocked_compiles.contains(&probe) {
                if health.blocked_compiles.len() >= MAX_BLOCKED_COMPILES {
                    health.blocked_compiles.remove(0);
                }
                health.blocked_compiles.push(probe);
            }
            true
        }
    }
}

fn reap_sentinel_locked(dir: &Path, state: &StoreState, entries: &mut Vec<ArchiveEntry>) {
    let mut dirty = false;
    if !state.active_probe.contains_key(dir) {
        dirty |= reap_sentinel_file(dir, SENTINEL_FILE, entries);
    }
    if !state.active_guest.contains_key(dir) {
        dirty |= reap_sentinel_file(dir, GUEST_SENTINEL_FILE, entries);
    }
    if dirty {
        write_archive(dir, entries);
    }
}

/// Loads recorded health for `prefix`, folding in any unfinished crash sentinel.
pub(crate) fn load_health(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    caller_disable: Modern,
) -> AdapterHealth {
    let Some(dir) = cache_dir else {
        return PROCESS_CACHE
            .lock()
            .unwrap()
            .get(&(*prefix, caller_disable.bits()))
            .cloned()
            .unwrap_or_default();
    };
    let state = STORE_LOCK.lock().unwrap();
    let mut entries = read_archive(dir);
    reap_sentinel_locked(dir, &state, &mut entries);
    // Non-empty `caller_disable` is used by diagnostics and tests; honor disk
    // crash masks while forcing fresh verification when caller masks differ.
    let mut health = entries
        .iter()
        .find(|e| &e.prefix == prefix)
        .map(|e| e.health.clone())
        .unwrap_or_default();
    if !caller_disable.is_empty() {
        health.verified = false;
    }
    health
}

/// Records that `degraded` passed all startup canaries for `prefix`.
pub(crate) fn save_verified(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    caller_disable: Modern,
    degraded: Mask,
    crashed: bool,
) {
    let Some(dir) = cache_dir else {
        let mut map = PROCESS_CACHE.lock().unwrap();
        let entry = map.entry((*prefix, caller_disable.bits())).or_default();
        entry.degraded = degraded;
        entry.verified = true;
        entry.crashed |= crashed;
        return;
    };
    if !caller_disable.is_empty() {
        return;
    }
    let state = STORE_LOCK.lock().unwrap();
    let mut entries = read_archive(dir);
    reap_sentinel_locked(dir, &state, &mut entries);
    let health = touch_entry(&mut entries, prefix);
    health.degraded = degraded;
    health.verified = true;
    health.crashed |= crashed;
    write_archive(dir, &entries);
}

/// Clears the cached verification flag if warm startup initialization later
/// fails down to the floor.
#[cfg_attr(target_vendor = "apple", allow(dead_code))]
pub(crate) fn invalidate_health(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    caller_disable: Modern,
) {
    let Some(dir) = cache_dir else {
        let mut map = PROCESS_CACHE.lock().unwrap();
        if let Some(entry) = map.get_mut(&(*prefix, caller_disable.bits())) {
            entry.verified = false;
        }
        return;
    };
    if !caller_disable.is_empty() {
        return;
    }
    let state = STORE_LOCK.lock().unwrap();
    let mut entries = read_archive(dir);
    reap_sentinel_locked(dir, &state, &mut entries);
    if let Some(entry) = entries.iter_mut().find(|e| &e.prefix == prefix) {
        entry.health.verified = false;
        write_archive(dir, &entries);
    }
}

/// Runs one startup probe step inside a crash sentinel when `cache_dir` is set.
pub(crate) fn run_probe_step<T>(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    step: ProbeStep,
    active: Mask,
    disabled: Mask,
    f: impl FnOnce() -> T,
) -> T {
    if let Some(dir) = cache_dir {
        let mut state = STORE_LOCK.lock().unwrap();
        *state.active_probe.entry(dir.to_path_buf()).or_insert(0) += 1;
        arm_sentinel(
            dir,
            &Sentinel::Probe {
                prefix: *prefix,
                step,
                active,
                disabled,
            },
        );
    }
    struct Guard<'a>(Option<&'a Path>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            if let Some(dir) = self.0 {
                let mut state = STORE_LOCK.lock().unwrap();
                if let Some(count) = state.active_probe.get_mut(dir) {
                    *count -= 1;
                    if *count == 0 {
                        state.active_probe.remove(dir);
                        disarm_sentinel_at(dir, SENTINEL_FILE);
                    }
                }
            }
        }
    }
    let _disarm = Guard(cache_dir);
    f()
}

/// Guards a background shader or pipeline compilation against repeat driver
/// compiler crashes on the same adapter and driver version.
pub(crate) fn guard_compile<T>(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    probe: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let Some(dir) = cache_dir else {
        return f();
    };
    let trimmed =
        String::from_utf8_lossy(&probe.as_bytes()[..probe.len().min(MAX_PROBE_LEN)]).into_owned();
    {
        let mut state = STORE_LOCK.lock().unwrap();
        let mut entries = read_archive(dir);
        reap_sentinel_locked(dir, &state, &mut entries);
        if entries
            .iter()
            .any(|e| &e.prefix == prefix && e.health.blocked_compiles.contains(&trimmed))
        {
            return Err(Error::Compile(format!(
                "blocked after driver crash: {trimmed}"
            )));
        }
        *state.active_probe.entry(dir.to_path_buf()).or_insert(0) += 1;
        arm_sentinel(
            dir,
            &Sentinel::Compile {
                prefix: *prefix,
                probe: trimmed,
            },
        );
    }
    struct Guard<'a>(&'a Path);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let mut state = STORE_LOCK.lock().unwrap();
            if let Some(count) = state.active_probe.get_mut(self.0) {
                *count -= 1;
                if *count == 0 {
                    state.active_probe.remove(self.0);
                    disarm_sentinel_at(self.0, SENTINEL_FILE);
                }
            }
        }
    }
    let _disarm = Guard(dir);
    f()
}

fn record_runtime_demotion(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    caller_disable: Modern,
    penalty: Mask,
) {
    let Some(dir) = cache_dir else {
        let mut map = PROCESS_CACHE.lock().unwrap();
        let entry = map.entry((*prefix, caller_disable.bits())).or_default();
        entry.degraded = entry.degraded.union(penalty);
        entry.verified = false;
        return;
    };
    if !caller_disable.is_empty() {
        return;
    }
    let state = STORE_LOCK.lock().unwrap();
    let mut entries = read_archive(dir);
    reap_sentinel_locked(dir, &state, &mut entries);
    let health = touch_entry(&mut entries, prefix);
    health.degraded = health.degraded.union(penalty);
    health.verified = false;
    write_archive(dir, &entries);
}

/// Guards runtime guest shader and render-pipeline compilation: arms a
/// concurrent-safe crash sentinel and demotes one Vulkan tier step for the
/// next launch if the driver crashes or rejects valid lowered SPIR-V.
pub(crate) fn guard_guest_pipeline<T>(
    cache_dir: Option<&Path>,
    prefix: &[u8; 28],
    caller_disable: Modern,
    active: Mask,
    disabled: Mask,
    demoted: &AtomicBool,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let Some(penalty) = next_fallback(ProbeStep::GuestPipeline, active, disabled) else {
        return f();
    };
    if demoted.load(Ordering::Relaxed) {
        return f();
    }
    let armed_dir = cache_dir.filter(|_| caller_disable.is_empty());
    if let Some(dir) = armed_dir {
        let mut state = STORE_LOCK.lock().unwrap();
        let count = state.active_guest.entry(dir.to_path_buf()).or_insert(0);
        if *count == 0 {
            arm_sentinel_at(
                dir,
                GUEST_SENTINEL_FILE,
                &Sentinel::Probe {
                    prefix: *prefix,
                    step: ProbeStep::GuestPipeline,
                    active,
                    disabled,
                },
            );
        }
        *count += 1;
    }
    struct GuestGuard<'a>(Option<&'a Path>);
    impl Drop for GuestGuard<'_> {
        fn drop(&mut self) {
            if let Some(dir) = self.0 {
                let mut state = STORE_LOCK.lock().unwrap();
                if let Some(count) = state.active_guest.get_mut(dir) {
                    *count -= 1;
                    if *count == 0 {
                        state.active_guest.remove(dir);
                        disarm_sentinel_at(dir, GUEST_SENTINEL_FILE);
                    }
                }
            }
        }
    }
    let result = {
        let _disarm = GuestGuard(armed_dir);
        f()
    };
    if matches!(result, Err(Error::Unavailable(_) | Error::Compile(_)))
        && !demoted.swap(true, Ordering::Relaxed)
    {
        record_runtime_demotion(cache_dir, prefix, caller_disable, penalty);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adreno_730_1_1() -> QueriedSupport {
        QueriedSupport {
            api: VK_1_1,
            vendor_id: 0x5143,
            driver_version: (512 << 22) | (615 << 12),
            android: true,
            ext_ahb: true,
            ext_semaphore_fd: true,
            ext_foreign_queue: true,
            ext_push_descriptor: true,
            ext_descriptor_indexing: true,
            ext_timeline: true,
            ext_float16_int8: true,
            ext_int8_dot: true,
            ext_qcom_image_processing: true,
            ext_format_feature_flags2: false,
            ext_4444_formats: true,
            ext_mirror_clamp: true,
            feat_ycbcr: true,
            feat_timeline: true,
            feat_bindless: true,
            feat_shader_f16: true,
            feat_int8_dot: true,
            feat_weighted_sampling: true,
            feat_4444: true,
            ..Default::default()
        }
    }

    // spec: GFX-CACHE-4
    #[test]
    fn adreno_730_vulkan_1_1_gates_qcom_image_processing_on_format_feature_flags2() {
        let mut support = adreno_730_1_1();
        let without_flags2 = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(!without_flags2.use_weighted_sampling);
        assert!(
            !without_flags2
                .extensions
                .contains(&c"VK_QCOM_image_processing")
        );
        assert!(
            !without_flags2
                .extensions
                .contains(&c"VK_KHR_shader_float_controls")
        );

        support.ext_format_feature_flags2 = true;
        support.ext_float_controls = true;
        support.feat_ieee_f32 = true;
        let with_flags2 = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(with_flags2.use_weighted_sampling);
        assert!(with_flags2.use_ieee_f32);
        assert!(with_flags2.use_shader_f16);
        assert!(with_flags2.use_int8_dot);
        assert!(with_flags2.use_4444);
        assert!(with_flags2.use_mirror_clamp);
        assert!(!with_flags2.use_coop_matrix_khr);
        assert!(!with_flags2.use_coop_matrix_nv);
        assert!(!with_flags2.use_memory_model);
        assert!(!with_flags2.use_tile_properties);
        assert!(
            with_flags2
                .active_mask()
                .optional
                .contains(Optional::WEIGHTED_SAMPLING | Optional::SHADER_F16)
        );
        assert!(
            with_flags2
                .extensions
                .contains(&c"VK_KHR_format_feature_flags2")
        );
        assert!(
            with_flags2
                .extensions
                .contains(&c"VK_QCOM_image_processing")
        );
        assert!(
            with_flags2
                .extensions
                .contains(&c"VK_KHR_shader_float_controls")
        );
    }

    // spec: GFX-CACHE-4
    #[test]
    fn vulkan_1_1_dynamic_rendering_and_eds_pull_transitive_extensions_and_deduplicate() {
        let mut support = adreno_730_1_1();
        support.ext_dynamic_rendering = true;
        support.feat_dynamic_rendering = true;
        let missing_deps = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(!missing_deps.modern.contains(Modern::DYNAMIC_RENDERING));

        support.ext_create_renderpass2 = true;
        support.ext_depth_stencil_resolve = true;
        support.ext_eds1 = true;
        support.ext_eds2 = true;
        support.feat_eds1 = true;
        support.feat_eds2 = true;
        let full_1_1 = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(full_1_1.modern.contains(Modern::DYNAMIC_RENDERING));
        assert!(full_1_1.modern.contains(Modern::EXTENDED_DYNAMIC_STATE));
        assert!(full_1_1.chain_eds_features);
        for required in [
            c"VK_KHR_dynamic_rendering",
            c"VK_KHR_create_renderpass2",
            c"VK_KHR_depth_stencil_resolve",
            c"VK_EXT_extended_dynamic_state",
            c"VK_EXT_extended_dynamic_state2",
        ] {
            assert_eq!(
                full_1_1
                    .extensions
                    .iter()
                    .filter(|&&e| e == required)
                    .count(),
                1
            );
        }

        support.api = VK_1_3;
        let on_1_3 = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(on_1_3.modern.contains(Modern::EXTENDED_DYNAMIC_STATE));
        assert!(!on_1_3.chain_eds_features);
        assert!(
            !on_1_3
                .extensions
                .contains(&c"VK_EXT_extended_dynamic_state")
        );
    }

    // spec: GFX-CACHE-4
    #[test]
    fn missing_ycbcr_degrades_cleanly_without_blocking_device_open() {
        let mut support = adreno_730_1_1();
        support.feat_ycbcr = false;
        let plan = select_device_plan(&support, Modern::empty(), Mask::empty()).unwrap();
        assert!(!plan.use_ycbcr);
        assert!(plan.use_ahb_ubwc);
    }

    // spec: GFX-CACHE-4
    #[test]
    fn progressive_fallback_walks_down_to_vulkan_1_1_floor_and_terminates() {
        let mut disabled = Mask::empty();
        let mut steps = 0;
        while let Some(next) = next_fallback(ProbeStep::OpenDevice, Mask::empty(), disabled) {
            assert_ne!(next, disabled);
            disabled = next;
            steps += 1;
            assert!(steps <= 8);
        }
        assert_eq!(disabled.modern, Modern::all());
        assert!(
            disabled
                .optional
                .contains(Optional::DEVICE_EXTENSIONS | Optional::AHB_UBWC)
        );

        // StartupInit prioritizes stripping BINDLESS cleanly before touching float controls.
        let active = Mask {
            modern: Modern::all(),
            optional: Optional::IEEE_F32 | Optional::SHADER_F16,
        };
        let first = next_fallback(ProbeStep::StartupInit, active, Mask::empty()).unwrap();
        assert_eq!(first.modern, Modern::BINDLESS);
        assert!(first.optional.is_empty());
        let second = next_fallback(
            ProbeStep::StartupInit,
            Mask {
                modern: Modern::all() & !Modern::BINDLESS,
                optional: Optional::IEEE_F32 | Optional::SHADER_F16,
            },
            first,
        )
        .unwrap();
        assert_eq!(
            second.modern,
            Modern::BINDLESS | Modern::EXTENDED_DYNAMIC_STATE
        );
    }

    // spec: GFX-CACHE-4
    #[test]
    fn crash_sentinel_masks_crashing_step_and_blocks_crashing_compile() {
        let dir = std::env::temp_dir().join(format!(
            "adv-gpu-health-test-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let prefix = [7u8; 28];

        // Simulate a driver crash mid-CanaryRender with BINDLESS + PUSH_DESCRIPTORS active.
        let active = Mask {
            modern: Modern::BINDLESS | Modern::PUSH_DESCRIPTORS | Modern::DYNAMIC_RENDERING,
            optional: Optional::empty(),
        };
        arm_sentinel(
            &dir,
            &Sentinel::Probe {
                prefix,
                step: ProbeStep::CanaryRender,
                active,
                disabled: Mask::empty(),
            },
        );
        let recovered = load_health(Some(&dir), &prefix, Modern::empty());
        assert!(recovered.crashed);
        assert!(!recovered.verified);
        assert_eq!(recovered.degraded.modern, Modern::BINDLESS);
        assert!(!dir.join(SENTINEL_FILE).exists());
        assert_eq!(
            format_tier(
                Modern::PUSH_DESCRIPTORS,
                recovered.degraded,
                recovered.crashed
            ),
            "push_descriptors (crash-masked: bindless)"
        );

        // Verify and persist warm health; sentinel must be absent afterwards.
        let ran = run_probe_step(
            Some(&dir),
            &prefix,
            ProbeStep::CanaryRender,
            Mask {
                modern: Modern::PUSH_DESCRIPTORS | Modern::DYNAMIC_RENDERING,
                optional: Optional::empty(),
            },
            recovered.degraded,
            || 42,
        );
        assert_eq!(ran, 42);
        assert!(!dir.join(SENTINEL_FILE).exists());
        save_verified(
            Some(&dir),
            &prefix,
            Modern::empty(),
            recovered.degraded,
            recovered.crashed,
        );
        let warm = load_health(Some(&dir), &prefix, Modern::empty());
        assert!(warm.verified);
        assert!(warm.crashed);
        assert_eq!(warm.degraded.modern, Modern::BINDLESS);

        // Simulate a background compiler crash inside `guard_compile`.
        arm_sentinel(
            &dir,
            &Sentinel::Compile {
                prefix,
                probe: "upscale:Anime4K:f16".into(),
            },
        );
        let mut called = false;
        let blocked = guard_compile(Some(&dir), &prefix, "upscale:Anime4K:f16", || {
            called = true;
            Ok(())
        });
        assert!(!called);
        assert!(matches!(blocked, Err(Error::Compile(_))));
        // Other compile keys remain allowed.
        let allowed = guard_compile(Some(&dir), &prefix, "upscale:Anime4K:f32", || Ok(99)).unwrap();
        assert_eq!(allowed, 99);
        assert!(!dir.join(SENTINEL_FILE).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // spec: GFX-CACHE-4
    // spec: GFX-CACHE-9
    #[test]
    fn corrupt_archives_are_ignored_and_partitions_cap_at_four() {
        let dir = std::env::temp_dir().join(format!(
            "adv-gpu-health-cap-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ARCHIVE_FILE), b"ADVVKHL1corrupt").unwrap();
        assert!(
            load_health(Some(&dir), &[1u8; 28], Modern::empty())
                .degraded
                .is_empty()
        );

        for id in 0..6u8 {
            save_verified(
                Some(&dir),
                &[id; 28],
                Modern::empty(),
                Mask {
                    modern: Modern::BINDLESS,
                    optional: Optional::empty(),
                },
                false,
            );
        }
        assert!(!load_health(Some(&dir), &[0u8; 28], Modern::empty()).verified);
        assert!(!load_health(Some(&dir), &[1u8; 28], Modern::empty()).verified);
        for id in 2..6u8 {
            assert!(load_health(Some(&dir), &[id; 28], Modern::empty()).verified);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // spec: GFX-CACHE-4
    #[test]
    fn startup_init_strips_float_controls_before_timeline_and_keeps_ycbcr_until_floor() {
        let mut disabled = Mask::empty();
        let mut active = Mask {
            modern: Modern::BINDLESS
                | Modern::PUSH_DESCRIPTORS
                | Modern::TIMELINE_SEMAPHORES
                | Modern::SYNCHRONIZATION2,
            optional: Optional::IEEE_F32 | Optional::YCBCR,
        };
        let mut order = Vec::new();
        while let Some(next) = next_fallback(ProbeStep::StartupInit, active, disabled) {
            let delta = Mask {
                modern: next.modern & !disabled.modern,
                optional: next.optional & !disabled.optional,
            };
            order.push(delta);
            active.modern &= !next.modern;
            active.optional &= !next.optional;
            disabled = next;
        }
        assert_eq!(
            order,
            vec![
                Mask {
                    modern: Modern::BINDLESS,
                    optional: Optional::empty(),
                },
                Mask {
                    modern: Modern::PUSH_DESCRIPTORS,
                    optional: Optional::empty(),
                },
                Mask {
                    modern: Modern::empty(),
                    optional: Optional::IEEE_F32,
                },
                Mask {
                    modern: Modern::SYNCHRONIZATION2 | Modern::TIMELINE_SEMAPHORES,
                    optional: Optional::empty(),
                },
                Mask {
                    modern: Modern::empty(),
                    optional: Optional::YCBCR,
                },
            ]
        );
    }

    // spec: GFX-CACHE-4
    #[test]
    fn guest_pipeline_sentinel_is_concurrent_safe_and_demotes_on_crash_or_error() {
        let dir = std::env::temp_dir().join(format!(
            "adv-gpu-guest-health-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let prefix = [9u8; 28];
        let active = Mask {
            modern: Modern::BINDLESS
                | Modern::EXTENDED_DYNAMIC_STATE
                | Modern::DYNAMIC_RENDERING
                | Modern::PUSH_DESCRIPTORS
                | Modern::TIMELINE_SEMAPHORES,
            optional: Optional::IEEE_F32 | Optional::YCBCR,
        };
        let demoted = AtomicBool::new(false);

        // Nested/concurrent guest pipeline compiles keep `vulkan-guest-probe.bin`
        // armed until the outermost finishes and never interfere with `guard_compile`.
        let res = guard_guest_pipeline(
            Some(&dir),
            &prefix,
            Modern::empty(),
            active,
            Mask::empty(),
            &demoted,
            || {
                assert!(dir.join(GUEST_SENTINEL_FILE).exists());
                // A live in-process sentinel is not falsely reaped as a crash.
                assert!(!load_health(Some(&dir), &prefix, Modern::empty()).crashed);
                let inner = guard_guest_pipeline(
                    Some(&dir),
                    &prefix,
                    Modern::empty(),
                    active,
                    Mask::empty(),
                    &demoted,
                    || {
                        let up = guard_compile(Some(&dir), &prefix, "upscale:test", || Ok(11))?;
                        assert!(!dir.join(SENTINEL_FILE).exists());
                        assert!(dir.join(GUEST_SENTINEL_FILE).exists());
                        Ok(up)
                    },
                )?;
                assert!(dir.join(GUEST_SENTINEL_FILE).exists());
                Ok(inner + 1)
            },
        )
        .unwrap();
        assert_eq!(res, 12);
        assert!(!dir.join(GUEST_SENTINEL_FILE).exists());
        assert!(!demoted.load(Ordering::Relaxed));

        // OOM does not demote the adapter tier.
        let oom: Result<()> = guard_guest_pipeline(
            Some(&dir),
            &prefix,
            Modern::empty(),
            active,
            Mask::empty(),
            &demoted,
            || Err(Error::OutOfMemory),
        );
        assert_eq!(oom, Err(Error::OutOfMemory));
        assert!(!demoted.load(Ordering::Relaxed));
        assert!(
            load_health(Some(&dir), &prefix, Modern::empty())
                .degraded
                .is_empty()
        );

        // Driver rejection (`Unavailable`) demotes one step (`BINDLESS`) for next launch.
        let err: Result<()> = guard_guest_pipeline(
            Some(&dir),
            &prefix,
            Modern::empty(),
            active,
            Mask::empty(),
            &demoted,
            || Err(Error::Unavailable("Vulkan: ERROR_UNKNOWN".into())),
        );
        assert!(matches!(err, Err(Error::Unavailable(_))));
        assert!(demoted.load(Ordering::Relaxed));
        let after_err = load_health(Some(&dir), &prefix, Modern::empty());
        assert_eq!(after_err.degraded.modern, Modern::BINDLESS);
        assert!(!after_err.verified);
        assert!(!after_err.crashed);

        // Simulate a native crash mid-guest compile on the stepped-down session.
        let stepped_active = Mask {
            modern: active.modern & !Modern::BINDLESS,
            optional: active.optional,
        };
        arm_sentinel_at(
            &dir,
            GUEST_SENTINEL_FILE,
            &Sentinel::Probe {
                prefix,
                step: ProbeStep::GuestPipeline,
                active: stepped_active,
                disabled: after_err.degraded,
            },
        );
        let after_crash = load_health(Some(&dir), &prefix, Modern::empty());
        assert!(after_crash.crashed);
        assert!(!after_crash.verified);
        assert_eq!(
            after_crash.degraded.modern,
            Modern::BINDLESS | Modern::EXTENDED_DYNAMIC_STATE
        );
        assert!(!dir.join(GUEST_SENTINEL_FILE).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
