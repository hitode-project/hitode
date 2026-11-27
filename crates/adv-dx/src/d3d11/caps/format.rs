//! Format gates from the published FL 11_0 table and measured downlevel masks.
use super::{Coverage, Error, FeatureLevel};
use crate::dxgi::{caps::present_format, formats::FORMATS};
use adv_gpu::{Caps, Format, FormatCaps};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormatClass {
    Resource,
    Video,
    Excluded,
}

const fn format_class(format: u32) -> Option<FormatClass> {
    match format {
        0..=65 | 67..=99 | 115 => Some(FormatClass::Resource),
        100..=114 => Some(FormatClass::Video),
        66 | 130..=132 | 189..=191 => Some(FormatClass::Excluded),
        _ => None,
    }
}

// Adding a DXGI format to the generated ABI requires an explicit capability class.
const _: () = {
    let mut index = 0;
    while index < FORMATS.len() {
        assert!(format_class(FORMATS[index].format).is_some());
        index += 1;
    }
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatSupport {
    pub support: u32,
    pub support2: u32,
    pub sample_counts: u32,
}

impl FormatSupport {
    fn allow(&mut self, bit: FormatOperation) {
        let (one, two) = bit.flags();
        self.support |= one;
        self.support2 |= two;
    }

    pub(super) fn contains(self, bit: FormatOperation) -> bool {
        let (one, two) = bit.flags();
        if bit.sample_count() != 0 {
            self.support & one == one && self.sample_counts & bit.sample_count() != 0
        } else {
            (one != 0 || two != 0) && self.support & one == one && self.support2 & two == two
        }
    }
}

/// The image backing selected by resource admission and the operations that can
/// execute with it. A converted or emulated operation can have a separate path.
#[derive(Clone, Copy, Debug)]
pub struct FormatPath<'a> {
    pub format: u32,
    pub backing: Option<Format>,
    pub native: &'a [FormatOperation],
    pub emulated: &'a [FormatOperation],
}

macro_rules! operations {
    ($($variant:ident $name:literal => ($mask1:expr, $mask2:expr), $path:literal);+ $(;)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum FormatOperation { $($variant),+ }

        impl FormatOperation {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const SPEC_TEST: &str = "GFX-CAP-5 / production_format_paths_are_derived_from_the_device";

            fn parse(name: &str) -> Self {
                match name { $($name => Self::$variant,)+ _ => panic!("unknown format capability {name}") }
            }

            fn flags(self) -> (u32, u32) {
                match self { $(Self::$variant => ($mask1, $mask2),)+ }
            }

            pub fn path(self) -> &'static str {
                match self { $(Self::$variant => $path,)+ }
            }
        }
    }
}

