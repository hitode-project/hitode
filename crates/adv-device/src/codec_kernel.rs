//! Stateless codec math. Buffers, plans and scratch belong to one caller;
//! no guest pointers, stream state or I/O enter these kernels.
mod deflate;
pub use deflate::inflate_fast;

use symphonia::core::dsp::mdct::Imdct;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    UnsupportedSize,
    NonFiniteInput,
    NonFiniteOutput,
}

/// PNG Sub on a complete row (DEV-CODEC-1). The first pixel is unchanged.
pub fn png_sub(row: &mut [u8], bpp: usize) -> Result<(), KernelError> {
    if bpp == 0 {
        return Err(KernelError::UnsupportedSize);
    }
    if bpp >= row.len() {
        return Ok(());
    }
    let (prefix, remaining) = row.split_at_mut(bpp);
    png_sub_segment(remaining, prefix)
}

/// Sub on a remaining row segment. `prefix` is the preceding pixel, or its
/// initial portion when the segment is shorter than one pixel. It is disjoint
/// from `row`; later predictor bytes come from the updated row itself.
pub fn png_sub_segment(row: &mut [u8], prefix: &[u8]) -> Result<(), KernelError> {
    if prefix.is_empty() {
        return Err(KernelError::UnsupportedSize);
    }
    for i in 0..row.len() {
        let left = if i < prefix.len() {
            prefix[i]
        } else {
            row[i - prefix.len()]
        };
        row[i] = row[i].wrapping_add(left);
    }
    Ok(())
}

/// PNG Up on a complete row or corresponding row segments (DEV-CODEC-1).
pub fn png_up(row: &mut [u8], previous: &[u8]) -> Result<(), KernelError> {
    if row.len() != previous.len() {
        return Err(KernelError::UnsupportedSize);
    }
    for (byte, &above) in row.iter_mut().zip(previous) {
        *byte = byte.wrapping_add(above);
    }
    Ok(())
}

/// PNG Average. The sum is widened before division and byte truncation.
pub fn png_average(row: &mut [u8], previous: &[u8], bpp: usize) -> Result<(), KernelError> {
    if row.len() != previous.len() || bpp == 0 {
        return Err(KernelError::UnsupportedSize);
    }
    let first = bpp.min(row.len());
    let (prefix, remaining) = row.split_at_mut(first);
    for (byte, &above) in prefix.iter_mut().zip(previous) {
        *byte = byte.wrapping_add(above / 2);
    }
    if remaining.is_empty() {
        return Ok(());
    }
    png_average_segment(remaining, &previous[first..], prefix)
}

/// Average on a remaining segment, retaining reconstructed bytes before the
/// segment. For a segment shorter than one pixel, only that prefix portion
/// is needed. Updated in-segment history is read from `row` itself.
pub fn png_average_segment(
    row: &mut [u8],
    previous: &[u8],
    prefix: &[u8],
) -> Result<(), KernelError> {
    if row.len() != previous.len() || prefix.is_empty() {
        return Err(KernelError::UnsupportedSize);
    }
    for i in 0..row.len() {
        let left = if i < prefix.len() {
            prefix[i]
        } else {
            row[i - prefix.len()]
        };
        let predictor = (u16::from(left) + u16::from(previous[i])) / 2;
        row[i] = row[i].wrapping_add(predictor as u8);
    }
    Ok(())
}

/// PNG Paeth. Ties prefer left, then above, then upper left.
pub fn png_paeth(row: &mut [u8], previous: &[u8], bpp: usize) -> Result<(), KernelError> {
    if row.len() != previous.len() || bpp == 0 {
        return Err(KernelError::UnsupportedSize);
    }
    let first = bpp.min(row.len());
    let (prefix, remaining) = row.split_at_mut(first);
    png_up(prefix, &previous[..first])?;
    if remaining.is_empty() {
        return Ok(());
    }
    png_paeth_segment(remaining, &previous[first..], prefix, &previous[..first])
}

