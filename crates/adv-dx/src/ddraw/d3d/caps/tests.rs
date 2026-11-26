use super::*;
use adv_gpu::Limits;

const FIXED_FUNCTION: &[Path] = &[
    TransformedVertices,
    ExecuteBuffer,
    LegacyVertexBuffer,
    TextureStorage,
    RenderAfterFlip,
    DrawPrimitives2,
    Tnl,
    Upload,
    HardwareRaster,
    TexturePerspective,
    TextureAlpha,
    ColorKey,
    PowerOfTwoValidation,
    BorderColor,
    TextureRepeat,
    PaletteAlpha,
    ProjectedTexture,
    CubeTexture,
    Filter,
    AnisotropicFilter,
    Blend,
    CombinedBlend,
    DepthComparison,
    AlphaComparison,
    Dither,
    SoftwareRasterOps,
    SubpixelRaster,
    Fog,
    AntialiasedRaster,
    MipBias,
    DepthBias,
    Shading,
    TextureBlend,
    Address,
    Cull,
    Stencil,
    VertexLayout,
    TextureCombiner,
    ExtendedTextureCombiner,
    BumpMapping,
    TextureGeneration,
    MaterialSources,
    LightTypes,
    LocalViewer,
];

fn limits() -> Limits {
    Limits {
        max_image_2d: 16384,
        max_image_3d: 2048,
        max_image_layers: 2048,
        max_color_attachments: 8,
        max_uniform_buffers: 30,
        max_sampled_images: 128,
        max_samplers: 16,
        max_storage_bindings: 8,
        max_workgroup_storage: 32768,
        max_workgroup_invocations: 1024,
        uniform_offset_alignment: 256,
        storage_offset_alignment: 256,
        sample_counts: 7,
        tile: (0, 0),
        push_constant_bytes: 128,
        max_viewports: 16,
        max_buffer_size: 1 << 32,
        max_color_bytes_per_sample: 128,
        max_fragment_input_components: 128,
        max_framebuffer_layers: 2048,
        max_uniform_buffer_bytes: 65536,
    }
}

// spec: GFX-CAP-6
#[test]
fn descriptors_follow_the_fixed_function_paths() {
    let coverage = Coverage {
        paths: FIXED_FUNCTION,
        max_texture_extent: 16384,
        max_stages: 8,
        max_textures: 8,
        max_lights: u32::MAX,
        textures: &TextureFormat::ALL,
        depths: &DepthFormat::ALL,
        render_formats: &[Format::Bgra8Unorm, Format::B5g6r5Unorm],
        converted_textures: &[TextureFormat::V8U8, TextureFormat::X8L8V8U8],
        ..Default::default()
    };
    let formats = TextureFormat::ALL
        .into_iter()
        .filter_map(TextureFormat::backing)
        .chain(DepthFormat::ALL.into_iter().map(DepthFormat::backing))
        .map(|format| {
            let usage = if format.has_depth() {
                FormatCaps::DEPTH_STENCIL
            } else {
                FormatCaps::COLOR
            };
            (format, usage | FormatCaps::SAMPLED, 1)
        })
        .collect();
    let backing = Caps::from_reported_formats(limits(), Features::BC_TEXTURES, formats);
    for (device, devcaps, texcaps, raster, texop, lights, extent) in [
        (Device::Rgb, 0x551, 0xdf, 0x002121f7, 0x78ff, u32::MAX, 1024),
        (
            Device::Hal,
            0x021abef1,
            0xccd,
            0x003379b1,
            0x03feffff,
            u32::MAX,
            16384,
        ),
        (
            Device::TnlHal,
            0x021bbef1,
            0xccd,
            0x003379b1,
            0x03feffff,
            8,
            16384,
        ),
    ] {
        let report = Report::derive(&backing, coverage, device);
        let caps = report.device;
        assert_eq!(caps.dwDevCaps, devcaps);
        assert_eq!(caps.dpcTriCaps.dwTextureCaps, texcaps);
        assert_eq!(caps.dpcTriCaps.dwRasterCaps, raster);
        assert_eq!(caps.dwTextureOpCaps, texop);
        assert_eq!(caps.dwMaxActiveLights, lights);
        assert_eq!(caps.dwMaxTextureWidth, extent);
        assert_eq!(caps.wMaxSimultaneousTextures, 8);
        assert_eq!(caps.dwVertexProcessingCaps, 0x3f);
        assert_eq!(
            caps.deviceGUID,
            bytemuck::cast::<_, [u32; 4]>(device.guid())
        );
        assert_eq!(
            report.depth_formats.len(),
            if device == Device::Rgb { 2 } else { 6 }
        );
        assert!(
            report
                .texture_formats
                .iter()
                .any(|format| format.dwFourCC == u32::from_le_bytes(*b"DXT5"))
        );
        assert!(
            !report
                .texture_formats
                .iter()
                .any(|format| format.dwFourCC == u32::from_le_bytes(*b"NVCS"))
        );
        assert_eq!(
            (report.hardware.dwFlags, report.software.dwFlags),
            if device == Device::Rgb {
                (0, 0x7ff)
            } else {
                (0x1e3, 0x1f)
            }
        );
        assert_eq!(
            (report.hardware.dwDevCaps, report.software.dwDevCaps),
            if device == Device::Rgb {
                (0, 0x551)
            } else {
                (0x021bbef1, 1)
            }
        );
    }

    let narrowed = Coverage {
        paths: &[TransformedVertices, Filter],
        ..coverage
    };
    let no_formats = Caps::from_reported_formats(limits(), Features::empty(), Vec::new());
    let report = Report::derive(&no_formats, narrowed, Device::Hal);
    assert_eq!(report.device.dwDevCaps, 0x401);
    assert_eq!(report.device.dpcTriCaps.dwSrcBlendCaps, 0);
    assert_eq!(report.device.dwTextureOpCaps, 0);
    assert!(report.depth_formats.is_empty());
    assert_eq!(report.texture_formats.len(), 2);
}