operations! {
    Format "FORMAT" => (0, 0), "format admission";
    Buffer "BUFFER" => (0x1, 0), "buffer resource layout";
    VertexBuffer "IA_VERTEX_BUFFER" => (0x2, 0), "vertex fetch conversion";
    IndexBuffer "IA_INDEX_BUFFER" => (0x4, 0), "index buffer binding";
    StreamBuffer "SO_BUFFER" => (0x8, 0), "stream output expansion";
    Texture1d "TEXTURE1D" => (0x10, 0), "height-one image backing and shader coordinates";
    Texture2d "TEXTURE2D" => (0x20, 0), "2D image backing";
    Texture3d "TEXTURE3D" => (0x40, 0), "volume image backing";
    TextureCube "TEXTURECUBE" => (0x80, 0), "cube image backing";
    ShaderLoad "SHADER_LOAD" => (0x100, 0), "shader texture load";
    ShaderSample "SHADER_SAMPLE" => (0x200, 0), "filtered texture sampling";
    ShaderComparison "SHADER_SAMPLE_COMPARISON" => (0x400, 0), "comparison texture sampling";
    Mip "MIP" => (0x1000, 0), "mipped image backing";
    MipAutogen "MIP_AUTOGEN" => (0x2000, 0), "mipmap generation";
    RenderTarget "RENDER_TARGET" => (0x4000, 0), "color attachment";
    Blend "BLENDABLE" => (0x8000, 0), "output-merger blend";
    DepthStencil "DEPTH_STENCIL" => (0x10000, 0), "depth/stencil attachment";
    CpuLock "CPU_LOCKABLE" => (0x20000, 0), "staging map and guest texel packing";
    MsaaResolve "MULTISAMPLE_RESOLVE" => (0x40000, 0), "multisample resolve";
    Display "DISPLAY" => (0x80000, 0), "present conversion";
    Cast "CAST_WITHIN_BIT_LAYOUT" => (0x100000, 0), "mutable view, depth aspect or typed BC decode";
    Msaa4 "MSAA_4X" => (0x200000, 0), "4x multisample attachments";
    Msaa8 "MSAA_8X" => (0x200000, 0), "8x multisample attachments";
    MsaaOther "MSAA_OTHER" => (0x200000, 0), "other multisample attachments";
    MsaaLoad "MULTISAMPLE_LOAD" => (0x400000, 0), "multisample shader load";
    Gather "SHADER_GATHER" => (0x800000, 0), "texture gather";
    BackBufferCast "BACK_BUFFER_CAST" => (0x1000000, 0), "swap-chain view cast";
    StorageView "TYPED_UNORDERED_ACCESS_VIEW" => (0x2000000, 0), "typed UAV view";
    GatherComparison "SHADER_GATHER_COMPARISON" => (0x4000000, 0), "comparison gather";
    Decoder "DECODER_OUTPUT" => (0x8000000, 0), "excluded video decoder";
    VideoOutput "VIDEO_PROCESSOR_OUTPUT" => (0x10000000, 0), "excluded video processor output";
    VideoInput "VIDEO_PROCESSOR_INPUT" => (0x20000000, 0), "excluded video processor input";
    AtomicAdd "UAV_ATOMIC_ADD" => (0, 0x1), "typed UAV atomic add";
    AtomicBitwise "UAV_ATOMIC_BITWISE_OPS" => (0, 0x2), "typed UAV atomic bitwise operations";
    AtomicCompare "UAV_ATOMIC_COMPARE_STORE_OR_COMPARE_EXCHANGE" => (0, 0x4), "typed UAV compare/exchange";
    AtomicExchange "UAV_ATOMIC_EXCHANGE" => (0, 0x8), "typed UAV atomic exchange";
    AtomicSigned "UAV_ATOMIC_SIGNED_MIN_OR_MAX" => (0, 0x10), "signed UAV atomic min/max";
    AtomicUnsigned "UAV_ATOMIC_UNSIGNED_MIN_OR_MAX" => (0, 0x20), "unsigned UAV atomic min/max";
    TypedLoad "UAV_TYPED_LOAD" => (0, 0x40), "typed UAV shader load";
    TypedStore "UAV_TYPED_STORE" => (0, 0x80), "typed UAV shader store";
    LogicOp "OUTPUT_MERGER_LOGIC_OP" => (0, 0x100), "excluded logic operations";
    Tiled "TILED" => (0, 0x200), "excluded tiled resources";
    Share "SHAREABLE" => (0, 0x400), "shared image alias";
    Raw "RAW_UAV_AND_SRV" => (0, 0), "byte-address buffer view";
    Structured "STRUCTURED_UAV_AND_SRV" => (0, 0), "structured buffer view";
}

use FormatOperation::*;

impl FormatOperation {
    fn sample_count(self) -> u32 {
        u32::from(self == Msaa4) * 4
            + u32::from(self == Msaa8) * 8
            + u32::from(self == MsaaOther) * 2
    }

    fn excluded(self) -> bool {
        matches!(self, Decoder | VideoOutput | VideoInput | LogicOp | Tiled)
    }

    fn for_compute(self) -> bool {
        matches!(
            self,
            StorageView
                | AtomicAdd
                | AtomicBitwise
                | AtomicCompare
                | AtomicExchange
                | AtomicSigned
                | AtomicUnsigned
                | TypedLoad
                | TypedStore
                | Raw
                | Structured
        )
    }

