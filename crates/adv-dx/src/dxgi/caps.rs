//! DXGI descriptions and presentation capability derivation.
use super::abi::*;
use adv_gpu::{Caps, Format, FormatCaps};
use std::num::NonZeroU32;

/// Hardware identity and memory budgets supplied by assembly.
#[derive(Clone, Debug, Default)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub subsystem: u32,
    pub revision: u32,
    pub driver_version: u64,
    pub dedicated_video: u64,
    pub dedicated_system: u64,
    pub shared_system: u64,
}

impl AdapterInfo {
    pub fn from_gpu(caps: &Caps) -> Self {
        // The backend has no texture-memory budget. The CPU surface arena
        // contributes nothing to these estimates (see d3d9::memory).
        Self {
            name: caps.adapter.clone(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    Hardware,
    Software,
    Null,
}

pub const ADAPTERS: [Adapter; 2] = [Adapter::Hardware, Adapter::Software];

impl Adapter {
    pub(super) fn description(self, gpu: &AdapterInfo) -> DXGI_ADAPTER_DESC2 {
        let (name, vendor, device, subsystem, revision, video, system, shared, flags, luid) =
            match self {
                Self::Hardware => (
                    gpu.name.as_str(),
                    gpu.vendor,
                    gpu.device,
                    gpu.subsystem,
                    gpu.revision,
                    gpu.dedicated_video,
                    gpu.dedicated_system,
                    gpu.shared_system,
                    0,
                    1,
                ),
                Self::Null => ("", 0, 0, 0, 0, 0, 0, 0, 0, 0),
                Self::Software => (
                    "Microsoft Basic Render Driver",
                    0x1414,
                    0x008c,
                    0,
                    0,
                    0,
                    0,
                    0,
                    2,
                    2,
                ),
            };
        DXGI_ADAPTER_DESC2 {
            Description: wide_name(name),
            VendorId: vendor,
            DeviceId: device,
            SubSysId: subsystem,
            Revision: revision,
            DedicatedVideoMemory: memory32(video),
            DedicatedSystemMemory: memory32(system),
            SharedSystemMemory: memory32(shared),
            AdapterLuid: LUID {
                LowPart: luid,
                HighPart: 0,
            },
            Flags: flags,
            // The virtual adapter schedules complete DMA buffers.
            GraphicsPreemptionGranularity: 0,
            ComputePreemptionGranularity: 0,
        }
    }
}

fn memory32(bytes: u64) -> u32 {
    bytes.min(u64::from(u32::MAX)) as u32
}

fn wide_name<const N: usize>(name: &str) -> [u16; N] {
    let mut result = [0; N];
    for (out, unit) in result[..N - 1].iter_mut().zip(name.encode_utf16()) {
        *out = unit;
    }
    result
}

/// A physical mode supplied by the display owner. Scaling and scanline
/// ordering have the DXGI values.
#[derive(Clone, Copy, Debug)]
pub struct PhysicalMode {
    pub size: (u32, u32),
    pub refresh: (u32, NonZeroU32),
    pub scaling: i32,
    pub scanline: i32,
    pub stereo: bool,
}

/// Snapshot from the same display owner that supplies desktop geometry and
/// DirectDraw's modes. DPI awareness can differ between calling threads.
#[derive(Clone, Debug)]
pub struct Display {
    pub name: String,
    pub monitor: u32,
    pub physical_rect: [i32; 4],
    pub dpi: NonZeroU32,
    pub dpi_aware: bool,
    pub modes: Vec<PhysicalMode>,
}

impl Display {
    /// GFX-DXGI-4: desktop coordinates virtualize, mode extents stay physical.
    pub fn description(&self) -> DXGI_OUTPUT_DESC {
        let virtualize = |value: i32| {
            if self.dpi_aware {
                value
            } else {
                let dpi = i64::from(self.dpi.get());
                let numerator = i64::from(value) * 96;
                ((numerator + numerator.signum() * (dpi / 2)) / dpi) as i32
            }
        };
        let [left, top, right, bottom] = self.physical_rect.map(virtualize);
        DXGI_OUTPUT_DESC {
            DeviceName: wide_name(&self.name),
            DesktopCoordinates: RECT {
                left,
                top,
                right,
                bottom,
            },
            AttachedToDesktop: 1,
            Rotation: 1,
            Monitor: self.monitor,
        }
    }
}

/// Swap effects covered by the Windows mode table. Presentation coverage is
/// supplied separately from the device's format support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapEffect {
    Discard,
    Sequential,
    FlipSequential,
    FlipDiscard,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SwapCoverage {
    pub bitblt: bool,
    pub flip: bool,
    pub srgb: bool,
    pub multisample: bool,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct SwapModel {
    pub effect: SwapEffect,
    pub min_buffers: u32,
    pub max_buffers: u32,
    pub sequential_buffers: bool,
    pub occluded_status: bool,
}

impl SwapEffect {
    /// GFX-DXGI-2: DXGI swap-effect rows, measured by dxgi_semantics.
    pub fn model(self) -> SwapModel {
        let (min_buffers, sequential_buffers, occluded_status) = match self {
            Self::Discard => (1, false, true),
            Self::Sequential => (1, true, true),
            Self::FlipSequential => (2, true, false),
            Self::FlipDiscard => (2, false, false),
        };
        SwapModel {
            effect: self,
            min_buffers,
            max_buffers: 3,
            sequential_buffers,
            occluded_status,
        }
    }
}

impl SwapModel {
    /// Derive support from the presenter's executing path and the backing format.
    pub fn supports(
        self,
        caps: &Caps,
        paths: SwapCoverage,
        format: u32,
        buffers: u32,
        samples: u32,
        flags: u32,
    ) -> bool {
        let Some(host) = present_format(format) else {
            return false;
        };
        let flip = matches!(
            self.effect,
            SwapEffect::FlipSequential | SwapEffect::FlipDiscard
        );
        let path = if flip { paths.flip } else { paths.bitblt };
        let srgb = matches!(host, Format::Rgba8Srgb | Format::Bgra8Srgb);
        path && (self.min_buffers..=self.max_buffers).contains(&buffers)
            && flags & !paths.flags == 0
            && (!srgb || (!flip && paths.srgb))
            && caps.format(host).contains(FormatCaps::COLOR)
            && (samples == 1
                || (!flip
                    && paths.multisample
                    && samples.is_power_of_two()
                    && caps.format_sample_counts(host) & samples != 0))
    }
}

/// GFX-CAP-1: formats supported by the present conversion.
pub(crate) fn present_format(format: u32) -> Option<Format> {
    Some(match format {
        10 => Format::Rgba16Float,
        24 => Format::Rgb10a2Unorm,
        28 => Format::Rgba8Unorm,
        29 => Format::Rgba8Srgb,
        87 => Format::Bgra8Unorm,
        91 => Format::Bgra8Srgb,
        _ => return None,
    })
}

impl Display {
    pub fn description1(&self) -> DXGI_OUTPUT_DESC1 {
        let base = self.description();
        DXGI_OUTPUT_DESC1 {
            DeviceName: base.DeviceName,
            DesktopCoordinates: base.DesktopCoordinates,
            AttachedToDesktop: base.AttachedToDesktop,
            Rotation: base.Rotation,
            Monitor: base.Monitor,
            BitsPerColor: 8,
            ColorSpace: 0,
            RedPrimary: [0.64, 0.33],
            GreenPrimary: [0.30, 0.60],
            BluePrimary: [0.15, 0.06],
            WhitePoint: [0.3127, 0.3290],
            ..Default::default()
        }
    }
}
