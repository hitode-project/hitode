//! Prepared off the callback thread. Processing uses only preallocated buffers.
use rubato::{
    Resampler, SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub(crate) struct Converter {
    inner: Option<SincFixedOut<f32>>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    at: usize,
    frequency: u32,
    pub rate: u32,
}
impl Converter {
    pub fn new(frequency: u32, rate: u32) -> Result<Self, String> {
        if frequency == 0 || rate == 0 {
            return Err("zero audio rate".into());
        }
        let inner = if frequency == rate {
            None
        } else {
            Some(
                SincFixedOut::<f32>::new(
                    f64::from(rate) / f64::from(frequency),
                    1.0,
                    SincInterpolationParameters {
                        sinc_len: 256,
                        f_cutoff: 0.95,
                        oversampling_factor: 128,
                        interpolation: SincInterpolationType::Cubic,
                        window: WindowFunction::BlackmanHarris2,
                    },
                    128,
                    2,
                )
                .map_err(|error| error.to_string())?,
            )
        };
        let input = inner
            .as_ref()
            .map_or_else(Vec::new, |inner| inner.input_buffer_allocate(true));
        let output = inner
            .as_ref()
            .map_or_else(Vec::new, |inner| inner.output_buffer_allocate(true));
        Ok(Self {
            inner,
            input,
            output,
            at: 128,
            frequency,
            rate,
        })
    }
    pub fn matches(&self, frequency: u32, rate: u32) -> bool {
        self.frequency == frequency && self.rate == rate
    }
    pub fn reset(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.reset();
        }
        self.at = 128;
    }
    /// Causal sinc history plus a prepared output block. Zero-rate identity
    /// sources have no delayed filter tail.
    pub fn tail_frames(&self) -> u32 {
        self.inner
            .as_ref()
            .map_or(0, |inner| (inner.output_delay() * 2 + 128) as u32)
    }
    pub fn next(&mut self, mut source: impl FnMut() -> [f32; 2]) -> [f32; 2] {
        let Some(inner) = self.inner.as_mut() else {
            return source();
        };
        if self.at == 128 {
            let frames = inner.input_frames_next();
            for i in 0..frames {
                let sample = source();
                self.input[0][i] = sample[0];
                self.input[1][i] = sample[1];
            }
            if inner
                .process_into_buffer(&self.input, &mut self.output, None)
                .is_err()
            {
                // Validated buffers and immutable rates make this unreachable;
                // retain bounded silence if a resampler ever rejects a block.
                self.output[0].fill(0.0);
                self.output[1].fill(0.0);
            }
            self.at = 0;
        }
        let sample = [self.output[0][self.at], self.output[1][self.at]];
        self.at += 1;
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sinc_downsampling_rejects_aliases_and_preserves_audible_amplitude() {
        for frequency in [1000.0f64, 20000.0] {
            let mut converter = Converter::new(48000, 16000).unwrap();
            let mut input_frame = 0;
            let mut square = 0.0;
            let mut frames = 0;
            for i in 0..16000 {
                let sample = converter.next(|| {
                    let value = (input_frame as f64 * std::f64::consts::TAU * frequency / 48000.0)
                        .sin() as f32
                        * 0.5;
                    input_frame += 1;
                    [value, value]
                });
                assert_eq!(sample[0], sample[1]);
                if i > 1024 {
                    square += f64::from(sample[0]).powi(2);
                    frames += 1;
                }
            }
            let rms = (square / frames as f64).sqrt();
            if frequency == 1000.0 {
                assert!(
                    (rms - 0.5 / 2.0f64.sqrt()).abs() < 0.0001,
                    "audible amplitude {rms}"
                );
            } else {
                assert!(
                    rms < 0.00003,
                    "20 kHz must not fold to audible 4 kHz: {rms}"
                );
            }
        }
    }
    #[test]
    fn equal_rates_preserve_samples_without_filter_latency() {
        let mut converter = Converter::new(44100, 44100).unwrap();
        for i in 0..1024 {
            let value = [i as f32 / 2048.0, -(i as f32) / 2048.0];
            assert_eq!(converter.next(|| value), value);
        }
    }

    #[test]
    fn restarting_an_epoch_matches_a_fresh_converter_even_inside_a_prepared_block() {
        for rate in [44_100, 48_000, 96_000, 192_000] {
            let source = |frame: u64| {
                let value =
                    (frame as f64 * std::f64::consts::TAU * 997.0 / 48_000.0).sin() as f32 * 0.25;
                [value, -value]
            };
            let mut reused = Converter::new(48_000, rate).unwrap();
            let mut old_input = 0;
            for _ in 0..4097 {
                reused.next(|| {
                    let value = source(old_input);
                    old_input += 1;
                    value
                });
            }
            reused.reset();
            let mut fresh = Converter::new(48_000, rate).unwrap();
            let mut reused_input = 12_345;
            let mut fresh_input = reused_input;
            for output in 0..3000 {
                let expected = fresh.next(|| {
                    let value = source(fresh_input);
                    fresh_input += 1;
                    value
                });
                let actual = reused.next(|| {
                    let value = source(reused_input);
                    reused_input += 1;
                    value
                });
                assert_eq!(actual, expected, "epoch restart at {rate}, output {output}");
            }
            assert_eq!(reused_input, fresh_input, "input consumption at {rate}");
        }
    }
}
