//! One instanced tile draw decodes a software source into a presentable buffer.
use crate::plan::Tiles;
use adv_gpu::encode::{Bind, Load};
use adv_gpu::pipeline::{ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology};
use adv_gpu::{Device, Format, Rect, RenderPass, RenderPipeline, Ring, Span};

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
            sample_mask: u32::MAX,
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
    pass: &mut RenderPass<'_>,
    pipeline: &RenderPipeline,
    ring: &mut Ring,
    software: &Source<'_>,
    tiles: &Tiles,
) -> adv_gpu::Result<Option<Span>> {
    if tiles.rects.is_empty() {
        return Ok(None);
    }
    let data = source(ring, software, tiles)?;
    pass.set_pipeline(pipeline);
    pass.set_viewport(
        0.,
        0.,
        software.extent.0 as f32,
        software.extent.1 as f32,
        0.,
        1.,
    );
    bind(pass, software, &data);
    pass.draw(0..6, 0..tiles.rects.len() as u32);
    Ok(Some(data))
}
/// Prepare the software bindings for Ingest or a composed Video draw. The
/// owner retains the returned span in the encoder after ending the pass.
pub fn source(ring: &mut Ring, software: &Source<'_>, tiles: &Tiles) -> adv_gpu::Result<Span> {
    let mut data = ring.reserve(source_bytes(tiles.rects.len()), 256)?;
    write_source(data.bytes(), software, tiles);
    Ok(data)
}

pub(crate) fn source_bytes(rects: usize) -> u64 {
    ((65 + rects) * 16) as u64
}

pub(crate) fn write_source(bytes: &mut [u8], software: &Source<'_>, tiles: &Tiles) {
    let (width, height) = software.extent;
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
    if matches!(
        software.format,
        PixelFormat::Indexed8 | PixelFormat::Indexed4 | PixelFormat::Indexed1
    ) {
        if let Some(palette) = software.palette {
            bytes[16..1040].copy_from_slice(palette.as_flattened());
        } else {
            bytes[16..1040].fill(0);
        }
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
}

/// Bind the software source in a stage's render pass. A Video shader includes
/// `SOFTWARE_WGSL` and calls `software_pixel` before composing its planes.
pub fn bind(pass: &mut RenderPass<'_>, software: &Source<'_>, data: &Span) {
    bind_slice(
        pass,
        software,
        data.buffer()
            .slice(data.offset()..data.offset() + data.len()),
    );
}

pub(crate) fn bind_slice(
    pass: &mut RenderPass<'_>,
    software: &Source<'_>,
    data: adv_gpu::resource::BufferSlice<'_>,
) {
    pass.bind(
        0,
        Bind::StorageBuffer(software.buffer.slice(0..software.buffer.desc().size)),
    );
    pass.bind(1, Bind::StorageBuffer(data));
}

fn write_words(bytes: &mut [u8], words: &[u32]) {
    for (out, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        out.copy_from_slice(&word.to_le_bytes());
    }
}
pub(crate) fn coverage(tiles: &Tiles) -> (Rect, Load<[f64; 4]>) {
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

/// One clustered pass of a sparse TBDR tile set.
pub(crate) struct Cluster {
    pub(crate) area: Rect,
    pub(crate) load: Load<[f64; 4]>,
    pub(crate) tiles: Tiles,
}

/// Split sparse distant rectangles into at most `max_clusters` tight render
/// areas for TBDR backends (Vulkan) when the single bounding box has <25% fill
/// and is large enough to justify extra passes. Returns `None` when a single
/// pass is better.
#[cfg_attr(target_vendor = "apple", allow(dead_code))]
pub(crate) fn coverage_clusters(tiles: &Tiles, max_clusters: usize) -> Option<Vec<Cluster>> {
    if max_clusters <= 1 || tiles.rects.len() < 2 {
        return None;
    }
    let (bbox, _) = coverage(tiles);
    let bbox_area = u64::from(bbox.w) * u64::from(bbox.h);
    let covered: u64 = tiles
        .rects
        .iter()
        .map(|r| u64::from(r.w) * u64::from(r.h))
        .sum();
    if bbox_area < 64 * 64 || covered * 4 >= bbox_area {
        return None;
    }
    // Start with one cluster per rect and greedily merge the pair whose union
    // adds the least wasted bounding-box area, stopping at `max_clusters` (or
    // earlier if merging adds zero waste).
    struct Group {
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        pixels: u64,
        rects: Vec<crate::Rect>,
    }
    impl Group {
        fn area(&self) -> u64 {
            u64::from(self.x1 - self.x0) * u64::from(self.y1 - self.y0)
        }
        fn merged_area(&self, other: &Self) -> u64 {
            let w = self.x1.max(other.x1) - self.x0.min(other.x0);
            let h = self.y1.max(other.y1) - self.y0.min(other.y0);
            u64::from(w) * u64::from(h)
        }
    }
    let mut groups: Vec<Group> = tiles
        .rects
        .iter()
        .map(|&r| Group {
            x0: r.x,
            y0: r.y,
            x1: r.x + r.w,
            y1: r.y + r.h,
            pixels: u64::from(r.w) * u64::from(r.h),
            rects: vec![r],
        })
        .collect();
    while groups.len() > max_clusters {
        let mut best = (0usize, 1usize, u64::MAX);
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let waste = groups[i]
                    .merged_area(&groups[j])
                    .saturating_sub(groups[i].area() + groups[j].area());
                if waste < best.2 {
                    best = (i, j, waste);
                }
            }
        }
        let other = groups.remove(best.1);
        let target = &mut groups[best.0];
        target.x0 = target.x0.min(other.x0);
        target.y0 = target.y0.min(other.y0);
        target.x1 = target.x1.max(other.x1);
        target.y1 = target.y1.max(other.y1);
        target.pixels += other.pixels;
        target.rects.extend(other.rects);
    }
    let split_area: u64 = groups.iter().map(Group::area).sum();
    if split_area * 2 >= bbox_area {
        return None;
    }
    Some(
        groups
            .into_iter()
            .map(|g| {
                let area = Rect {
                    x: g.x0,
                    y: g.y0,
                    w: g.x1 - g.x0,
                    h: g.y1 - g.y0,
                };
                let load = if g.pixels == u64::from(area.w) * u64::from(area.h) {
                    Load::DontCare
                } else {
                    Load::Load
                };
                Cluster {
                    area,
                    load,
                    tiles: Tiles {
                        size: tiles.size,
                        rects: g.rects,
                    },
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_distant_rects_cluster_with_dont_care() {
        let tiles = Tiles {
            size: (8, 8),
            rects: vec![
                crate::Rect {
                    x: 0,
                    y: 0,
                    w: 16,
                    h: 16,
                },
                crate::Rect {
                    x: 600,
                    y: 440,
                    w: 16,
                    h: 16,
                },
            ],
        };
        let clusters = coverage_clusters(&tiles, 4).expect("should split distant corners");
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|c| c.load == Load::DontCare));
        assert_eq!(
            clusters[0].area,
            Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 16
            }
        );
        assert_eq!(
            clusters[1].area,
            Rect {
                x: 600,
                y: 440,
                w: 16,
                h: 16
            }
        );
    }
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
