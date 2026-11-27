use super::*;
use adv_gpu::{Format, FormatCaps, Limits};

#[derive(Clone)]
struct Fixture {
    limits: Limits,
    features: Features,
    rgba: FormatCaps,
    rgba_samples: u32,
    storage_read: bool,
}

impl Fixture {
    fn caps(&self) -> Caps {
        let color =
            FormatCaps::COLOR | FormatCaps::BLEND | FormatCaps::SAMPLED | FormatCaps::FILTERED;
        let storage_read = if self.storage_read {
            FormatCaps::STORAGE_READ
        } else {
            FormatCaps::empty()
        };
        Caps::from_reported_formats(
            self.limits,
            self.features,
            vec![
                (
                    Format::Rgba8Unorm,
                    self.rgba | storage_read,
                    self.rgba_samples,
                ),
                (Format::Bgra8Unorm, color, self.limits.sample_counts),
                (Format::Rgba8Srgb, color, self.limits.sample_counts),
                (
                    Format::Rgba16Float,
                    color | FormatCaps::STORAGE,
                    self.limits.sample_counts,
                ),
                (Format::Rgb10a2Unorm, color, self.limits.sample_counts),
                (
                    Format::Depth32FloatStencil8,
                    FormatCaps::DEPTH_STENCIL | FormatCaps::SAMPLED,
                    self.limits.sample_counts,
                ),
            ],
        )
    }
    fn m1() -> Self {
        Self {
            limits: Limits {
                max_image_2d: 16384,
                max_image_3d: 2048,
                max_image_layers: 2048,
                max_color_attachments: 8,
                max_uniform_buffers: 30,
                max_sampled_images: 128,
                max_samplers: 16,
                max_storage_bindings: 30,
                max_workgroup_storage: 32768,
                max_workgroup_invocations: 1024,
                uniform_offset_alignment: 256,
                storage_offset_alignment: 16,
                sample_counts: 7,
                tile: (0, 0),
                push_constant_bytes: 0,
                max_viewports: 16,
                max_buffer_size: 1 << 32,
                max_color_bytes_per_sample: 128,
                max_fragment_input_components: 124,
                max_framebuffer_layers: 2048,
                max_uniform_buffer_bytes: 65536,
            },
            features: Features::INDIRECT
                | Features::MULTISAMPLE
                | Features::BC_TEXTURES
                | Features::OCCLUSION_QUERIES
                | Features::TIMESTAMPS
                | Features::DUAL_SOURCE_BLEND
                | Features::DEPTH_CLAMP
                | Features::FULL_SAMPLE_COVERAGE
                | Features::STANDARD_SAMPLE_LOCATIONS,
            rgba: FormatCaps::COLOR
                | FormatCaps::BLEND
                | FormatCaps::SAMPLED
                | FormatCaps::FILTERED
                | FormatCaps::STORAGE,
            rgba_samples: 7,
            storage_read: true,
        }
    }

    fn native_8x() -> Self {
        let mut result = Self::m1();
        result.limits.sample_counts |= 8;
        result.rgba_samples |= 8;
        result
    }

    fn a12() -> Self {
        let mut result = Self::m1();
        result.limits.max_sampled_images = 96;
        result.limits.max_color_bytes_per_sample = 64;
        result.limits.max_buffer_size = 1 << 28;
        result.features.remove(Features::BC_TEXTURES);
        result
    }

    fn adreno_610() -> Self {
        // Report 7037, the oldest Adreno 610 driver in vulkan-floor.tsv.
        let mut result = Self::m1();
        result.limits.max_sampled_images = 256;
        result.limits.max_storage_bindings = 72;
        result.limits.max_uniform_buffers = 96;
        result.limits.max_workgroup_storage = 16384;
        result.limits.push_constant_bytes = 128;
        result.limits.max_fragment_input_components = 112;
        result.limits.max_viewports = 1;
        result
            .features
            .remove(Features::BC_TEXTURES | Features::FULL_SAMPLE_COVERAGE);
        result
    }

