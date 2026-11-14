//! Prepared off the callback thread. Processing uses only preallocated buffers.
//!
//! A fixed rate pair places output frame n at n * frequency / rate input
//! frames, so every fraction is a multiple of 1 / phases with phases =
//! rate / gcd(rate, frequency). Sinc tables of exactly that many phases give
//! the filter at the exact position with one product per frame; other pairs
//! interpolate cubically between 128 phases, four products per frame. Tables
//! depend only on their phase count and cutoff and are shared by every
//! converter.
use rubato::sinc_interpolator::SincInterpolator;
use rubato::sinc_interpolator::sinc_interpolator_neon::NeonInterpolator;
use rubato::{Resampler, SincFixedOut, SincInterpolationType, WindowFunction};
use std::sync::{Arc, Mutex};

const SINC_LEN: usize = 256;
/// Exact tables up to the 640 phases of 11025 Hz into 48 kHz.
const EXACT_PHASES: u32 = 640;
const CUBIC_PHASES: usize = 128;

struct Shared(Arc<NeonInterpolator<f32>>);
impl SincInterpolator<f32> for Shared {
    fn get_sinc_interpolated(&self, wave: &[f32], index: usize, subindex: usize) -> f32 {
        self.0.get_sinc_interpolated(wave, index, subindex)
    }
    fn len(&self) -> usize {
        SINC_LEN
    }
    fn nbr_sincs(&self) -> usize {
        self.0.nbr_sincs()
    }
}

/// As rubato's own construction: downsampling lowers the cutoff by the ratio.
fn table(phases: usize, ratio: f64) -> Result<Shared, String> {
    type Tables = Vec<(usize, u32, Arc<NeonInterpolator<f32>>)>;
    static TABLES: Mutex<Tables> = Mutex::new(Vec::new());
    let cutoff = if ratio >= 1.0 {
        0.95
    } else {
        0.95 * ratio as f32
    };
    let mut tables = TABLES.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some((.., table)) = tables
        .iter()
        .find(|&&(count, bits, _)| (count, bits) == (phases, cutoff.to_bits()))
    {
        return Ok(Shared(table.clone()));
    }
    let table = Arc::new(
        NeonInterpolator::new(SINC_LEN, phases, cutoff, WindowFunction::BlackmanHarris2)
            .map_err(|error| error.to_string())?,
    );
    tables.push((phases, cutoff.to_bits(), table.clone()));
    Ok(Shared(table))
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

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
            let ratio = f64::from(rate) / f64::from(frequency);
            let phases = rate / gcd(rate, frequency);
            let (interpolation, phases) = if phases <= EXACT_PHASES {
                (SincInterpolationType::Nearest, phases as usize)
            } else {
                (SincInterpolationType::Cubic, CUBIC_PHASES)
            };
            Some(
                SincFixedOut::<f32>::new_with_interpolator(
                    ratio,
                    1.0,
                    interpolation,
                    Box::new(table(phases, ratio)?),
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
    fn sine(frame: usize) -> f64 {
        (frame as f64 * std::f64::consts::TAU * 997.0 / 44_100.0).sin() * 0.5
    }

    fn converted(mut converter: Converter, frames: usize) -> Vec<f64> {
        let mut input = 0;
        (0..frames)
            .map(|_| {
                let sample = converter.next(|| {
                    input += 1;
                    let value = sine(input - 1) as f32;
                    [value, -value]
                });
                assert_eq!(sample[0], -sample[1]);
                f64::from(sample[0])
            })
            .collect()
    }

    /// Rubato building its own table: cubic phases, or exact ones otherwise.
    fn rubato<T: rubato::Sample>(
        frequency: u32,
        rate: u32,
        (cubic, phases): (bool, usize),
        frames: usize,
        wide: fn(T) -> f64,
    ) -> Vec<f64> {
        let mut resampler = SincFixedOut::<T>::new(
            f64::from(rate) / f64::from(frequency),
            1.0,
            rubato::SincInterpolationParameters {
                sinc_len: SINC_LEN,
                f_cutoff: 0.95,
                oversampling_factor: phases,
                interpolation: if cubic {
                    SincInterpolationType::Cubic
                } else {
                    SincInterpolationType::Nearest
                },
                window: WindowFunction::BlackmanHarris2,
            },
            128,
            2,
        )
        .unwrap();
        let mut input = resampler.input_buffer_allocate(true);
        let mut output = resampler.output_buffer_allocate(true);
        let (mut consumed, mut out) = (0, Vec::new());
        while out.len() < frames {
            for i in 0..resampler.input_frames_next() {
                let value = T::coerce(sine(consumed) as f32 as f64);
                input[0][i] = value;
                input[1][i] = -value;
                consumed += 1;
            }
            resampler
                .process_into_buffer(&input, &mut output, None)
                .unwrap();
            out.extend(output[0].iter().map(|&value| wide(value)));
        }
        out.truncate(frames);
        out
    }

    #[test]
    fn shared_tables_reproduce_rubato_construction() {
        for (frequency, rate, table) in [
            (44_100, 48_000, (false, 160)),
            (22_050, 48_000, (false, 320)),
            (48_000, 44_100, (false, 147)),
            (44_101, 48_000, (true, CUBIC_PHASES)),
        ] {
            // A second converter reuses the first one's table.
            for _ in 0..2 {
                assert_eq!(
                    converted(Converter::new(frequency, rate).unwrap(), 4096),
                    rubato(frequency, rate, table, 4096, <f64 as From<f32>>::from),
                    "{frequency} -> {rate}"
                );
            }
        }
    }

    #[test]
    fn exact_phases_stay_closer_to_the_filter_than_cubic_phases() {
        let ideal = rubato(44_100, 48_000, (false, 160), 8192, |value: f64| value);
        let error = |output: Vec<f64>| {
            output[1024..]
                .iter()
                .zip(&ideal[1024..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max)
        };
        let exact = error(converted(Converter::new(44_100, 48_000).unwrap(), 8192));
        let cubic = error(rubato(
            44_100,
            48_000,
            (true, CUBIC_PHASES),
            8192,
            <f64 as From<f32>>::from,
        ));
        assert!(exact < 1e-6, "f32 rounding only: {exact}");
        assert!(exact * 100.0 < cubic, "exact {exact}, cubic {cubic}");
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
