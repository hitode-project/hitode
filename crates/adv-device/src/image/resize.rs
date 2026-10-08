//! D3DX NONE, POINT and LINEAR pixel resizing, measured against native D3DX9.
//! POINT uses integer source cells; LINEAR samples pixel centers and wraps edges.
/// Resize tightly packed BGRA8 pixels. Callers validate positive dimensions,
/// source length and the bounded destination extent before calling.
pub fn pixels(input: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, filter: super::Filter) -> Vec<u8> {
    let mut output = vec![0; dw as usize * dh as usize * 4];
    for y in 0..dh {
        for x in 0..dw {
            let dest = ((y * dw + x) * 4) as usize;
            if filter == super::Filter::Copy {
                if x < sw && y < sh {
                    let src = ((y * sw + x) * 4) as usize;
                    output[dest..dest + 4].copy_from_slice(&input[src..src + 4]);
                }
            } else if filter == super::Filter::Point {
                let sx = u64::from(x) * u64::from(sw) / u64::from(dw);
                let sy = u64::from(y) * u64::from(sh) / u64::from(dh);
                let src = ((sy * u64::from(sw) + sx) * 4) as usize;
                output[dest..dest + 4].copy_from_slice(&input[src..src + 4]);
            } else {
                let sx = (f64::from(x) + 0.5) * f64::from(sw) / f64::from(dw) - 0.5;
                let sy = (f64::from(y) + 0.5) * f64::from(sh) / f64::from(dh) - 0.5;
                let ix = sx.floor() as i64;
                let iy = sy.floor() as i64;
                let fx = sx - sx.floor();
                let fy = sy - sy.floor();
                let at = |x: i64, y: i64, c: usize| {
                    input[((y.rem_euclid(i64::from(sh)) * i64::from(sw)
                        + x.rem_euclid(i64::from(sw)))
                        * 4) as usize
                        + c] as f64
                };
                for c in 0..4 {
                    let top = at(ix, iy, c) * (1. - fx) + at(ix + 1, iy, c) * fx;
                    let bottom = at(ix, iy + 1, c) * (1. - fx) + at(ix + 1, iy + 1, c) * fx;
                    output[dest + c] =
                        (top * (1. - fy) + bottom * fy).round().clamp(0., 255.) as u8;
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_point_cells_linear_wrapping_and_none_padding() {
        let source: Vec<u8> = (0..3)
            .flat_map(|y| (0..4).flat_map(move |x| [20 + x * 40, 30 + y * 60, 31, 127]))
            .collect();
        let point = pixels(&source, 4, 3, 4, 4, super::super::Filter::Point);
        assert_eq!(
            (0..4).map(|y| point[y * 16 + 1]).collect::<Vec<_>>(),
            [30, 30, 90, 150]
        );
        let linear = pixels(&source, 4, 3, 8, 8, super::super::Filter::LinearWrap);
        assert_eq!(
            (0..8).map(|x| linear[x * 4]).collect::<Vec<_>>(),
            [50, 30, 50, 70, 90, 110, 130, 110]
        );
        assert_eq!(
            (0..8).map(|y| linear[y * 32 + 1]).collect::<Vec<_>>(),
            [68, 34, 56, 79, 101, 124, 146, 113]
        );
        let none = pixels(&source, 4, 3, 4, 4, super::super::Filter::Copy);
        assert_eq!(&none[..source.len()], &source);
        assert_eq!(&none[source.len()..], &[0; 16]);
    }
}