    fn mali_r48() -> Self {
        // Report 51870 (Mali-G57 MC2, r48p0).
        let mut result = Self::m1();
        result.limits.sample_counts = 1 | 4 | 8;
        result.rgba_samples = result.limits.sample_counts;
        result.limits.max_image_layers = 4096;
        result.limits.max_sampled_images = 500_000;
        result.limits.max_storage_bindings = 500_000;
        result.limits.max_uniform_buffers = 500_000;
        result.limits.max_workgroup_invocations = 512;
        result.limits.max_viewports = 1;
        result.limits.max_framebuffer_layers = 256;
        result.limits.max_fragment_input_components = 128;
        result
            .features
            .remove(Features::BC_TEXTURES | Features::FULL_SAMPLE_COVERAGE);
        result
    }

    fn mali_r21() -> Self {
        // Report 14237 (Mali-G57, r21p0).
        let mut result = Self::mali_r48();
        result.limits.max_image_2d = 8192;
        result.limits.max_image_layers = 256;
        result.limits.max_sampled_images = 16;
        result.limits.max_storage_bindings = 35;
        result.limits.max_uniform_buffers = 12;
        result.features.remove(Features::DUAL_SOURCE_BLEND);
        result
    }
}

fn requirements() -> Vec<(Requirement, u32)> {
    Requirement::ALL
        .iter()
        .map(|&requirement| {
            let maximum = FeatureLevel::ALL
                .into_iter()
                .map(|level| level.limit(requirement))
                .max()
                .unwrap();
            (requirement, maximum)
        })
        .collect()
}

const EMULATED: &[Requirement] = &[
    Requirement::PsInputRegisters,
    Requirement::Viewports,
    Requirement::Scissors,
    Requirement::FramebufferLayers,
    Requirement::Bc1ThroughBc3,
    Requirement::Bc4Bc5,
    Requirement::Bc6hBc7,
    Requirement::R32UavAtomics,
    Requirement::QueryTimestampAndDisjoint,
];

const FEATURES: &[FeaturePath] = &[
    FeaturePath::ConcurrentCreates,
    FeaturePath::CommandLists,
    FeaturePath::Discard,
    FeaturePath::UpdateCopyFlags,
    FeaturePath::ClearView,
    FeaturePath::OverlappingCopy,
    FeaturePath::PartialConstantUpdate,
    FeaturePath::ConstantOffsets,
    FeaturePath::DynamicConstantNoOverwrite,
    FeaturePath::DynamicSrvNoOverwrite,
    FeaturePath::Sad4,
    FeaturePath::FullNonPowerOfTwo,
    FeaturePath::ShadowComparison,
    FeaturePath::ClearDepthView,
    FeaturePath::SimpleInstancing,
    FeaturePath::MixedCubeDepthView,
];

// spec: GFX-CAP-4
#[test]
fn mandatory_rows_follow_device_and_coverage() {
    let requirements = requirements();
    let formats = format::fixture_rows();
    let paths = Coverage {
        requirements: &requirements,
        emulated: EMULATED,
        formats: &formats,
        features: FEATURES,
    };
    for (name, fixture, level, compute, quality_8x) in [
        ("Apple7", Fixture::m1(), FeatureLevel::L11_0, true, 0),
        ("Apple8–9", Fixture::m1(), FeatureLevel::L11_0, true, 0),
        (
            "Apple10+",
            Fixture {
                features: Fixture::m1().features - Features::FULL_SAMPLE_COVERAGE,
                ..Fixture::native_8x()
            },
            FeatureLevel::L11_0,
            true,
            0,
        ),
        (
            "Native 8x",
            Fixture::native_8x(),
            FeatureLevel::L11_0,
            true,
            1,
        ),
        ("A12", Fixture::a12(), FeatureLevel::L9_3, false, 0),
        (
            "Adreno 610",
            Fixture::adreno_610(),
            FeatureLevel::L10_1,
            true,
            0,
        ),
        (
            "Mali-G57 r48",
            Fixture::mali_r48(),
            FeatureLevel::L10_1,
            false,
            0,
        ),
        (
            "Mali-G57 r21",
            Fixture::mali_r21(),
            FeatureLevel::L9_3,
            false,
            0,
        ),
    ] {
        let report =
            Report::derive(&fixture.caps(), paths).unwrap_or_else(|| panic!("{name}: no level"));
        assert_eq!(
            (report.level, report.compute_4x),
            (level, compute),
            "{name}"
        );
        assert_eq!(
            report.check_feature(4, 4, 0).unwrap().words(),
            &[u32::from(compute)]
        );
        assert_eq!(report.multisample_quality(28, 8), Ok(quality_8x), "{name}");
        for count in [1, 2, 4] {
            let quality = u32::from(fixture.rgba_samples & count != 0);
            assert_eq!(report.multisample_quality(28, count), Ok(quality), "{name}");
        }
    }

    let without_fallbacks = Coverage {
        emulated: &[],
        ..paths
    };
    assert!(Report::derive(&Fixture::mali_r48().caps(), without_fallbacks).is_none());
    assert_eq!(
        Report::derive(
            &Fixture::adreno_610().caps(),
            Coverage {
                emulated: &[Requirement::Bc1ThroughBc3],
                ..paths
            }
        )
        .unwrap()
        .level,
        FeatureLevel::L9_3
    );
    for level in FeatureLevel::ALL {
        assert_eq!(level::requirements(level).len(), Requirement::ALL.len());
    }
    assert!(Report::derive(&Fixture::m1().caps(), Coverage::default()).is_none());
}