    fn hardware(self, caps: &Caps, format: u32, backing: Option<Format>) -> bool {
        let features = backing.map_or(FormatCaps::empty(), |f| caps.format(f));
        match self {
            Texture1d | Texture2d | Texture3d | TextureCube | Mip => !features.is_empty(),
            ShaderLoad | Gather => features.contains(FormatCaps::SAMPLED),
            ShaderSample | MipAutogen => features.contains(FormatCaps::FILTERED),
            ShaderComparison | GatherComparison => {
                let depth = match format {
                    41 => caps.format(Format::Depth32Float),
                    56 => caps.format(Format::Depth16Unorm),
                    _ => features,
                };
                depth.contains(FormatCaps::SAMPLED | FormatCaps::DEPTH_STENCIL)
            }
            RenderTarget => features.contains(FormatCaps::COLOR),
            Blend => features.contains(FormatCaps::BLEND),
            DepthStencil => features.contains(FormatCaps::DEPTH_STENCIL),
            Msaa4 | Msaa8 | MsaaOther => {
                features.intersects(FormatCaps::COLOR | FormatCaps::DEPTH_STENCIL)
            }
            MsaaResolve => {
                features.contains(FormatCaps::COLOR)
                    && backing.is_some_and(|host| caps.format_sample_counts(host) & !1 != 0)
            }
            MsaaLoad => {
                features.contains(FormatCaps::SAMPLED)
                    && backing.is_some_and(|host| caps.format_sample_counts(host) & !1 != 0)
            }
            Display => present_format(format)
                .is_some_and(|host| caps.format(host).contains(FormatCaps::COLOR)),
            StorageView | TypedStore => features.contains(FormatCaps::STORAGE),
            TypedLoad => {
                backing.is_some_and(|host| caps.format(host).contains(FormatCaps::STORAGE_READ))
            }
            AtomicAdd | AtomicBitwise | AtomicCompare | AtomicExchange | AtomicSigned
            | AtomicUnsigned => features.contains(FormatCaps::STORAGE_ATOMIC),
            Decoder | VideoOutput | VideoInput | LogicOp | Tiled => false,
            Format | Buffer | VertexBuffer | IndexBuffer | StreamBuffer | CpuLock | Cast
            | BackBufferCast | Share | Raw | Structured => true,
        }
    }
}

struct TableRow {
    format: u32,
    required: Vec<FormatOperation>,
    optional: Vec<FormatOperation>,
    downlevel: [FormatSupport; 5],
}

fn table() -> &'static [TableRow] {
    const DATA: &str = include_str!("../../../../../data/gfx/format-support-fl11_0.tsv");
    const DOWNLEVEL: &str = include_str!("../../../../../data/gfx/format-support-downlevel.tsv");
    static TABLE: OnceLock<Vec<TableRow>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut masks = BTreeMap::new();
        for row in DOWNLEVEL.lines().skip(2) {
            let mut columns = row.split('\t');
            let format = columns.next().unwrap().parse::<u32>().unwrap();
            let columns = std::array::from_fn(|_| {
                let mut masks = columns
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| u32::from_str_radix(s, 16).unwrap());
                FormatSupport {
                    support: masks.next().unwrap(),
                    support2: masks.next().unwrap(),
                    sample_counts: masks.next().unwrap(),
                }
            });
            masks.insert(format, columns);
        }
        DATA.lines()
            .skip(2)
            .map(|line| {
                let mut columns = line.split('\t');
                let format = columns.next().unwrap().parse::<u32>().unwrap();
                columns.next().unwrap();
                columns.next().unwrap();
                let parse = |column: &str| {
                    column
                        .split(',')
                        .filter(|&bit| bit != "-")
                        .map(FormatOperation::parse)
                        .collect::<Vec<_>>()
                };
                let required = parse(columns.next().unwrap());
                let mut optional = parse(columns.next().unwrap());
                optional.extend(parse(columns.next().unwrap()));
                TableRow {
                    format,
                    required,
                    optional,
                    downlevel: masks.remove(&format).unwrap_or_default(),
                }
            })
            .collect()
    })
}

#[cfg(feature = "gpu")]
pub(in crate::d3d11) struct ProductionFormat {
    format: u32,
    backing: Option<Format>,
    operations: Vec<FormatOperation>,
    emulated: Vec<FormatOperation>,
}

#[cfg(feature = "gpu")]
impl ProductionFormat {
    pub fn path(&self) -> FormatPath<'_> {
        FormatPath {
            format: self.format,
            backing: self.backing,
            native: &self.operations,
            emulated: &self.emulated,
        }
    }
}

