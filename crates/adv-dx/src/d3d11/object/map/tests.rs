use super::*;
use crate::d3d11::{
    abi::{D3D11_BUFFER_DESC, D3D11_TEXTURE2D_DESC, DXGI_SAMPLE_DESC},
    caps::{FeatureLevel, Report, tests::fixture_report},
    desc::{Bind, Buffer, Texture},
};

fn resource(name: &str, report: &Report) -> Result<Resource, crate::d3d11::desc::Error> {
    const READ: u32 = 0x20000;
    const WRITE: u32 = 0x10000;
    let (usage, bind, cpu) = match name {
        "dynamic-vb" => (Usage::Dynamic, Bind::VERTEX, WRITE),
        "dynamic-cb" => (Usage::Dynamic, Bind::CONSTANT, WRITE),
        "dynamic-srv" | "dynamic-tex" => (Usage::Dynamic, Bind::SHADER, WRITE),
        "staging-read" => (Usage::Staging, 0, READ),
        "staging-write" => (Usage::Staging, 0, WRITE),
        "staging-both" => (Usage::Staging, 0, READ | WRITE),
        "default-vb" => (Usage::Default, Bind::VERTEX, 0),
        _ => panic!("Map fixture resource"),
    };
    if matches!(name, "dynamic-tex" | "staging-both") {
        Texture::new_2d(
            D3D11_TEXTURE2D_DESC {
                Width: 16,
                Height: 16,
                MipLevels: 1,
                ArraySize: 1,
                Format: 28,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: usage as i32,
                BindFlags: bind,
                CPUAccessFlags: cpu,
                ..Default::default()
            },
            false,
            report,
        )
        .map(Resource::Texture)
    } else {
        Buffer::new(
            D3D11_BUFFER_DESC {
                ByteWidth: 64,
                Usage: usage as i32,
                BindFlags: bind,
                CPUAccessFlags: cpu,
                ..Default::default()
            },
            false,
            report,
            1 << 28,
        )
        .map(Resource::Buffer)
    }
}

// spec: GFX-REC-1 GFX-CAP-4
#[test]
fn windows_map_admission() {
    let reports = [
        ("b000", fixture_report(FeatureLevel::L11_0)),
        ("a000", fixture_report(FeatureLevel::L10_0)),
        ("9100", fixture_report(FeatureLevel::L9_1)),
    ];
    for line in include_str!("windows.tsv").lines() {
        let [name, levels, mask] = line.split('\t').collect::<Vec<_>>()[..] else {
            panic!("Map fixture");
        };
        for (level, report) in &reports {
            if !levels.split(',').any(|value| value == *level) {
                continue;
            }
            let candidate = resource(name, report);
            if mask == "-" {
                assert!(candidate.is_err());
                continue;
            }
            let resource = candidate.unwrap();
            let expected = u16::from_str_radix(mask, 16).unwrap();
            for mode in 1..=5 {
                for (index, flags) in [0, 0x100000, 1].into_iter().enumerate() {
                    let outcome = Map::admit(resource, 0, mode, flags);
                    let bit = (mode as usize - 1) * 3 + index;
                    assert_eq!(
                        outcome.is_ok(),
                        expected & (1 << bit) != 0,
                        "{name} at {level}: mode {mode}, flags {flags:#x}"
                    );
                    if let Ok(map) = outcome {
                        let expected = if flags == 0 {
                            Ok(())
                        } else {
                            Err(Error::StillDrawing)
                        };
                        assert_eq!(map.check_busy(true), expected);
                        assert_eq!(map.check_busy(false), Ok(()));
                    }
                }
            }
        }
    }
}

// spec: GFX-REC-1 GFX-CAP-4
#[test]
fn dynamic_maps_share_one_guest_shadow_and_staging_transfers_only_stale_data() {
    use adv_device::surface::Access as SurfaceAccess;
    use std::num::NonZeroU32;
    let report = fixture_report(FeatureLevel::L11_0);
    let alignment = NonZeroU32::new(128).unwrap();
    let mut dynamic = Layouts::new(resource("dynamic-vb", &report).unwrap(), alignment)
        .unwrap()
        .attach(0x20000);
    for mode in [4, 4, 5, 4] {
        let result = dynamic
            .map(0, mode, 0, true, |_, _, _| unreachable!())
            .unwrap();
        assert_eq!(
            (result.pData, result.RowPitch, result.DepthPitch),
            (0x20000, 64, 64)
        );
    }

    let layouts = Layouts::new(resource("staging-both", &report).unwrap(), alignment).unwrap();
    assert_eq!(layouts.size(), 128 * 16);
    let mut staging = layouts.attach(0x30000);
    staging.attach_staging_gpu(0);
    let mut uploaded = Vec::new();
    staging
        .staging_gpu_access(0, SurfaceAccess::Write, |layout, slice, parts| {
            assert_eq!(layout.slice(0x30000, slice), 0x30000);
            uploaded.extend_from_slice(parts);
            Ok(())
        })
        .unwrap();
    assert_eq!(uploaded.len(), 1);
    let nonblocking = staging.map(0, 1, 0x100000, true, |_, _, _| unreachable!());
    assert_eq!(nonblocking.unwrap_err(), Error::StillDrawing);
    let mut downloaded = Vec::new();
    let result = staging
        .map(0, 1, 0, false, |layout, slice, parts| {
            assert_eq!(layout.row_pitch(), 128);
            assert_eq!(slice, 0);
            downloaded.extend_from_slice(parts);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        (result.pData, result.RowPitch, result.DepthPitch),
        (0x30000, 128, 2048)
    );
    assert_eq!(downloaded, uploaded);
    staging
        .map(0, 1, 0x100000, false, |_, _, _| unreachable!())
        .unwrap();
}