// spec: GFX-CAP-4
#[test]
fn feature_layouts_masks_and_creation_status() {
    let requirements = requirements();
    let formats = format::fixture_rows();
    let paths = Coverage {
        requirements: &requirements,
        emulated: EMULATED,
        formats: &formats,
        features: FEATURES,
    };
    let high = Report::derive(&Fixture::native_8x().caps(), paths).unwrap();
    let low = Report::derive_at_most(&Fixture::m1().caps(), paths, FeatureLevel::L9_3).unwrap();
    for feature in Feature::ALL {
        let path = feature.path();
        let probe_format = 28;
        let size = feature.size();
        assert_eq!(
            high.check_feature(feature as u32, size, probe_format)
                .unwrap()
                .words()
                .len()
                * 4,
            size,
            "{path}"
        );
        assert_eq!(
            high.check_feature(feature as u32, size - 4, probe_format),
            Err(Error::InvalidArgument),
            "{path}"
        );
    }
    assert_eq!(low.check_feature(5, 56, 0).unwrap().words()[9], 0);
    assert_eq!(low.check_feature(5, 56, 0).unwrap().words()[11], 0);
    assert_eq!(high.check_feature(5, 56, 0).unwrap().words()[9], 1);
    assert_eq!(high.check_feature(5, 56, 0).unwrap().words()[11], 1);
    assert_eq!(high.check_feature(6, 4, 0).unwrap().words(), &[1]);
    assert_eq!(high.check_feature(14, 36, 0).unwrap().words()[7], 1);
    assert_eq!(high.check_feature(21, 4, 0), Err(Error::InvalidArgument));
    let counter = high.counter_info();
    assert_eq!(
        (
            counter.LastDeviceDependentCounter,
            counter.NumSimultaneousCounters,
            counter.NumDetectableParallelUnits
        ),
        (0, 0, 1)
    );

    let request = DeviceRequest {
        driver: Driver::Hardware,
        has_adapter: false,
        has_software_module: false,
        levels: None,
        flags: 0,
        sdk_version: 7,
        level_count: 0,
        wants_device: false,
        wants_context: false,
    };
    assert_eq!(
        request.choose(Some(high.level)),
        Ok(DeviceChoice {
            level: FeatureLevel::L11_0,
            status: 1,
            adapter: DeviceAdapter::Hardware
        })
    );
    for (levels, flags, error) in [
        (Some(&[][..]), 0, Error::Unsupported),
        (Some(&[0xb100][..]), 0, Error::Unsupported),
        (Some(&[0xb001][..]), 0, Error::InvalidArgument),
        (None, 2, Error::SdkComponentMissing),
        (None, 0x400, Error::SdkComponentMissing),
    ] {
        let request = DeviceRequest {
            levels,
            flags,
            ..request
        };
        assert_eq!(request.choose(Some(high.level)), Err(error));
    }
}

