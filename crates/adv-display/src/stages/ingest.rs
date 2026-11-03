//! One instanced tile draw decodes a software source into a presentable buffer.
use crate::plan::Tiles;
use adv_gpu::encode::{Bind, ColorAttachment, Load, Store};
use adv_gpu::pipeline::{ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology};
use adv_gpu::{
    Device, Encoder, Format, ImageView, Rect, RenderPass, RenderPassDesc, RenderPipeline, Ring,
    Span,
};

/// Packed software encodings understood by the ingest shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra32,
    Bgr24,
    Rgb565,
    Rgb555,
    Indexed8,
    Indexed4,
    Indexed1,
}

/// Borrowed publication data. The producer owns its slot and completion state;
/// the caller reports its last submitted read through the producer's token API.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub buffer: &'a adv_gpu::Buffer,
    pub pitch: u32,
    pub extent: (u32, u32),
    pub format: PixelFormat,
    /// BGRA palette entries; absent indexed palettes decode to black.
    pub palette: Option<&'a [[u8; 4]; 256]>,
}

/// The decoder and its bindings can also be included in a video composition
/// shader, which calls `software_pixel` in the same fragment invocation.
pub const SOFTWARE_WGSL: &str = include_str!("software.wgsl");

pub fn pipeline(device: &Device, format: Format) -> adv_gpu::Result<RenderPipeline> {
    let shader = device.create_shader(adv_gpu::shader::ShaderDesc {
        source: adv_gpu::shader::Source::Precompiled {
            spirv: &include!(concat!(env!("OUT_DIR"), "/ingest.spv.rs")),
            metal: include_bytes!(concat!(env!("OUT_DIR"), "/ingest.msl")),
            entries: &include!(concat!(env!("OUT_DIR"), "/ingest.entries.rs")),
        },
        identity: include!(concat!(env!("OUT_DIR"), "/ingest.identity.rs")),
        label: "software ingest",
    })?;
    device.create_render_pipeline(
        &RenderPipelineDesc {
            vertex: shader.entry("vertex_main", adv_gpu::shader::Stage::Vertex)?,
            fragment: Some(shader.entry("fragment_main", adv_gpu::shader::Stage::Fragment)?),
            bindings: super::bindings::INGEST.to_vec(),
            primitive: Primitive {
                topology: Topology::Triangles,
                cull: Cull::None,
                front_ccw: true,
                wireframe: false,
                unclipped_depth: false,
            },
            colors: [ColorTarget {
                format,
                blend: None,
                write_mask: 15,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "software ingest",
        },
        None,
    )
}

/// Encodes Ingest into a presentable buffer's view. Video stages use [`source`]
/// and [`bind`] to decode inline in their composition draw. The submitting owner
/// reports the source slot's ingest token after its last read.
/// `ring` belongs to that owner; Busy leaves the encoder untouched.
pub fn encode(
    encoder: &mut Encoder,
    pipeline: &RenderPipeline,
    ring: &mut Ring,
    software: &Source<'_>,
    destination: &ImageView,
    tiles: &Tiles,
) -> adv_gpu::Result<()> {
    if tiles.rects.is_empty() {
        return Ok(());
    }
    let (width, height) = software.extent;
    debug_assert_eq!(
        (destination.size().width, destination.size().height),
        (width, height)
    );
    let data = source(ring, software, tiles)?;
    let (area, load) = coverage(tiles);
    let mut pass = encoder.render_pass(&RenderPassDesc {
        colors: [ColorAttachment {
            view: destination,
            load,
            store: Store::Store,
            resolve: None,
        }]
        .into_iter()
        .collect(),
        depth_stencil: None,
        area,
        timestamps: None,
        label: "software ingest",
    });
    pass.set_pipeline(pipeline);
    pass.set_viewport(0., 0., width as f32, height as f32, 0., 1.);
    bind(&mut pass, software, &data);
    pass.draw(0..6, 0..tiles.rects.len() as u32);
    drop(pass);
    encoder.retain(data);
    Ok(())
}
/// Prepare the software bindings for Ingest or a composed Video draw. The
/// owner retains the returned span in the encoder after ending the pass.
pub fn source(ring: &mut Ring, software: &Source<'_>, tiles: &Tiles) -> adv_gpu::Result<Span> {
    let (width, height) = software.extent;
    let mut data = ring.reserve(((65 + tiles.rects.len()) * 16) as u64, 256)?;
    let bytes = data.bytes();
    let kind = match software.format {
        PixelFormat::Bgra32 => 0,
        PixelFormat::Bgr24 => 1,
        PixelFormat::Rgb565 => 2,
        PixelFormat::Rgb555 => 3,
        PixelFormat::Indexed8 => 4,
        PixelFormat::Indexed4 => 5,
        PixelFormat::Indexed1 => 6,
    };
    write_words(&mut bytes[..16], &[width, height, software.pitch, kind]);
    if let Some(palette) = software.palette {
        bytes[16..1040].copy_from_slice(palette.as_flattened());
    } else {
        bytes[16..1040].fill(0);
    }
    for (bytes, rect) in bytes[1040..]
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .zip(&tiles.rects)
    {
        debug_assert!(rect.x + rect.w <= width && rect.y + rect.h <= height);
        write_words(bytes, &[rect.x, rect.y, rect.w, rect.h]);
    }
    Ok(data)
}

/// Bind the software source in a stage's render pass. A Video shader includes
/// `SOFTWARE_WGSL` and calls `software_pixel` before composing its planes.
pub fn bind(pass: &mut RenderPass<'_>, software: &Source<'_>, data: &Span) {
    pass.bind(
        0,
        Bind::StorageBuffer(software.buffer.slice(0..software.buffer.desc().size)),
    );
    pass.bind(
        1,
        Bind::StorageBuffer(
            data.buffer()
                .slice(data.offset()..data.offset() + data.len()),
        ),
    );
}

fn write_words(bytes: &mut [u8], words: &[u32]) {
    for (out, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        out.copy_from_slice(&word.to_le_bytes());
    }
}
pub(super) fn coverage(tiles: &Tiles) -> (Rect, Load<[f64; 4]>) {
    let first = tiles.rects[0];
    let (mut x, mut y, mut right, mut bottom, mut pixels) =
        (first.x, first.y, first.x + first.w, first.y + first.h, 0u64);
    for r in &tiles.rects {
        x = x.min(r.x);
        y = y.min(r.y);
        right = right.max(r.x + r.w);
        bottom = bottom.max(r.y + r.h);
        pixels += u64::from(r.w) * u64::from(r.h);
    }
    let area = Rect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    };
    // Planner tiles do not overlap. Pixels outside the render area are preserved
    // by the encoder, so only holes inside it require a load.
    let load = if pixels == u64::from(area.w) * u64::from(area.h) {
        Load::DontCare
    } else {
        Load::Load
    };
    (area, load)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loads_only_for_holes_inside_the_tile_area() {
        let mut tiles = Tiles {
            size: (1, 1),
            rects: vec![crate::Rect {
                x: 2,
                y: 3,
                w: 5,
                h: 7,
            }],
        };
        assert_eq!(
            coverage(&tiles),
            (
                Rect {
                    x: 2,
                    y: 3,
                    w: 5,
                    h: 7
                },
                Load::DontCare
            )
        );
        tiles.rects.push(crate::Rect {
            x: 9,
            y: 3,
            w: 2,
            h: 7,
        });
        assert_eq!(
            coverage(&tiles),
            (
                Rect {
                    x: 2,
                    y: 3,
                    w: 9,
                    h: 7
                },
                Load::Load
            )
        );
        tiles.rects.push(crate::Rect {
            x: 7,
            y: 3,
            w: 2,
            h: 7,
        });
        assert_eq!(coverage(&tiles).1, Load::DontCare);
    }
}
