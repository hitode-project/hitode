use super::*;
use adv_abi::types::DXGI_SAMPLE_DESC;
use adv_gpu::{Features, Format, FormatCaps, Limits};

fn caps() -> Caps {
    let formats = [
        Format::Rgba8Unorm,
        Format::Rgba8Srgb,
        Format::Bgra8Unorm,
        Format::Rgba16Float,
        Format::Rgb10a2Unorm,
    ];
    Caps::from_reported_formats(
        Limits {
            max_image_2d: 16384,
            max_image_3d: 2048,
            max_image_layers: 2048,
            max_color_attachments: 8,
            max_color_bytes_per_sample: 128,
            max_fragment_input_components: 128,
            max_framebuffer_layers: 2048,
            max_uniform_buffer_bytes: 65536,
            max_viewports: 16,
            max_buffer_size: 1 << 30,
            max_uniform_buffers: 14,
            max_sampled_images: 128,
            max_samplers: 16,
            max_storage_bindings: 8,
            max_workgroup_storage: 32768,
            max_workgroup_invocations: 1024,
            uniform_offset_alignment: 256,
            storage_offset_alignment: 16,
            sample_counts: 5,
            tile: (0, 0),
            push_constant_bytes: 0,
        },
        Features::empty(),
        formats
            .into_iter()
            .map(|format| (format, FormatCaps::COLOR, 5))
            .collect(),
    )
}

const COVERAGE: SwapCoverage = SwapCoverage {
    bitblt: true,
    flip: true,
    srgb: true,
    multisample: true,
    flags: 0,
};

fn desc(effect: i32, buffers: u32, format: i32) -> DXGI_SWAP_CHAIN_DESC1 {
    DXGI_SWAP_CHAIN_DESC1 {
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: 32,
        BufferCount: buffers,
        SwapEffect: effect,
        ..Default::default()
    }
}

// spec: GFX-DXGI-2
#[test]
fn swap_descriptors_and_window_status_follow_the_windows_rows() {
    let caps = caps();
    let window = Window {
        client: [304, 201],
        minimized: false,
    };
    for line in include_str!("windows.tsv").lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let reported = u32::from_str_radix(fields.last().unwrap(), 16).unwrap();
        let effect = fields[1].parse().unwrap();
        let result = match fields.as_slice() {
            ["create", _, count, format, _] => Description::new(
                desc(effect, count.parse().unwrap(), format.parse().unwrap()),
                window,
                &caps,
                COVERAGE,
            )
            .map(|_| ()),
            ["buffer", _, _] => Description::new(desc(effect, 2, 28), window, &caps, COVERAGE)
                .and_then(|chain| chain.buffer(1))
                .map(|_| ()),
            ["present", _, state, _] => {
                let chain = Description::new(desc(effect, 2, 28), window, &caps, COVERAGE).unwrap();
                let current = match *state {
                    "minimized" => Window {
                        minimized: true,
                        ..window
                    },
                    "zero-client" => Window {
                        client: [0, 0],
                        ..window
                    },
                    _ => window,
                };
                assert_eq!(chain.present_status(current), reported, "{line}");
                continue;
            }
            _ => panic!("swap oracle {line}"),
        };
        assert_eq!(
            result.map_or_else(Error::hresult, |_| 0),
            reported,
            "{line}"
        );
    }
}

// spec: GFX-DXGI-2
#[test]
fn resize_revalidates_description_after_reference_check() {
    let window = Window {
        client: [304, 201],
        minimized: false,
    };
    let caps = caps();
    let mut desc = desc(0, 1, 28);
    desc.SampleDesc.Count = 4;
    let chain = Description::new(desc, window, &caps, COVERAGE).unwrap();
    assert_eq!((chain.desc().Width, chain.desc().Height), (304, 201));

    let resize = Resize {
        buffers: 0,
        size: [800, 600],
        format: 0,
        flags: 0,
        referenced: true,
    };
    assert!(matches!(
        chain.resize(resize, window, &caps, COVERAGE),
        Err(Error::InvalidCall)
    ));
    let resize = Resize {
        buffers: 0,
        size: [640, 480],
        format: 0,
        flags: 0,
        referenced: false,
    };
    let resized = chain
        .resize(resize, window, &caps, COVERAGE)
        .unwrap()
        .desc();
    assert_eq!(
        (
            resized.Width,
            resized.Height,
            resized.BufferCount,
            resized.Format
        ),
        (640, 480, 1, 28)
    );
}
