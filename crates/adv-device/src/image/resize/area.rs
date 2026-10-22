//! D3DX tent reconstruction and the SDK's 2x2 mipmap box kernel.
use crate::image::Filter;

pub(super) fn pixels(input: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, filter: Filter) -> Vec<u8> {
    let mut output = vec![0; dw as usize * dh as usize * 4];
    let sample = |x: i64, y: i64, c: usize| {
        f64::from(
            input[((y.rem_euclid(i64::from(sh)) * i64::from(sw) + x.rem_euclid(i64::from(sw))) * 4)
                as usize
                + c],
        )
    };
    for y in 0..dh {
        for x in 0..dw {
            let mut channels = [0.; 4];
            if filter == Filter::Box {
                for sy in [y * 2, (y * 2 + 1).min(sh - 1)] {
                    for sx in [x * 2, (x * 2 + 1).min(sw - 1)] {
                        for (c, sum) in channels.iter_mut().enumerate() {
                            *sum += sample(i64::from(sx), i64::from(sy), c) * 0.25;
                        }
                    }
                }
            } else {
                let rx = f64::from(sw) / f64::from(dw);
                let ry = f64::from(sh) / f64::from(dh);
                let cx = (f64::from(x) + 0.5) * rx - 0.5;
                let cy = (f64::from(y) + 0.5) * ry - 0.5;
                let (radius_x, radius_y) = (rx.max(1.), ry.max(1.));
                let mut total = 0.;
                for sy in (cy - radius_y).floor() as i64..=(cy + radius_y).ceil() as i64 {
                    let wy = (1. - (sy as f64 - cy).abs() / radius_y).max(0.);
                    if wy == 0. {
                        continue;
                    }
                    for sx in (cx - radius_x).floor() as i64..=(cx + radius_x).ceil() as i64 {
                        let weight = wy * (1. - (sx as f64 - cx).abs() / radius_x).max(0.);
                        if weight == 0. {
                            continue;
                        }
                        for (c, sum) in channels.iter_mut().enumerate() {
                            *sum += sample(sx, sy, c) * weight;
                        }
                        total += weight;
                    }
                }
                for channel in &mut channels {
                    *channel /= total;
                }
            }
            for (c, value) in channels.into_iter().enumerate() {
                output[((y * dw + x) * 4) as usize + c] = value.round().clamp(0., 255.) as u8;
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: DEV-IMAGE-1
    fn box_mips_average_four_pixels_and_tent_downsampling_wraps_edges() {
        let input: Vec<u8> = (0..4)
            .flat_map(|y| (0..4).flat_map(move |x| [x * 40, y * 40, 100, 255]))
            .collect();
        let half = pixels(&input, 4, 4, 2, 2, Filter::Box);
        assert_eq!(
            half,
            [
                20, 20, 100, 255, 100, 20, 100, 255, 20, 100, 100, 255, 100, 100, 100, 255
            ]
        );
        assert_eq!(pixels(&half, 2, 2, 1, 1, Filter::Box), [60, 60, 100, 255]);
        assert_eq!(
            pixels(&input, 4, 4, 2, 2, Filter::TriangleWrap),
            [
                40, 40, 100, 255, 80, 40, 100, 255, 40, 80, 100, 255, 80, 80, 100, 255
            ]
        );
    }
}
