//! Device-independent DAC acceptance. These tests exercise the production DMA
//! consumer; they do not imply physical-device testing at every output rate.
use adv_device::{AudioControl, AudioEvent, RingState, ring::RingConfig};
use adv_sync::shared::SharedReadView;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const OUTPUT_RATES: [u32; 9] = [
    32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];
const SOURCE_RATES: [u32; 3] = [22_050, 44_100, 48_000];
const AMPLITUDE: f64 = 10_000.0 / 32_768.0;

struct Pcm(Vec<AtomicU8>);
impl SharedReadView for Pcm {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn load_u8(&self, index: usize) -> u8 {
        self.0
            .get(index)
            .map_or(0, |byte| byte.load(Ordering::Relaxed))
    }
}

fn source(control: &AudioControl, rate: u32, channels: u16) -> Arc<RingState> {
    // Integer frequencies make the one-second storage loop continuous. Stereo
    // channels differ, so swapping, duplicating or dropping a channel is visible.
    let bytes: Vec<_> = (0..rate)
        .flat_map(|frame| {
            (0..channels).flat_map(move |channel| {
                let frequency = if channel == 0 { 997.0 } else { 503.0 };
                let value = (10_000.0
                    * (f64::from(frame) * std::f64::consts::TAU * frequency / f64::from(rate))
                        .sin())
                .round() as i16;
                value.to_le_bytes()
            })
        })
        .map(AtomicU8::new)
        .collect();
    let len = bytes.len() as u32;
    let state = control
        .add_ring(
            1,
            RingConfig {
                view: Arc::new(Pcm(bytes)),
                bytes: len,
                channels,
                bits: 16,
                frequency: rate,
                output_rate: 44_100,
                notifications: vec![(rate / 2 * u32::from(channels) * 2, 7)],
            },
        )
        .unwrap();
    assert!(control.written(1, 0, len));
    assert!(control.play_ring(1, true, 1));
    state
}

fn render(reader: &mut adv_device::DeviceReader, frames: usize, channels: u16) -> Vec<f32> {
    // Callback sizes need not be multiples of the resampler block, and change
    // between devices. Include a one-frame callback and a large startup callback.
    let sizes = [4224, 1, 257, 1919, 4096, 128, 2053];
    let channels = usize::from(channels);
    let mut out = vec![0.0; frames * channels];
    let mut first = 0;
    let mut index = 0;
    while first < frames {
        let count = sizes[index % sizes.len()].min(frames - first);
        reader.read(
            &mut out[first * channels..(first + count) * channels],
            20_000,
        );
        first += count;
        index += 1;
    }
    out
}

fn check_waveform(
    output: &[f32],
    rate: u32,
    source_channels: u16,
    output_channels: u16,
    label: &str,
) {
    assert!(
        output.iter().all(|s| s.is_finite() && s.abs() < 0.4),
        "invalid amplitude {label}"
    );
    // Exclude startup filter latency, but retain callback and storage-loop edges.
    let channels = usize::from(output_channels);
    let stable = &output[(rate as usize / 25) * channels..];
    let frames = stable.len() / channels;
    for channel in 0..channels.min(2) {
        let values: Vec<f32> = stable.chunks_exact(channels).map(|f| f[channel]).collect();
        let rms =
            (values.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / frames as f64).sqrt();
        let mixed = source_channels == 2 && output_channels == 1;
        let expected_rms = if mixed {
            AMPLITUDE / 2.0
        } else {
            AMPLITUDE / 2.0f64.sqrt()
        };
        assert!(
            (rms - expected_rms).abs() < 0.0005,
            "RMS {rms} vs {expected_rms}: {label}"
        );
        if !mixed {
            let frequency = if source_channels == 2 && channel == 1 {
                503.0
            } else {
                997.0
            };
            let crossings = values
                .windows(2)
                .filter(|p| p[0] <= 0.0 && p[1] > 0.0)
                .count();
            let measured = crossings as f64 * f64::from(rate) / frames as f64;
            assert!(
                (measured - frequency).abs() < 1.5,
                "pitch {measured} vs {frequency}: {label}"
            );
        }
        let max_delta = values
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0f32, f32::max);
        let derivative_bound = AMPLITUDE * std::f64::consts::TAU * 997.0 / f64::from(rate);
        assert!(
            f64::from(max_delta) < derivative_bound * 1.02 + 0.0001,
            "callback/loop discontinuity {max_delta}: {label}"
        );
    }
    for frame in stable.chunks_exact(channels) {
        if source_channels == 1 && channels >= 2 {
            assert_eq!(frame[0], frame[1], "mono routing: {label}");
        }
        assert!(
            frame.iter().skip(2).all(|&s| s == 0.0),
            "unassigned speaker output: {label}"
        );
    }
}

