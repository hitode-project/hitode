use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::*;

fn cases() -> impl Iterator<
    Item = (
        Domain,
        Partitioning,
        usize,
        &'static str,
        std::str::Split<'static, char>,
    ),
> {
    include_str!("fixture.tsv")
        .lines()
        .filter(|row| !row.starts_with('#'))
        .map(parse_case)
}

fn parse_case(
    row: &'static str,
) -> (
    Domain,
    Partitioning,
    usize,
    &'static str,
    std::str::Split<'static, char>,
) {
    let mut fields = row.split('\t');
    let domain = fields.next().unwrap();
    let partitioning = match fields.next().unwrap() {
        "integer" => Partitioning::Integer,
        "pow2" => Partitioning::Pow2,
        "fractional_odd" => Partitioning::FractionalOdd,
        "fractional_even" => Partitioning::FractionalEven,
        name => panic!("unknown partitioning {name}"),
    };
    let [a, b, c, d, e, f]: [f32; 6] = fields
        .next()
        .unwrap()
        .split(',')
        .map(|x| x.parse().unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let (domain, arity) = match domain {
        "tri" => (
            Domain::Tri {
                edges: [a, b, c],
                inside: d,
                output: SurfaceOutput::TriangleCw,
            },
            3,
        ),
        "quad" => (
            Domain::Quad {
                edges: [a, b, c, d],
                inside: [e, f],
                output: SurfaceOutput::TriangleCw,
            },
            3,
        ),
        "isoline" => (
            Domain::Isoline {
                density: a,
                detail: b,
                output: IsolineOutput::Line,
            },
            2,
        ),
        name => panic!("unknown domain {name}"),
    };
    (domain, partitioning, arity, row, fields)
}

#[test]
// spec: GFX-TESS-1
fn matches_d3d11_stream_output() {
    let mut count = 0;
    for (domain, partitioning, arity, row, mut fields) in cases() {
        let points: usize = fields.next().unwrap().parse().unwrap();
        let primitives: usize = fields.next().unwrap().parse().unwrap();
        let digest = fields.next().unwrap();
        let mesh = tessellate(domain, partitioning);
        assert_eq!(mesh.indices.len(), primitives * arity, "{row}");
        let used_indices = mesh.indices.iter().collect::<BTreeSet<_>>();
        assert_eq!(used_indices.len(), mesh.points.len(), "{row}");
        let point_bits: Vec<_> = mesh
            .points
            .iter()
            .map(|point| point.map(f32::to_bits))
            .collect();
        assert_eq!(
            point_bits.iter().collect::<BTreeSet<_>>().len(),
            points,
            "{row}"
        );

        let mut actual: Vec<_> = mesh
            .indices
            .chunks_exact(arity)
            .map(|indices| {
                let at = |i: usize| point_bits[indices[i] as usize];
                let [a, b, c] = [at(0), at(1), if arity == 3 { at(2) } else { [0; 3] }];
                if arity == 3 {
                    // Canonicalize the starting vertex while preserving winding.
                    [a, b, c].min([b, c, a]).min([c, a, b])
                } else {
                    [a, b, c]
                }
            })
            .collect();
        actual.sort_unstable();
        let vertices = actual.iter().flat_map(|primitive| &primitive[..arity]);
        let mut hash = Sha256::new();
        for vertex in vertices.clone() {
            for bits in vertex {
                hash.update(bits.to_le_bytes());
            }
        }
        assert_eq!(format!("{:x}", hash.finalize()), digest, "{row}");

        if let Some(literal) = fields.next() {
            let expected: Vec<[u32; 3]> = literal
                .split(',')
                .map(|location| {
                    let mut bits = location.split('.');
                    std::array::from_fn(|_| u32::from_str_radix(bits.next().unwrap(), 16).unwrap())
                })
                .collect();
            assert_eq!(vertices.copied().collect::<Vec<_>>(), expected, "{row}");
        }
        count += 1;
    }
    assert_eq!(count, 252);
}

#[test]
// spec: GFX-TESS-1
fn point_output_and_counterclockwise_winding() {
    let partitioning = Partitioning::FractionalOdd;
    let tri = |output| Domain::Tri {
        edges: [1.0, 4.0, 7.0],
        inside: 3.3,
        output,
    };
    let quad = |output| Domain::Quad {
        edges: [1.0, 4.0, 7.0, 3.3],
        inside: [2.5, 6.5],
        output,
    };
    for domain in [tri as fn(SurfaceOutput) -> Domain, quad] {
        let cw = tessellate(domain(SurfaceOutput::TriangleCw), partitioning);
        let ccw = tessellate(domain(SurfaceOutput::TriangleCcw), partitioning);
        let points = tessellate(domain(SurfaceOutput::Point), partitioning);
        assert_eq!(cw.points, ccw.points);
        assert_eq!(cw.points, points.points);
        assert_eq!(cw.indices.len(), ccw.indices.len());
        let (clockwise, _) = cw.indices.as_chunks::<3>();
        let (counterclockwise, _) = ccw.indices.as_chunks::<3>();
        for (cw, ccw) in clockwise.iter().zip(counterclockwise) {
            assert_eq!(ccw, &[cw[0], cw[2], cw[1]]);
        }
        assert!(points.indices.into_iter().eq(0..points.points.len() as u32));
    }
    let points = tessellate(
        Domain::Isoline {
            density: 2.0,
            detail: 2.0,
            output: IsolineOutput::Point,
        },
        Partitioning::Integer,
    );
    assert_eq!(points.indices, [0, 1, 2, 3, 4, 5]);
    assert_eq!(
        points.points,
        [
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.5, 0.0],
            [0.5, 0.5, 0.0],
            [1.0, 0.5, 0.0],
        ]
    );
}