// spec: GFX-CAP-5
#[test]
fn formats_follow_the_backing_and_the_operation() {
    let requirements = requirements();
    let formats = format::fixture_rows();
    let paths = Coverage {
        requirements: &requirements,
        emulated: EMULATED,
        formats: &formats,
        features: FEATURES,
    };
    let fixture = Fixture::native_8x();
    let report = Report::derive(&fixture.caps(), paths).unwrap();
    assert_ne!(report.check_format(28).unwrap() & 0xc000, 0);
    assert_eq!(report.check_format2(28).unwrap() & 0xc0, 0xc0);
    assert_eq!(report.check_format(103), Err(Error::FormatUnsupported));
    assert_eq!(report.check_format2(103), Err(Error::FormatUnsupported));
    assert_eq!(report.multisample_quality(28, 1), Ok(1));
    assert_eq!(report.multisample_quality(28, 4), Ok(1));
    assert!(report.supports_multisample_quality(28, 4, 0xffffffff));
    assert!(!report.supports_multisample_quality(28, 4, 1));
    assert_eq!(report.multisample_quality(28, 32), Ok(0));
    assert_eq!(report.multisample_quality(103, 1), Ok(1));
    assert_eq!(report.multisample_quality(103, 4), Ok(0));
    assert_eq!(
        report.multisample_quality(116, 1),
        Err(Error::InvalidArgument)
    );

    assert_eq!(report.multisample_quality(28, 8), Ok(1));
    assert!(report.supports_multisample_quality(28, 8, 0xffffffff));
    let with_8x = Fixture {
        rgba_samples: 15,
        ..Fixture::m1()
    };
    let report = Report::derive(&with_8x.caps(), paths).unwrap();
    assert_eq!(report.multisample_quality(28, 8), Ok(1));
    assert_eq!(report.multisample_quality(45, 8), Ok(0));
    assert_eq!(report.level, FeatureLevel::L11_0);
    for missing in [
        Features::STANDARD_SAMPLE_LOCATIONS,
        Features::FULL_SAMPLE_COVERAGE,
    ] {
        let limited = Fixture {
            features: with_8x.features - missing,
            ..Fixture::native_8x()
        };
        let report = Report::derive(&limited.caps(), paths).unwrap();
        assert_eq!(report.multisample_quality(28, 8), Ok(0));
        assert_eq!(report.level, FeatureLevel::L11_0);
    }
    let no_typed_load = Fixture {
        storage_read: false,
        ..Fixture::native_8x()
    };
    let report = Report::derive(&no_typed_load.caps(), paths).unwrap();
    assert_eq!(report.level, FeatureLevel::L11_0);
    assert_eq!(report.check_format2(28).unwrap() & 0xc0, 0x80);
    let no_uav = Fixture {
        rgba: fixture.rgba - FormatCaps::STORAGE,
        ..Fixture::native_8x()
    };
    let report = Report::derive(&no_uav.caps(), paths).unwrap();
    assert!(report.level < FeatureLevel::L11_0);
    assert_eq!(report.check_format(98), Err(Error::FormatUnsupported));
}

// spec: GFX-CAP-4 GFX-CAP-5
#[test]
fn sample_counts_only_affect_queries_and_texture_creation() {
    use crate::d3d11::{abi::*, desc};

    let paths = Coverage {
        requirements: &requirements(),
        emulated: EMULATED,
        formats: &format::fixture_rows(),
        features: FEATURES,
    };
    for samples in [1, 1 | 2, 1 | 4, 1 | 2 | 4] {
        let mut fixture = Fixture::m1();
        fixture.limits.sample_counts = samples;
        fixture.rgba_samples = samples;
        let report = Report::derive(&fixture.caps(), paths).unwrap();
        assert_eq!(report.level, FeatureLevel::L11_0);

        for count in [1, 2, 4, 8] {
            let supported = samples & count != 0;
            for format in [28, 85] {
                assert_eq!(
                    report.multisample_quality(format, count),
                    Ok(u32::from(supported))
                );
                let create = desc::Texture::new_2d(
                    D3D11_TEXTURE2D_DESC {
                        Width: 1,
                        Height: 1,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: format as i32,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: count,
                            Quality: 0,
                        },
                        BindFlags: desc::Bind::RENDER_TARGET,
                        ..Default::default()
                    },
                    false,
                    &report,
                );
                assert_eq!(
                    create.err(),
                    (!supported).then_some(desc::Error::InvalidArgument)
                );
            }
        }
    }
}

