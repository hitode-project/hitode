//! Capability answers and ordered COM enumerations.
use super::{Face, INVALID, caps, object::Object};
use adv_abi::{Env, Outcome};
use adv_cpu::backend::{Prot, Reg};
use adv_device::surface::SurfaceStore;

use caps::{DepthFormat, Device, Path, Report, TextureFormat};

const DEVICES: [Device; 3] = [Device::Rgb, Device::Hal, Device::TnlHal];
const PATHS: &[Path] = &[
    Path::TransformedVertices,
    Path::LegacyVertexBuffer,
    Path::TextureStorage,
    Path::RenderAfterFlip,
    Path::DrawPrimitives2,
    Path::Tnl,
    Path::Upload,
    Path::HardwareRaster,
    Path::TexturePerspective,
    Path::TextureAlpha,
    Path::ColorKey,
    Path::BorderColor,
    Path::TextureRepeat,
    Path::ProjectedTexture,
    Path::Filter,
    Path::Blend,
    Path::CombinedBlend,
    Path::DepthComparison,
    Path::AlphaComparison,
    Path::Dither,
    Path::SubpixelRaster,
    Path::Fog,
    Path::MipBias,
    Path::DepthBias,
    Path::Shading,
    Path::TextureBlend,
    Path::Address,
    Path::Cull,
    Path::Stencil,
    Path::VertexLayout,
    Path::TextureCombiner,
    Path::ExtendedTextureCombiner,
    Path::TextureGeneration,
    Path::MaterialSources,
    Path::LightTypes,
    Path::LocalViewer,
];

pub(super) fn derive(store: &SurfaceStore, device: Device) -> Report {
    Report::derive(
        store.device().caps(),
        caps::Coverage {
            paths: PATHS,
            max_texture_extent: 16384,
            max_stages: 8,
            max_textures: 8,
            max_lights: 8,
            max_texture_repeat: 8192,
            max_vertices: 65535,
            clip_planes: 6,
            vertex_blend_matrices: u16::from(device == Device::TnlHal) * 4,
            render_formats: &[adv_gpu::Format::Bgra8Unorm, adv_gpu::Format::B5g6r5Unorm],
            converted_textures: &[
                TextureFormat::Xrgb1555,
                TextureFormat::Argb1555,
                TextureFormat::Argb4444,
                TextureFormat::Rgb565,
                TextureFormat::Xrgb8888,
                TextureFormat::Argb8888,
                TextureFormat::L8,
                TextureFormat::A8,
                TextureFormat::A8L8,
            ],
            converted_depths: &[
                DepthFormat::Depth16,
                DepthFormat::Depth32High,
                DepthFormat::LowStencil,
                DepthFormat::HighStencil,
                DepthFormat::Depth32Low,
                DepthFormat::Depth24,
            ],
            ..Default::default()
        },
        device,
    )
}

pub(super) fn enumerate(env: &mut Env<'_>, face: Face, method: &str, args: &[u32]) -> Outcome {
    let state = env.ext.get::<super::super::front::State>();
    let objects = &env.sys.objects;
    let store = env.ext.get::<SurfaceStore>();
    let Some(com) = state.draw.com else {
        return Outcome::Return(INVALID);
    };
    let (callback, context, rows) = if method == "EnumTextureFormats" {
        let Some(Object::Device(device)) = com.get::<Object>(objects, args[0]) else {
            return Outcome::Return(INVALID);
        };
        (
            args[1],
            args[2],
            derive(store, device.kind)
                .texture_formats
                .into_iter()
                .map(Row::single)
                .collect(),
        )
    } else {
        if !com
            .get::<super::super::ComState>(objects, args[0])
            .is_some_and(|object| matches!(object.kind, super::super::Kind::Device))
        {
            return Outcome::Return(INVALID);
        }
        if method == "EnumZBufferFormats" {
            let mut guid = [0; 16];
            if env.cpu.read(args[1], &mut guid).is_err() {
                return Outcome::Return(INVALID);
            }
            let Some(device) = DEVICES.into_iter().find(|d| d.guid() == guid) else {
                return Outcome::Return(INVALID);
            };
            (
                args[2],
                args[3],
                derive(store, device)
                    .depth_formats
                    .into_iter()
                    .map(Row::single)
                    .collect(),
            )
        } else {
            let modern = face == Face::Direct3D7;
            let rows = DEVICES
                .into_iter()
                .take(if modern { 3 } else { 2 })
                .map(|device| {
                    let mut row = Row::default();
                    let (name, description) = device.name();
                    let report = derive(store, device);
                    if !modern {
                        row.push(&device.guid());
                    }
                    row.push(&[description.as_bytes(), &[0]].concat());
                    row.push(&[name.as_bytes(), &[0]].concat());
                    if modern {
                        row.push(bytemuck::bytes_of(&report.device));
                    } else {
                        row.push(bytemuck::bytes_of(&report.hardware));
                        row.push(bytemuck::bytes_of(&report.software));
                    }
                    row
                })
                .collect();
            (args[1], args[2], rows)
        }
    };
    if callback == 0 || !env.cpu.accessible(callback, 1, Prot::EXEC) {
        return Outcome::Return(INVALID);
    }
    let rows: Vec<Row> = rows;
    let capacity = rows.iter().map(|r| r.bytes.len()).max().unwrap_or(0) as u32;
    let Some(scratch) = env.cpu.reg(Reg::Esp).checked_sub(capacity).map(|p| p & !3) else {
        return Outcome::Return(INVALID);
    };
    next(env, rows.into_iter(), callback, context, scratch)
}

#[derive(Default)]
struct Row {
    bytes: Vec<u8>,
    offsets: Vec<u32>,
}
impl Row {
    fn single(record: impl bytemuck::Pod) -> Self {
        let mut row = Self::default();
        row.push(bytemuck::bytes_of(&record));
        row
    }
    fn push(&mut self, bytes: &[u8]) {
        self.offsets.push(self.bytes.len() as u32);
        self.bytes.extend_from_slice(bytes);
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
    }
}

fn next(
    env: &mut Env<'_>,
    mut rows: std::vec::IntoIter<Row>,
    callback: u32,
    context: u32,
    scratch: u32,
) -> Outcome {
    let Some(row) = rows.next() else {
        return Outcome::Return(0);
    };
    if env.cpu.write(scratch, &row.bytes).is_err() {
        return Outcome::Return(INVALID);
    }
    let mut args = row
        .offsets
        .into_iter()
        .map(|offset| scratch + offset)
        .collect::<Vec<_>>();
    args.push(context);
    Outcome::call_on_stack(callback, args, scratch, move |env, returned| {
        if returned.value() == 0 {
            Outcome::Return(0)
        } else {
            next(env, rows, callback, context, scratch)
        }
    })
}