/// Paeth on a remaining segment, with reconstructed current and previous
/// prefixes. Prefix borrows are separate from the mutable output; updated
/// history inside the batch is read from `row` itself.
pub fn png_paeth_segment(
    row: &mut [u8],
    previous: &[u8],
    prefix: &[u8],
    previous_prefix: &[u8],
) -> Result<(), KernelError> {
    if row.len() != previous.len() || prefix.is_empty() || prefix.len() != previous_prefix.len() {
        return Err(KernelError::UnsupportedSize);
    }
    match prefix.len() {
        1 => paeth_pixels::<1>(row, previous, prefix, previous_prefix),
        2 => paeth_pixels::<2>(row, previous, prefix, previous_prefix),
        3 => paeth_pixels::<3>(row, previous, prefix, previous_prefix),
        4 => paeth_pixels::<4>(row, previous, prefix, previous_prefix),
        6 => paeth_pixels::<6>(row, previous, prefix, previous_prefix),
        8 => paeth_pixels::<8>(row, previous, prefix, previous_prefix),
        _ => {
            for i in 0..row.len() {
                let (left, corner) = if i < prefix.len() {
                    (prefix[i], previous_prefix[i])
                } else {
                    (row[i - prefix.len()], previous[i - prefix.len()])
                };
                row[i] = row[i].wrapping_add(paeth_predictor(left, previous[i], corner));
            }
        }
    }
    Ok(())
}

#[inline]
fn paeth_predictor(left: u8, above: u8, corner: u8) -> u8 {
    let x = i16::from(above) - i16::from(corner);
    let y = i16::from(left) - i16::from(corner);
    let (a, b, c) = (x.abs(), y.abs(), (x + y).abs());
    let (best, minimum) = if b < a { (above, b) } else { (left, a) };
    if c < minimum { corner } else { best }
}

// PNG channel widths are constant within a row. Keeping each channel's history
// in a fixed array removes per-byte history selection and exposes lane parallelism.
fn paeth_pixels<const N: usize>(
    row: &mut [u8],
    previous: &[u8],
    prefix: &[u8],
    previous_prefix: &[u8],
) {
    let mut left: [u8; N] = prefix.try_into().unwrap();
    let mut corner: [u8; N] = previous_prefix.try_into().unwrap();
    let (pixels, tail) = row.as_chunks_mut::<N>();
    let (above, above_tail) = previous.as_chunks::<N>();
    for (pixel, above) in pixels.iter_mut().zip(above) {
        for i in 0..N {
            pixel[i] = pixel[i].wrapping_add(paeth_predictor(left[i], above[i], corner[i]));
            left[i] = pixel[i];
            corner[i] = above[i];
        }
    }
    for i in 0..tail.len() {
        tail[i] = tail[i].wrapping_add(paeth_predictor(left[i], above_tail[i], corner[i]));
    }
}

/// Forward LZ77 expansion. Reads include history written by this call.
pub fn lz77(
    window: &mut [u8],
    destination: usize,
    distance: usize,
    length: usize,
) -> Result<(), KernelError> {
    if distance == 0
        || distance > destination
        || destination
            .checked_add(length)
            .is_none_or(|end| end > window.len())
    {
        return Err(KernelError::UnsupportedSize);
    }
    if length == 0 {
        return Ok(());
    }
    let (history, output) = window[..destination + length].split_at_mut(destination);
    let prefix = &history[destination - distance..destination - distance + distance.min(length)];
    lz77_segment(output, prefix)
}

/// Forward expansion on a remaining segment. The disjoint prefix contains
/// min(distance, length) history bytes; overlapping reads use updated output.
pub fn lz77_segment(output: &mut [u8], prefix: &[u8]) -> Result<(), KernelError> {
    if prefix.is_empty() {
        return Err(KernelError::UnsupportedSize);
    }
    for i in 0..output.len() {
        output[i] = if i < prefix.len() {
            prefix[i]
        } else {
            output[i - prefix.len()]
        };
    }
    Ok(())
}

struct Plan {
    transform: Imdct,
    output: Box<[f32]>,
}

/// At most eight lazy plans for 64..=8192 time-domain samples. The number of
/// streams and calls cannot grow this cache. Mutability prevents concurrent use
/// of the same scratch; a future compute lane must own its own cache.
#[derive(Default)]
pub struct ImdctCache {
    plans: [Option<Plan>; 8],
}

