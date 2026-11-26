use super::*;
use crate::d3d11::{
    abi::*,
    caps::{FeatureLevel, Report, tests::fixture_report},
};

fn result_buffer(report: &Report, input: &[u32]) -> Result<Vec<u32>, Error> {
    let &[size, usage, bind, cpu, misc, stride, initialized] = input else {
        panic!("buffer request");
    };
    Buffer::new(
        D3D11_BUFFER_DESC {
            ByteWidth: size,
            Usage: usage as i32,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: misc,
            StructureByteStride: stride,
        },
        initialized != 0,
        report,
        1 << 28,
    )
    .map(|buffer| vec![buffer.desc().StructureByteStride])
}

fn result_texture(report: &Report, input: &[u32]) -> Result<Vec<u32>, Error> {
    let &[
        width,
        height,
        mips,
        layers,
        format,
        samples,
        quality,
        usage,
        bind,
        cpu,
        misc,
        initialized,
    ] = input
    else {
        panic!("texture request");
    };
    Texture::new_2d(
        D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: mips,
            ArraySize: layers,
            Format: format as i32,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: samples,
                Quality: quality,
            },
            Usage: usage as i32,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: misc,
        },
        initialized != 0,
        report,
    )
    .map(|texture| vec![texture.desc_2d().MipLevels])
}

fn result_view(report: &Report, input: &[u32], output: &[u32]) -> Result<Vec<u32>, Error> {
    let &[kind, format, dimension, a, b, c, d] = input else {
        panic!("view request");
    };
    let fields = [a, b, c, d];
    let texture = Texture::new_2d(
        D3D11_TEXTURE2D_DESC {
            Width: 16,
            Height: 16,
            MipLevels: 3,
            ArraySize: 2,
            Format: 28,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: Bind::SHADER | Bind::RENDER_TARGET | Bind::UNORDERED,
            ..Default::default()
        },
        false,
        report,
    )
    .unwrap();
    let (kind, desc) = match kind {
        0 => (
            ViewKind::ShaderResource,
            D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: format as i32,
                ViewDimension: dimension as i32,
                Anonymous: bytemuck::cast(fields),
            }
            .into(),
        ),
        1 => (
            ViewKind::RenderTarget,
            D3D11_RENDER_TARGET_VIEW_DESC {
                Format: format as i32,
                ViewDimension: dimension as i32,
                Anonymous: bytemuck::cast([a, b, c]),
            }
            .into(),
        ),
        2 => (
            ViewKind::UnorderedAccess,
            D3D11_UNORDERED_ACCESS_VIEW_DESC {
                Format: format as i32,
                ViewDimension: dimension as i32,
                Anonymous: bytemuck::cast([a, b, c]),
            }
            .into(),
        ),
        _ => panic!("view kind"),
    };
    View::new(
        kind,
        Resource::Texture(texture),
        (dimension != 0).then_some(desc),
        report,
    )
    .map(|view| {
        let mut words = match kind {
            ViewKind::ShaderResource => {
                bytemuck::cast_slice::<_, u32>(std::slice::from_ref(&view.srv_desc())).to_vec()
            }
            ViewKind::RenderTarget => {
                bytemuck::cast_slice::<_, u32>(std::slice::from_ref(&view.rtv_desc())).to_vec()
            }
            ViewKind::UnorderedAccess => {
                bytemuck::cast_slice::<_, u32>(std::slice::from_ref(&view.uav_desc())).to_vec()
            }
            ViewKind::DepthStencil => unreachable!(),
        };
        words.truncate(output.len());
        words
    })
}

// spec: GFX-CAP-4 GFX-CAP-5
#[test]
fn windows_descriptor_results() {
    let reports = [
        ("b000", fixture_report(FeatureLevel::L11_0)),
        ("a000", fixture_report(FeatureLevel::L10_0)),
        ("9100", fixture_report(FeatureLevel::L9_1)),
    ];
    for line in include_str!("windows.tsv").lines() {
        let mut row = line.split('\t');
        let (kind, name, levels, status) = (
            row.next().unwrap(),
            row.next().unwrap(),
            row.next().unwrap(),
            row.next().unwrap(),
        );
        let words = |field: &str| {
            field
                .split_whitespace()
                .map(|word| word.parse::<u32>().unwrap())
                .collect::<Vec<_>>()
        };
        let input = words(row.next().unwrap());
        let output = words(row.next().unwrap_or_default());
        let expected = match status {
            "00000000" => Ok(output.clone()),
            "80070057" => Err(Error::InvalidArgument),
            "80004005" => Err(Error::UnsupportedView),
            _ => panic!("unknown descriptor status {status}"),
        };
        for (level, report) in &reports {
            if !levels.split(',').any(|value| value == *level) {
                continue;
            }
            let actual = match kind {
                "buffer" => result_buffer(report, &input),
                "texture" => result_texture(report, &input),
                "view" => result_view(report, &input, &output),
                _ => panic!("unknown descriptor type {kind}"),
            };
            assert_eq!(actual, expected, "{name} at {level}");
        }
    }
}