#[test]
fn common_dac_rates_preserve_pitch_channels_cursor_and_notifications() {
    for source_rate in SOURCE_RATES {
        for output_rate in OUTPUT_RATES {
            for source_channels in [1, 2] {
                for output_channels in [1, 2, 6] {
                    let label = format!(
                        "{source_rate}/{source_channels} -> {output_rate}/{output_channels}"
                    );
                    let control = AudioControl::new(false);
                    // As in a game, buffers can exist before the hardware opens.
                    let state = source(&control, source_rate, source_channels);
                    let mut reader = control
                        .stream()
                        .device_reader(output_rate, output_channels)
                        .unwrap();
                    let frames = output_rate as usize * 5 / 4;
                    let output = render(&mut reader, frames, output_channels);
                    check_waveform(
                        &output,
                        output_rate,
                        source_channels,
                        output_channels,
                        &label,
                    );
                    assert_eq!(
                        control.stream().metrics().missing_output_frames,
                        0,
                        "{label}"
                    );
                    let align = u32::from(source_channels) * 2;
                    let elapsed_us = frames as u64 * 1_000_000 / u64::from(output_rate);
                    let actual = state.cursor(elapsed_us, 1).unwrap() / align;
                    let expected = (frames as u64 * u64::from(source_rate) / u64::from(output_rate)
                        % u64::from(source_rate)) as u32;
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "cursor {actual} vs {expected}: {label}"
                    );
                    let events = control.take_events();
                    let notices: Vec<_> = events
                        .iter()
                        .filter_map(|event| match event {
                            AudioEvent::Notification {
                                source: 1,
                                event: 7,
                                due_us,
                                generation: 1,
                            } => Some(*due_us),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(notices.len(), 1, "notification count: {label}");
                    assert!(
                        (499_900..=506_000).contains(&notices[0]),
                        "notification time {:?}: {label}",
                        notices
                    );
                }
            }
        }
    }
}

#[test]
fn changing_dac_rate_keeps_the_source_position_and_playback_speed() {
    let control = AudioControl::new(false);
    let state = source(&control, 44_100, 2);
    let mut elapsed_frames = 0u64;
    // Includes rates below and above the source, and a nonstandard rate. The
    // implementation must accept validated device rates, not a DAC whitelist.
    for rate in [
        48_000, 192_000, 44_100, 384_000, 32_000, 352_800, 96_000, 88_200, 176_400, 44_117,
    ] {
        let mut reader = control.stream().device_reader(rate, 2).unwrap();
        let frames = rate as usize;
        let output = render(&mut reader, frames, 2);
        check_waveform(&output, rate, 2, 2, &format!("hot switch {rate}"));
        elapsed_frames += 44_100;
        let position = state
            .cursor(elapsed_frames * 1_000_000 / 44_100, 1)
            .unwrap()
            / 4;
        // Q32 advancement may remain one frame below the wrap; both sides are
        // adjacent positions. Reopening must not restart or skip source time.
        assert!(
            position <= 1 || position >= 44_098,
            "position after switch {rate}: {position}"
        );
        assert_eq!(control.stream().metrics().missing_output_frames, 0);
        drop(reader);
        control.maintain();
    }
}

#[test]
fn callback_partitioning_is_sample_identical_at_every_rate_ratio() {
    for source_rate in SOURCE_RATES {
        for output_rate in OUTPUT_RATES {
            let run = |partitioned: bool| {
                let control = AudioControl::new(false);
                source(&control, source_rate, 2);
                let mut reader = control.stream().device_reader(output_rate, 2).unwrap();
                if partitioned {
                    render(&mut reader, 12_345, 2)
                } else {
                    let mut output = vec![0.0; 12_345 * 2];
                    reader.read(&mut output, 20_000);
                    output
                }
            };
            assert_eq!(
                run(false),
                run(true),
                "partition changed samples: {source_rate} -> {output_rate}"
            );
        }
    }
}

#[test]
fn unsigned_eight_bit_pcm_has_no_dc_offset_at_different_dac_rates() {
    for rate in OUTPUT_RATES {
        let input_rate = 22_050;
        let control = AudioControl::new(false);
        let samples = (0..input_rate)
            .map(|i| {
                AtomicU8::new(
                    (128.0
                        + 40.0
                            * (f64::from(i) * std::f64::consts::TAU * 997.0
                                / f64::from(input_rate))
                            .sin())
                    .round() as u8,
                )
            })
            .collect();
        control
            .add_ring(
                1,
                RingConfig {
                    view: Arc::new(Pcm(samples)),
                    bytes: input_rate,
                    channels: 1,
                    bits: 8,
                    frequency: input_rate,
                    output_rate: 44_100,
                    notifications: vec![],
                },
            )
            .unwrap();
        assert!(control.written(1, 0, input_rate));
        assert!(control.play_ring(1, true, 1));
        let mut reader = control.stream().device_reader(rate, 2).unwrap();
        let output = render(&mut reader, rate as usize, 2);
        let stable: Vec<_> = output
            .as_chunks::<2>()
            .0
            .iter()
            .skip(rate as usize / 25)
            .map(|frame| {
                assert_eq!(frame[0], frame[1]);
                f64::from(frame[0])
            })
            .collect();
        let mean = stable.iter().sum::<f64>() / stable.len() as f64;
        let rms = (stable.iter().map(|s| s * s).sum::<f64>() / stable.len() as f64).sqrt();
        assert!(mean.abs() < 0.001, "8-bit DC offset {mean} at {rate}");
        assert!(
            (rms - 40.0 / 128.0 / 2.0f64.sqrt()).abs() < 0.001,
            "8-bit gain {rms} at {rate}"
        );
        assert_eq!(control.stream().metrics().missing_output_frames, 0);
    }
}

#[test]
fn sources_created_after_device_open_use_its_current_rate() {
    for output_rate in OUTPUT_RATES {
        let control = AudioControl::new(false);
        let mut reader = control.stream().device_reader(output_rate, 2).unwrap();
        source(&control, 44_100, 2);
        let output = render(&mut reader, output_rate as usize, 2);
        check_waveform(
            &output,
            output_rate,
            2,
            2,
            &format!("late source at {output_rate}"),
        );
        assert_eq!(control.stream().metrics().missing_output_frames, 0);
    }
}

#[test]
fn finite_pcm_keeps_its_resampler_tail_at_every_dac_rate() {
    for rate in OUTPUT_RATES {
        let run = |padding: usize| {
            let control = AudioControl::new(false);
            // A transient at the end of a 10 ms effect. The explicitly padded
            // reference has the same source samples and a known silent suffix.
            let samples = (0..441 + padding)
                .flat_map(|frame| {
                    let sample: i16 = if frame == 440 { 12_000 } else { 0 };
                    sample.to_le_bytes()
                })
                .map(AtomicU8::new)
                .collect::<Vec<_>>();
            let bytes = samples.len() as u32;
            control
                .add_ring(
                    1,
                    RingConfig {
                        view: Arc::new(Pcm(samples)),
                        bytes,
                        channels: 1,
                        bits: 16,
                        frequency: 44_100,
                        output_rate: 44_100,
                        notifications: vec![],
                    },
                )
                .unwrap();
            assert!(control.written(1, 0, bytes));
            assert!(control.play_ring(1, false, 1));
            let mut reader = control.stream().device_reader(rate, 2).unwrap();
            let output = render(&mut reader, rate as usize / 10, 2);
            assert_eq!(control.stream().metrics().missing_output_frames, 0);
            output
        };
        let short = run(0);
        let padded = run(1024);
        let error = short
            .iter()
            .zip(&padded)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            error < 0.000001,
            "finite source discarded the filter tail at {rate} Hz: {error}"
        );
    }
}

