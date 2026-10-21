use super::*;

fn apply(point: [f32; 2], matrix: Matrix) -> [f32; 2] {
    let p = transform_value([point[0], point[1], 0., 1.], matrix);
    [p[0] / p[3], p[1] / p[3]]
}
fn close(actual: [f32; 2], expected: [f32; 2]) {
    for (a, e) in actual.into_iter().zip(expected) {
        assert!((a - e).abs() < 1e-4, "{actual:?} != {expected:?}");
    }
}

#[test]
fn transformation_2d_scales_about_its_center_then_rotates_and_translates() {
    let quarter = std::f32::consts::FRAC_PI_2;
    // Scale (2,3) about (1,1): (2,2) -> (3,4).
    let scale = transformation_2d_value([1., 1.], 0., [2., 3.], [0., 0.], 0., [0., 0.]);
    close(apply([2., 2.], scale), [3., 4.]);
    // Rotate a quarter turn about (1,0), then translate by (5,0): (2,0) -> (6,1).
    let turn = transformation_2d_value([0., 0.], 0., [1., 1.], [1., 0.], quarter, [5., 0.]);
    close(apply([2., 0.], turn), [6., 1.]);
    // A rotated scaling axis stretches along the diagonal and keeps it.
    let diagonal =
        transformation_2d_value([0., 0.], quarter / 2., [2., 1.], [0., 0.], 0., [0., 0.]);
    close(apply([1., 1.], diagonal), [2., 2.]);
    close(apply([1., -1.], diagonal), [1., -1.]);
}

#[test]
fn screen_projection_maps_pixel_edges_with_the_half_pixel_offset() {
    let projection = screen_projection(D3DVIEWPORT9 {
        X: 0,
        Y: 0,
        Width: 1280,
        Height: 720,
        MinZ: 0.,
        MaxZ: 1.,
    });
    close(apply([0.5, 0.5], projection), [-1., 1.]);
    close(apply([1280.5, 720.5], projection), [1., -1.]);
}

#[test]
fn quads_place_the_source_rectangle_at_position_minus_center() {
    let mut transform = IDENTITY;
    transform[12] = 100.;
    let quad = Quad {
        texture: 1,
        owned: true,
        rect: [64, 32, 96, 80],
        size: [128, 128],
        center: [8., 4., 0.],
        position: [10., 20., 0.5],
        color: 0x80ff_ffff,
        transform,
    };
    let v = vertices(&quad);
    let f = |w: u32| f32::from_bits(w);
    assert_eq!([f(v[0][0]), f(v[0][1]), f(v[0][2])], [102., 16., 0.5]);
    assert_eq!([f(v[2][0]), f(v[2][1])], [134., 64.]);
    assert_eq!([f(v[0][4]), f(v[0][5])], [0.5, 0.25]);
    assert_eq!([f(v[2][4]), f(v[2][5])], [0.75, 0.625]);
    assert_eq!(v[1][3], 0x80ff_ffff);
    assert_eq!([v[4], v[5]], [v[0], v[2]]);
}