// spec: GFX-DXGI-2
#[test]
fn swap_models_use_present_paths_and_renderable_formats() {
    use crate::dxgi::caps::{SwapCoverage, SwapEffect};
    let caps = Fixture::m1().caps();
    let paths = SwapCoverage {
        bitblt: true,
        flip: true,
        srgb: true,
        multisample: true,
        flags: 0,
    };
    let discard = SwapEffect::Discard.model();
    let flip = SwapEffect::FlipDiscard.model();
    assert!(discard.supports(&caps, paths, 28, 1, 4, 0));
    assert!(discard.supports(&caps, paths, 29, 1, 1, 0));
    assert!(!discard.supports(&caps, paths, 88, 1, 1, 0));
    assert!(flip.supports(&caps, paths, 24, 2, 1, 0));
    assert!(!flip.supports(&caps, paths, 24, 4, 1, 0));
    assert!(!flip.supports(&caps, paths, 24, 1, 1, 0));
    assert!(!flip.supports(&caps, paths, 29, 2, 1, 0));
    assert!(!flip.supports(&caps, paths, 28, 2, 4, 0));
    assert!(!flip.supports(&caps, paths, 28, 2, 1, 1));
    assert!(!flip.supports(&caps, SwapCoverage::default(), 28, 2, 1, 0));
    assert!(!discard.sequential_buffers && discard.occluded_status);
    let sequential = SwapEffect::FlipSequential.model();
    assert!(sequential.sequential_buffers && !sequential.occluded_status);
}

pub(in crate::d3d11) fn fixture_report(maximum: FeatureLevel) -> Report {
    let paths = Coverage {
        requirements: &requirements(),
        emulated: EMULATED,
        formats: &format::fixture_rows(),
        features: FEATURES,
    };
    let report = Report::derive_at_most(&Fixture::native_8x().caps(), paths, maximum).unwrap();
    assert_eq!(report.level, maximum);
    report
}

// spec: GFX-CAP-4
#[test]
fn public_creation_request_matches_windows() {
    let report = fixture_report(FeatureLevel::L11_0);
    for line in include_str!("creation.tsv").lines() {
        let mut fields = line.split('\t');
        let name = fields.next().unwrap();
        let status = u32::from_str_radix(fields.next().unwrap(), 16).unwrap();
        let sdk_version = fields.next().unwrap().parse().unwrap();
        let level = u32::from_str_radix(fields.next().unwrap(), 16).unwrap();
        let wants_device = fields.next().unwrap() != "unchanged";
        let wants_context = fields.next().unwrap() != "unchanged";
        let levels: Option<&[u32]> = match name {
            "array-order" => Some(&[0x9100, 0xb000]),
            "invalid-after-valid" => Some(&[0xb000, 0xb080]),
            "invalid-before-valid" => Some(&[0xb080, 0xc000]),
            "empty-levels" => Some(&[]),
            "sdk-zero"
            | "sdk-minus"
            | "sdk-plus"
            | "null-levels-with-count"
            | "none-out"
            | "context-out"
            | "debug" => None,
            _ => panic!("device creation case {name}"),
        };
        let level_count = levels.map_or_else(
            || {
                if name == "null-levels-with-count" {
                    4
                } else {
                    0
                }
            },
            |list| list.len() as u32,
        );
        let request = DeviceRequest {
            driver: Driver::Hardware,
            has_adapter: false,
            has_software_module: false,
            levels,
            flags: u32::from(name == "debug") * 2,
            sdk_version,
            level_count,
            wants_device,
            wants_context,
        };
        let actual = request
            .choose(Some(report.level))
            .map(|choice| (choice.status, choice.level as u32))
            .unwrap_or_else(|error| (error.hresult(), 0));
        assert_eq!(actual, (status, level), "{name}");
    }
}
