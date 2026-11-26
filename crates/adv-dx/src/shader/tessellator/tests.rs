use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::*;

#[test]
// spec: GFX-TESS-1
fn matches_d3d11_stream_output() {
    let mut cases = 0;
    for row in include_str!("fixture.tsv")
        .lines()
        .filter(|row| !row.starts_with('#'))
    {
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
        cases += 1;
    }
    assert_eq!(cases, 252);
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