#[test]
// spec: GFX-TESS-1
fn nan_edges_cull_and_nan_inside_uses_lower_bound() {
    let partitioning = Partitioning::FractionalOdd;
    let tri = tessellate(
        Domain::Tri {
            edges: [1.0, 1.0, f32::NAN],
            inside: 1.0,
            output: SurfaceOutput::TriangleCw,
        },
        partitioning,
    );
    let isoline = tessellate(
        Domain::Isoline {
            density: 1.0,
            detail: f32::NAN,
            output: IsolineOutput::Line,
        },
        partitioning,
    );
    for mesh in [tri, isoline] {
        assert!(mesh.points.is_empty());
        assert!(mesh.indices.is_empty());
    }

    let quad = |inside| Domain::Quad {
        edges: [1.0; 4],
        inside: [inside, 3.0],
        output: SurfaceOutput::TriangleCw,
    };
    let clamped = tessellate(quad(f32::NAN), partitioning);
    let lower_bound = tessellate(quad(1.0 + EPSILON), partitioning);
    assert_eq!(clamped.points, lower_bound.points);
    assert_eq!(clamped.indices, lower_bound.indices);
}

#[cfg(feature = "gpu")]
#[test]
// spec: GFX-TESS-1
fn compute_matches_reference_points_and_indices_in_all_captured_cases() {
    use adv_device::gpu::{
        BufferRange, DispatchArguments, Gpu, ModernShader, ModernTransfer, SlotBindings,
        StorageBinding,
    };
    use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
    use std::sync::Arc;

    let mut gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    let cases: Vec<_> = cases().collect();
    let mut parameters = Vec::new();
    let mut points = Vec::new();
    let mut indices = Vec::new();
    let mut ranges = Vec::new();

    let surface_output = |output| match output {
        SurfaceOutput::Point => 0,
        SurfaceOutput::TriangleCw => 2,
        SurfaceOutput::TriangleCcw => 3,
    };
    for (patch, &(domain, partitioning, _, _, _)) in cases.iter().enumerate() {
        let mesh = tessellate(domain, partitioning);
        let (edges, inside, domain, output) = match domain {
            Domain::Tri {
                edges: [a, b, c],
                inside,
                output,
            } => ([a, b, c, 0.], [inside, 0.], 0, surface_output(output)),
            Domain::Quad {
                edges,
                inside,
                output,
            } => (edges, inside, 1, surface_output(output)),
            Domain::Isoline {
                density,
                detail,
                output,
            } => ([density, detail, 0., 0.], [0.; 2], 2, output as u32),
        };
        let partitioning = match partitioning {
            Partitioning::Integer | Partitioning::Pow2 => 0,
            Partitioning::FractionalOdd => 1,
            Partitioning::FractionalEven => 2,
        };
        parameters.extend(edges.map(f32::to_bits));
        parameters.extend(inside.map(f32::to_bits));
        parameters.extend([domain, partitioning, output, 0, 0, 0]);
        let first_point = points.len() as u32 / 4;
        ranges.extend([
            mesh.points.len() as u32,
            mesh.indices.len() as u32,
            first_point,
            indices.len() as u32,
        ]);
        points.extend(mesh.points.into_iter().flat_map(|point| {
            [
                point[0].to_bits(),
                point[1].to_bits(),
                point[2].to_bits(),
                patch as u32,
            ]
        }));
        indices.extend(mesh.indices.into_iter().map(|index| index + first_point));
    }

    let size = |words: &[u32]| words.len() as u64 * 4;
    let storage = |size, usage, memory| {
        let buffer = gpu
            .device()
            .create_buffer(&BufferDesc {
                size,
                usage,
                memory,
                label: "tessellation oracle",
            })
            .unwrap();
        BufferRange {
            buffer,
            range: 0..size,
        }
    };
    let input = storage(size(&parameters), BufferUsage::STORAGE, Memory::Upload);
    // The input has not been submitted.
    unsafe { &mut *input.buffer.mapped().unwrap().as_ptr() }
        .copy_from_slice(bytemuck::cast_slice(&parameters));
    let expected = [ranges, points, indices];
    let outputs = expected.each_ref().map(|words| {
        storage(
            size(words),
            BufferUsage::STORAGE | BufferUsage::COPY_SRC,
            Memory::Device,
        )
    });
    let readbacks = expected
        .each_ref()
        .map(|words| storage(size(words), BufferUsage::COPY_DST, Memory::Readback));
    let buffers = [
        input,
        outputs[0].clone(),
        outputs[1].clone(),
        outputs[2].clone(),
    ];
    let groups = (cases.len() as u32).div_ceil(64);

    for (entry, groups) in [
        ("tess_count", groups),
        ("tess_scan", 1),
        ("tess_emit", groups),
    ] {
        let source = concat!(
            include_str!("compute.wgsl"),
            "\nfn tess_active() -> u32 { return arrayLength(&ranges); }"
        );
        let shader = ModernShader::wgsl(
            xxhash_rust::xxh3::xxh3_64(entry.as_bytes()),
            source,
            entry,
            adv_gpu::shader::Stage::Compute,
        )
        .unwrap();
        let pipeline = gpu.prepare_dispatch(Arc::new(shader)).unwrap();
        let slots = SlotBindings {
            storage: buffers
                .iter()
                .cloned()
                .map(|buffer| Some(StorageBinding::Buffer(buffer)))
                .collect(),
            ..Default::default()
        };
        gpu.dispatch(&pipeline, &slots, DispatchArguments::Direct([groups, 1, 1]))
            .unwrap();
    }
    for (output, dst) in outputs.into_iter().zip(&readbacks) {
        gpu.transfer(ModernTransfer::CopyBuffer {
            source: output,
            destination: dst.buffer.clone(),
            offset: 0,
        })
        .unwrap();
    }
    let fence = gpu.target(1, 1, false).unwrap();
    gpu.clear(&fence, Some([0.; 4]), None).unwrap();
    let mut pending = gpu.readback(&fence.image, None).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(result) = pending.poll() {
            result.unwrap();
            break;
        }
        gpu.check().unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "tessellation readback timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for (readback, expected) in readbacks.iter().zip(&expected) {
        // The readback above waited for all copies.
        let words: &[u32] =
            bytemuck::cast_slice(unsafe { readback.buffer.mapped().unwrap().as_ref() });
        assert_eq!(words.len(), expected.len());
        if let Some(index) = words.iter().zip(expected).position(|(a, b)| a != b) {
            panic!(
                "{} at word {index} expected {:08x}, got {:08x}",
                words.len(),
                expected[index],
                words[index]
            );
        }
    }
}
