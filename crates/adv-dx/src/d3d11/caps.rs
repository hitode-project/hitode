//! D3D11 capability queries over the GPU and admission/execution coverage.
#![deny(clippy::todo, clippy::unimplemented, clippy::wildcard_enum_match_arm)]

mod feature;
mod format;
mod level;

pub use feature::{Feature, FeatureAnswer, FeaturePath};
pub use format::{FormatOperation, FormatPath, FormatSupport};
pub use level::{FeatureLevel, Requirement};

use adv_gpu::{Caps, Features};
use std::collections::BTreeMap;

/// Executing paths of a particular front. Numeric maxima describe the
/// validation and translation limits of the path. A missing path has limit zero.
#[derive(Clone, Copy, Default)]
pub struct Coverage<'a> {
    pub requirements: &'a [(Requirement, u32)],
    /// The numerically listed hardware requirement can be met by this other
    /// executing path, such as viewport bins or BC decode on upload.
    pub emulated: &'a [Requirement],
    pub features: &'a [FeaturePath],
    pub formats: &'a [FormatPath<'a>],
}

impl Coverage<'_> {
    fn limit(self, requirement: Requirement) -> u32 {
        self.requirements
            .iter()
            .find_map(|&(path, maximum)| (path == requirement).then_some(maximum))
            .unwrap_or(0)
    }

    fn feature(self, path: FeaturePath) -> u32 {
        u32::from(self.features.contains(&path))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    Unsupported,
    SdkComponentMissing,
    FormatUnsupported,
}

impl Error {
    pub fn hresult(self) -> u32 {
        match self {
            Self::InvalidArgument => 0x80070057,
            Self::Unsupported => 0x887a0004,
            Self::SdkComponentMissing => 0x887a002d,
            Self::FormatUnsupported => 0x80004005,
        }
    }
}

pub struct Report {
    pub level: FeatureLevel,
    pub compute_4x: bool,
    formats: BTreeMap<u32, FormatSupport>,
    features: [FeatureAnswer; Feature::COUNT],
    pub standard_sample_locations: bool,
}

impl Report {
    pub fn derive(caps: &Caps, paths: Coverage<'_>) -> Option<Self> {
        Self::derive_at_most(caps, paths, FeatureLevel::L11_0)
    }

    pub fn derive_at_most(caps: &Caps, paths: Coverage<'_>, maximum: FeatureLevel) -> Option<Self> {
        for level in FeatureLevel::ALL
            .into_iter()
            .rev()
            .filter(|&level| level <= maximum)
        {
            let requirements = level::requirements(level);
            if requirements.iter().any(|&(requirement, limit)| {
                !requirement.optional(level) && !requirement.available(caps, paths, limit)
            }) {
                continue;
            }
            let compute_4x = level >= FeatureLevel::L10_0
                && [
                    Requirement::ComputeProfile,
                    Requirement::ComputeThreads,
                    Requirement::ComputeSharedBytes,
                    Requirement::ComputeUavSlots,
                ]
                .into_iter()
                .all(|requirement| {
                    let limit = requirements
                        .iter()
                        .find(|&&(entry, _)| entry == requirement)
                        .unwrap()
                        .1;
                    requirement.available(caps, paths, limit)
                });
            if let Some(formats) = format::derive(caps, paths, level, compute_4x) {
                let features = feature::derive(caps, paths, level, compute_4x, &formats);
                return Some(Self {
                    level,
                    compute_4x,
                    formats,
                    features,
                    standard_sample_locations: caps
                        .features
                        .contains(Features::STANDARD_SAMPLE_LOCATIONS),
                });
            }
        }
        None
    }

    pub fn check_feature(
        &self,
        feature: u32,
        size: usize,
        format: u32,
    ) -> Result<FeatureAnswer, Error> {
        let feature = Feature::parse(feature)?;
        let expected = feature.size();
        if size != expected && !(feature == Feature::Options2 && size == expected + 4) {
            return Err(Error::InvalidArgument);
        }
        match feature {
            Feature::FormatSupport => Ok(FeatureAnswer::new([format, self.check_format(format)?])),
            Feature::FormatSupport2 => {
                Ok(FeatureAnswer::new([format, self.check_format2(format)?]))
            }
            Feature::Threading
            | Feature::Doubles
            | Feature::Compute4x
            | Feature::Options
            | Feature::Architecture
            | Feature::D3d9Options
            | Feature::MinPrecision
            | Feature::D3d9Shadow
            | Feature::Options1
            | Feature::D3d9Instancing
            | Feature::Markers
            | Feature::D3d9Options1
            | Feature::Options2
            | Feature::Options3
            | Feature::VirtualAddress
            | Feature::Options4
            | Feature::ShaderCache
            | Feature::Options5
            | Feature::Displayable => Ok(self.features[feature as usize]),
        }
    }

    pub(super) fn supports(&self, format: u32, operation: FormatOperation) -> bool {
        self.formats
            .get(&format)
            .is_some_and(|row| row.contains(operation))
    }

    /// E_FAIL leaves a zero format-support mask.
    pub fn check_format(&self, format: u32) -> Result<u32, Error> {
        format::checked_mask(self.formats.get(&format).map_or(0, |row| row.support))
    }