#[cfg(feature = "gpu")]
pub(in crate::d3d11) fn production(caps: &Caps) -> Vec<ProductionFormat> {
    use crate::d3d11::resource;

    table()
        .iter()
        .filter(|row| format_class(row.format) == Some(FormatClass::Resource))
        .filter_map(|row| {
            let format = row.format;
            let backing = resource::backing_format(caps, format);
            if backing.is_none() && format != 0 {
                return None;
            }
            let packed = matches!(format, 85 | 86 | 115) && backing == Some(Format::Bgra8Unorm);
            let operations =
                row.required
                    .iter()
                    .chain(&row.optional)
                    .copied()
                    .filter(|&bit| match bit {
                        Format | CpuLock => true,
                        Buffer => format == 0 || resource::buffer_format(format).is_ok(),
                        VertexBuffer => true,
                        IndexBuffer => matches!(format, 42 | 57),
                        StreamBuffer => true,
                        Texture1d | Texture2d | Texture3d | TextureCube | Mip => backing.is_some(),
                        ShaderLoad | ShaderSample | ShaderComparison | Gather
                        | GatherComparison => true,
                        MipAutogen | RenderTarget | Blend | Msaa4 | Msaa8 | MsaaOther => !packed,
                        MsaaResolve => {
                            !packed
                                || backing.is_some_and(|backing| {
                                    resource::packed_resolve(caps, format, backing).is_some()
                                })
                        }
                        MsaaLoad => !packed,
                        DepthStencil => matches!(format, 20 | 40 | 45 | 55),
                        Cast => crate::dxgi::formats::format_layout(format)
                            .unwrap()
                            .cast_family
                            .iter()
                            .all(|&member| resource::backing_format(caps, member).is_some()),
                        Display | BackBufferCast => present_format(format).is_some(),
                        StorageView | TypedStore | TypedLoad => {
                            resource::storage_format(format).is_ok()
                        }
                        AtomicAdd | AtomicBitwise | AtomicSigned | AtomicUnsigned
                        | AtomicCompare | AtomicExchange => matches!(format, 41..=43),
                        Share => backing.is_some(),
                        Raw => format == 39,
                        Structured => format == 0,
                        Decoder | VideoOutput | VideoInput | LogicOp | Tiled => false,
                    })
                    .collect::<Vec<_>>();
            let emulated = row
                .required
                .iter()
                .copied()
                .filter(|bit| {
                    operations.contains(bit)
                        && match bit {
                            ShaderSample => resource::unfilterable_view(caps, format, false),
                            MipAutogen => backing
                                .is_some_and(|format| resource::image_filter_needed(caps, format)),
                            StorageView | TypedStore | TypedLoad => {
                                resource::storage_uses_buffer(caps, format, false)
                            }
                            AtomicAdd | AtomicBitwise | AtomicSigned | AtomicUnsigned
                            | AtomicCompare | AtomicExchange => {
                                resource::storage_uses_buffer(caps, format, true)
                            }
                            Format | Buffer | VertexBuffer | IndexBuffer | StreamBuffer
                            | Texture1d | Texture2d | Texture3d | TextureCube | ShaderLoad
                            | ShaderComparison | Mip | RenderTarget | Blend | DepthStencil
                            | CpuLock | MsaaResolve | Display | Cast | Msaa4 | Msaa8
                            | MsaaOther | MsaaLoad | Gather | BackBufferCast | GatherComparison
                            | Decoder | VideoOutput | VideoInput | LogicOp | Tiled | Share
                            | Raw | Structured => false,
                        }
                })
                .collect();
            Some(ProductionFormat {
                format,
                backing,
                operations,
                emulated,
            })
        })
        .collect()
}

fn resolve_path(
    caps: &Caps,
    available: Option<&FormatPath<'_>>,
    bit: FormatOperation,
    format: u32,
) -> bool {
    !bit.excluded()
        && (bit != Display || present_format(format).is_some())
        && available.is_some_and(|path| {
            path.emulated.contains(&bit)
                || (path.native.contains(&bit) && bit.hardware(caps, format, path.backing))
        })
}