impl ImdctCache {
    /// Unscaled inverse MDCT, N spectral inputs to 2N time-domain outputs.
    /// No window, overlap-add or PCM quantization is performed here. The result
    /// borrows reusable scratch and must be consumed before the next call.
    pub fn inverse(&mut self, input: &[f32]) -> Result<&[f32], KernelError> {
        let count = input.len();
        if !(32..=4096).contains(&count) || !count.is_power_of_two() {
            return Err(KernelError::UnsupportedSize);
        }
        if input.iter().any(|x| !x.is_finite()) {
            return Err(KernelError::NonFiniteInput);
        }
        let slot = count.trailing_zeros() as usize - 5;
        let plan = self.plans[slot].get_or_insert_with(|| Plan {
            transform: Imdct::new(count),
            output: vec![0.0; count * 2].into_boxed_slice(),
        });
        plan.transform.imdct(input, &mut plan.output);
        if plan.output.iter().any(|x| !x.is_finite()) {
            return Err(KernelError::NonFiniteOutput);
        }
        Ok(&plan.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: DEV-CODEC-1
    #[test]
    fn integer_rows_and_overlapping_expansion() {
        let mut row = [1, 2, 3, 4, 5, 6];
        png_sub(&mut row, 2).unwrap();
        assert_eq!(row, [1, 2, 4, 6, 9, 12]);
        let mut row = [1, 2, 3, 4, 5, 6];
        png_up(&mut row, &[250, 254, 0, 255, 5, 6]).unwrap();
        assert_eq!(row, [251, 0, 3, 3, 10, 12]);
        let mut row = [1, 2, 3, 4, 5, 6];
        png_average(&mut row, &[250, 254, 200, 100, 250, 10], 2).unwrap();
        assert_eq!(row, [126, 129, 166, 118, 213, 70]);
        let mut row = [128, 0];
        png_average(&mut row, &[254, 255], 1).unwrap();
        assert_eq!(row, [255, 255]);
        for (filtered, above, expected) in [
            ([20, 0], [100, 90], [120, 120]),
            ([246, 0], [100, 120], [90, 120]),
            ([20, 0], [100, 80], [120, 100]),
        ] {
            let mut row = filtered;
            png_paeth(&mut row, &above, 1).unwrap();
            assert_eq!(row, expected);
        }
        let mut window = [b'a', b'b', 0, 0, 0, 0, 0, 0];
        lz77(&mut window, 2, 2, 6).unwrap();
        assert_eq!(&window, b"abababab");
        lz77(&mut window, 1, 1, 7).unwrap();
        assert_eq!(&window, b"aaaaaaaa");
    }

    #[test]
    fn byte_kernel_rejection_has_no_effects() {
        let mut row = [255, 128, 64];
        let original = row;
        assert!(png_sub(&mut row, 0).is_err());
        assert!(png_up(&mut row, &[1]).is_err());
        assert!(png_average(&mut row, &[1, 2, 3], 0).is_err());
        assert!(png_paeth(&mut row, &[1], 1).is_err());
        for (at, distance, len) in [(1, 0, 1), (1, 2, 1), (2, 1, 2), (1, 1, usize::MAX)] {
            assert!(lz77(&mut row, at, distance, len).is_err());
        }
        assert_eq!(row, original);
    }

    #[test]
    fn unscaled_impulse_has_correct_order_and_reuses_bounded_scratch() {
        let mut cache = ImdctCache::default();
        let mut spectral = [0.0; 32];
        spectral[0] = 1.0;
        let actual = cache.inverse(&spectral).unwrap();
        for (i, &value) in actual.iter().enumerate() {
            let expected = (std::f64::consts::PI / 128.0 * (2 * i + 33) as f64).cos();
            assert!((f64::from(value) - expected).abs() < 2e-7);
        }
        let pointer = actual.as_ptr();
        let actual = cache.inverse(&[0.0; 32]).unwrap();
        assert_eq!(pointer, actual.as_ptr());
        assert!(actual.iter().all(|x| *x == 0.0));
        assert_eq!(cache.inverse(&[0.0; 4096]).unwrap().len(), 8192);
        assert_eq!(cache.plans.iter().filter(|x| x.is_some()).count(), 2);
    }

    #[test]
    fn unsupported_or_nonfinite_data_is_rejected_before_plan_creation() {
        let mut cache = ImdctCache::default();
        assert_eq!(cache.inverse(&[0.0; 31]), Err(KernelError::UnsupportedSize));
        assert_eq!(cache.inverse(&[0.0; 33]), Err(KernelError::UnsupportedSize));
        assert_eq!(
            cache.inverse(&[f32::NAN; 32]),
            Err(KernelError::NonFiniteInput)
        );
        assert_eq!(
            cache.inverse(&[f32::INFINITY; 32]),
            Err(KernelError::NonFiniteInput)
        );
        assert!(cache.plans.iter().all(Option::is_none));
    }
}
