//! Stateless codec math using the same Symphonia DSP implementation as decoding.
//! Plans and scratch belong to one caller; no guest pointers, stream state or I/O.
use symphonia::core::dsp::mdct::Imdct;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    UnsupportedSize,
    NonFiniteInput,
    NonFiniteOutput,
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