    /// E_FAIL leaves a zero FORMAT_SUPPORT2 mask.
    pub fn check_format2(&self, format: u32) -> Result<u32, Error> {
        format::checked_mask(self.formats.get(&format).map_or(0, |row| row.support2))
    }

    /// An invalid format leaves the caller's output unwritten.
    pub fn multisample_quality(&self, format: u32, count: u32) -> Result<u32, Error> {
        if !format::multisample_query_format(format, self.level) {
            return Err(Error::InvalidArgument);
        }
        Ok(match count {
            1 => 1,
            2 | 4 | 8 => u32::from(
                self.formats
                    .get(&format)
                    .is_some_and(|row| row.sample_counts & count != 0),
            ),
            _ => 0,
        })
    }

    pub fn supports_multisample_quality(&self, format: u32, count: u32, quality: u32) -> bool {
        self.multisample_quality(format, count).is_ok_and(|levels| {
            quality < levels
                || (count > 1
                    && levels != 0
                    && self.standard_sample_locations
                    && matches!(quality, 0xfffffffe | 0xffffffff))
        })
    }

    /// Device-dependent counters are outside the executor's query coverage.
    pub fn counter_info(&self) -> adv_abi::types::D3D11_COUNTER_INFO {
        adv_abi::types::D3D11_COUNTER_INFO {
            LastDeviceDependentCounter: 0,
            NumSimultaneousCounters: 0,
            NumDetectableParallelUnits: 1,
            ..Default::default()
        }
    }

    pub fn check_counter(&self) -> Result<(), Error> {
        Err(Error::InvalidArgument)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driver {
    Unknown,
    Hardware,
    Reference,
    Null,
    Software,
    Warp,
}

pub struct DeviceRequest<'a> {
    pub driver: Driver,
    pub has_adapter: bool,
    pub has_software_module: bool,
    pub levels: Option<&'a [u32]>,
    pub flags: u32,
    pub sdk_version: u32,
    pub level_count: u32,
    pub wants_device: bool,
    pub wants_context: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceChoice {
    pub level: FeatureLevel,
    pub status: u32,
    pub adapter: DeviceAdapter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceAdapter {
    Explicit,
    Hardware,
    Software,
    Null,
}

impl DeviceRequest<'_> {
    /// GFX-CAP-4: the first supported requested level and the Windows argument
    /// and debug-layer errors. The maximum comes from capability derivation.
    pub fn choose(&self, maximum: Option<FeatureLevel>) -> Result<DeviceChoice, Error> {
        if self.sdk_version != super::abi::limit("D3D11_SDK_VERSION")
            || (self.levels.is_none() && self.level_count != 0)
            || self.has_adapter != (self.driver == Driver::Unknown)
            || (self.driver == Driver::Software && !self.has_software_module)
        {
            return Err(Error::InvalidArgument);
        }
        let d3d10 = self.flags & 0xc000_0000 != 0;
        let allowed = if d3d10 { 0xc000_09ff } else { 0x9ef };
        if self.flags & !allowed != 0 || self.flags & 0x42 != 0 {
            return Err(Error::SdkComponentMissing);
        }
        let adapter = match self.driver {
            Driver::Unknown => DeviceAdapter::Explicit,
            Driver::Hardware => DeviceAdapter::Hardware,
            Driver::Reference | Driver::Warp => DeviceAdapter::Software,
            Driver::Null => DeviceAdapter::Null,
            Driver::Software => return Err(Error::Unsupported),
        };
        let levels = self.levels.unwrap_or(&FeatureLevel::DEFAULT);
        let parsed = levels
            .iter()
            .map(|&level| FeatureLevel::requested(level))
            .collect::<Result<Vec<_>, _>>()?;
        let level = parsed
            .into_iter()
            .flatten()
            .find(|level| maximum.is_some_and(|max| *level <= max))
            .ok_or(Error::Unsupported)?;
        Ok(DeviceChoice {
            level,
            status: u32::from(!self.wants_device && !self.wants_context),
            adapter,
        })
    }
    pub(super) fn choose_core(&self, maximum: Option<FeatureLevel>) -> Result<DeviceChoice, Error> {
        if self.flags & 0x402 != 0 {
            return Err(Error::SdkComponentMissing);
        }
        let adapter = match self.driver {
            Driver::Unknown => DeviceAdapter::Explicit,
            Driver::Hardware if self.has_adapter => DeviceAdapter::Explicit,
            Driver::Hardware => DeviceAdapter::Hardware,
            Driver::Reference | Driver::Warp => DeviceAdapter::Software,
            Driver::Null => DeviceAdapter::Null,
            Driver::Software => return Err(Error::Unsupported),
        };
        let level = self
            .levels
            .unwrap_or_default()
            .iter()
            .find_map(|&requested| {
                FeatureLevel::ALL.into_iter().find(|&level| {
                    level as u32 == requested && maximum.is_some_and(|max| level <= max)
                })
            })
            .ok_or(Error::Unsupported)?;
        Ok(DeviceChoice {
            level,
            status: u32::from(!self.wants_device),
            adapter,
        })
    }
}

#[cfg(test)]
pub(super) mod tests;