#[test]
fn queued_new_loop_does_not_report_the_previous_turn_before_it_becomes_audible() {
    for start_frame in [0, 4410] {
        let control = AudioControl::new(false);
        let mut reader = control.stream().device_reader(44_100, 2).unwrap();
        // The device already has 20 ms queued. Its rendered timeline is ahead
        // of the playback observation when a new/seeked voice is submitted.
        let mut silence = vec![0.0; 44_100 * 2];
        reader.read(&mut silence, 0);
        let state = source(&control, 44_100, 2);
        if start_frame != 0 {
            assert!(control.set_position(1, start_frame * 4));
        }
        let mut output = [0.0; 882];
        reader.read(&mut output, 20_000);
        assert_eq!(
            state.cursor(990_000, 1),
            Some(start_frame * 4),
            "unheard new data must not expose a position before this Play/Seek"
        );
        assert_eq!(state.cursor(1_005_000, 1), Some((start_frame + 220) * 4));
        // After a genuine full turn, extrapolating just behind a small modulo
        // position must still return the end of the previous turn.
        let mut later = vec![0.0; 44_100 * 2];
        reader.read(&mut later, 20_000);
        let expected = (start_frame + 43_659) % 44_100;
        assert_eq!(state.cursor(1_990_000, 1), Some(expected * 4));
    }
}