pub(super) fn derive(
    caps: &Caps,
    paths: Coverage<'_>,
    level: FeatureLevel,
    compute: bool,
) -> Option<BTreeMap<u32, FormatSupport>> {
    let mut result = BTreeMap::new();
    for row in table() {
        let downlevel = (level != FeatureLevel::L11_0).then(|| row.downlevel[level.column()]);
        let path = paths.formats.iter().find(|path| path.format == row.format);
        let eligible = |bit: FormatOperation| {
            downlevel.is_none_or(|mask| {
                if matches!(bit, Format | Raw | Structured) {
                    mask.support != 0
                } else {
                    mask.contains(bit)
                }
            }) && (!bit.for_compute() || compute)
        };
        // Published video formats and downlevel video-only rows have no
        // applicable mandatory entries in this front.
        let relevant = row
            .required
            .iter()
            .chain(&row.optional)
            .any(|&bit| !bit.excluded() && !matches!(bit, Format | Share) && eligible(bit));
        let class = format_class(row.format).expect("published DXGI format");
        if !relevant || class == FormatClass::Excluded {
            continue;
        }
        let mut support = FormatSupport::default();
        for (&bit, required) in row
            .required
            .iter()
            .map(|bit| (bit, true))
            .chain(row.optional.iter().map(|bit| (bit, false)))
        {
            if bit.excluded() || !eligible(bit) {
                continue;
            }
            let samples = if path.is_some_and(|path| path.emulated.contains(&bit)) {
                caps.limits.sample_counts
            } else {
                path.and_then(|path| path.backing)
                    .map_or(0, |format| caps.format_sample_counts(format))
            };
            let count = bit.sample_count();
            let mask = samples & count;
            let available = resolve_path(caps, path, bit, row.format) && (count == 0 || mask != 0);
            if available {
                support.allow(bit);
                support.sample_counts |= if matches!(bit, MsaaLoad | MsaaResolve) {
                    samples & !1
                } else {
                    mask
                };
            } else if required
                && !matches!(bit, Share | Msaa8)
                && (bit != MsaaOther || level == FeatureLevel::L11_0)
                && class == FormatClass::Resource
                && !(matches!(bit, Display | BackBufferCast)
                    && present_format(row.format).is_none())
            {
                return None;
            }
        }
        if let Some(downlevel) = downlevel {
            support.sample_counts &= downlevel.sample_counts;
        }
        result.insert(row.format, support);
    }
    Some(result)
}

pub(super) fn checked_mask(mask: u32) -> Result<u32, Error> {
    if mask == 0 {
        Err(Error::FormatUnsupported)
    } else {
        Ok(mask)
    }
}

pub(super) fn multisample_query_format(format: u32, level: FeatureLevel) -> bool {
    matches!(
        format_class(format),
        Some(FormatClass::Resource | FormatClass::Video)
    ) && !(level <= FeatureLevel::L9_3 && matches!(format, 94..=99))
}

#[cfg(test)]
pub(super) fn fixture_rows() -> Vec<FormatPath<'static>> {
    table()
        .iter()
        .filter(|row| format_class(row.format) == Some(FormatClass::Resource))
        .map(|row| {
            // The future resource front supplies the complete backing map. The
            // fixture uses real native queries for representative color and depth,
            // and models coverage for the other resource paths.
            let backing = match row.format {
                28 => Some(adv_gpu::Format::Rgba8Unorm),
                45 => Some(adv_gpu::Format::Depth32FloatStencil8),
                _ => None,
            };
            let (native, emulated) = if backing.is_some() {
                (FormatOperation::ALL, &[][..])
            } else {
                (&[][..], FormatOperation::ALL)
            };
            FormatPath {
                format: row.format,
                backing,
                native,
                emulated,
            }
        })
        .collect()
}

#[cfg(all(test, feature = "gpu"))]
mod production_tests {
    use super::*;

    // spec: GFX-CAP-5
    #[test]
    fn production_format_paths_are_derived_from_the_device() {
        let gpu = adv_device::gpu::Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
        let caps = gpu.device().caps();
        let formats = production(caps);
        let paths: Vec<_> = formats.iter().map(ProductionFormat::path).collect();
        let available = |format, bit| {
            resolve_path(
                caps,
                paths.iter().find(|path| path.format == format),
                bit,
                format,
            )
        };
        for format in [41, 42, 43] {
            assert!(available(format, AtomicExchange), "{format} exchange");
            for bit in [
                AtomicAdd,
                AtomicBitwise,
                AtomicCompare,
                AtomicSigned,
                AtomicUnsigned,
            ] {
                assert_eq!(available(format, bit), format != 41, "{format} {bit:?}");
            }
        }
        assert!(available(65, TypedStore));
        for format in [5, 6, 7, 8, 68, 69, 70, 71, 72] {
            assert!(available(format, Texture2d), "{format} texture");
            assert!(available(format, CpuLock), "{format} map");
        }
        for format in 6..=8 {
            assert!(available(format, ShaderLoad), "{format} load");
            assert!(available(format, Buffer), "{format} typed buffer");
            assert!(available(format, Cast), "{format} cast");
            assert!(!available(format, TypedStore), "{format} UAV");
        }
        for format in [28, 65, 68, 71, 72] {
            assert!(available(format, ShaderSample), "{format} sample");
        }
        assert!(!available(103, VideoInput));
    }
}
