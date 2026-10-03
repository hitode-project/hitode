use pal_audio::{AudioControl, ring::RingConfig};
use pal_cpu::SharedReadView;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
thread_local! {static COUNT:Cell<Option<(u64,u64)>>=const {Cell::new(None)};}
struct Allocator;
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.with(|count| {
            if let Some((a, d)) = count.get() {
                count.set(Some((a + 1, d)));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        COUNT.with(|count| {
            if let Some((a, d)) = count.get() {
                count.set(Some((a, d + 1)));
            }
        });
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        COUNT.with(|count| {
            if let Some((a, d)) = count.get() {
                count.set(Some((a + 1, d + 1)));
            }
        });
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
fn no_alloc(mut action: impl FnMut()) {
    COUNT.with(|count| count.set(Some((0, 0))));
    action();
    let result = COUNT.with(|count| count.replace(None)).unwrap();
    assert_eq!(result, (0, 0), "callback allocated or destroyed heap storage");
}
fn sine(rate: u32, frequency: f64, frames: usize) -> Arc<View> {
    let values = (0..frames)
        .flat_map(|i| {
            let sample =
                ((i as f64 * std::f64::consts::TAU * frequency / f64::from(rate)).sin() * 12000.0).round() as i16;
            sample.to_le_bytes()
        })
        .map(AtomicU8::new)
        .collect();
    Arc::new(View(values))
}
fn add(control: &AudioControl, source: u64, view: Arc<View>, rate: u32, notices: Vec<(u32, u32)>) {
    control
        .add_ring(
            source,
            RingConfig {
                bytes: view.len() as u32,
                view,
                channels: 1,
                bits: 16,
                frequency: rate,
                output_rate: 44100,
                notifications: notices,
            },
        )
        .unwrap();
}
struct View(Vec<AtomicU8>);
impl SharedReadView for View {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn load_u8(&self, index: usize) -> u8 {
        self.0.get(index).map_or(0, |v| v.load(Ordering::Relaxed))
    }
}
#[test]
fn direct_ring_equal_rate_is_continuous() {
    let control = AudioControl::new(false);
    let view = Arc::new(View((0..1000).map(|_| AtomicU8::new(192)).collect()));
    control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 1000,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 1000,
                notifications: vec![],
            },
        )
        .unwrap();
    control.play_ring(1, false, 1);
    control.sync_clock(0);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut out = [0.0; 20];
    reader.read(&mut out, 0);
    assert_eq!(out, [0.5; 20]);
}

#[test]
fn first_192khz_callback_and_loop_boundaries_preserve_waveform_without_allocating() {
    let control = AudioControl::new(false);
    // Create and Play before opening the 192k device: initial 44.1k converter
    // must be replaced off the callback, never used at the wrong speed.
    add(&control, 1, sine(44100, 1000.0, 44100), 44100, vec![(22050 * 2, 10)]);
    control.written(1, 0, 88200);
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(192000, 2).unwrap();
    let mut output = vec![0.0; 192000 * 2 * 3];
    no_alloc(|| {
        reader.read(&mut output, 22000);
    });
    let mono: Vec<f32> = output
        .chunks_exact(2)
        .map(|frame| {
            assert_eq!(frame[0], frame[1]);
            frame[0]
        })
        .collect();
    let stable = &mono[4096..];
    let rms = (stable.iter().map(|value| f64::from(*value).powi(2)).sum::<f64>() / stable.len() as f64).sqrt();
    assert!((rms - 12000.0 / 32768.0 / 2.0f64.sqrt()).abs() < 0.0001, "amplitude {rms}");
    let crossings = stable.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
    let measured = crossings as f64 * 192000.0 / stable.len() as f64;
    assert!((measured - 1000.0).abs() < 1.0, "pitch {measured}");
    let jump = stable.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0f32, f32::max);
    assert!(jump < 0.0121, "loop/resampler discontinuity {jump}");
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
}

#[test]
fn callback_chunk_sizes_do_not_repeat_drop_or_change_samples() {
    let render = |chunks: &[usize]| {
        let control = AudioControl::new(false);
        add(&control, 1, sine(44100, 713.0, 44100), 44100, vec![]);
        control.written(1, 0, 88200);
        control.play_ring(1, true, 1);
        let mut reader = control.stream().device_reader(192000, 2).unwrap();
        let mut output = vec![0.0; chunks.iter().sum::<usize>() * 2];
        let mut at = 0;
        no_alloc(|| {
            for &frames in chunks {
                reader.read(&mut output[at..at + frames * 2], 22000);
                at += frames * 2;
            }
        });
        output
    };
    let sizes = [4224, 1920, 2048, 1, 4096, 128, 65536, 128];
    let expected = render(&[sizes.iter().sum()]);
    assert_eq!(render(&sizes), expected);
}

#[test]
fn hot_switch_new_source_frequency_seek_release_and_owned_pcm_allocate_only_off_callback() {
    let control = AudioControl::new(false);
    add(&control, 1, sine(44100, 1000.0, 44100), 44100, vec![]);
    control.written(1, 0, 88200);
    control.play_ring(1, true, 1);
    let mut output = vec![0.0; 8192];
    let mut first = control.stream().device_reader(48000, 2).unwrap();
    no_alloc(|| {
        first.read(&mut output, 22000);
    });
    drop(first);
    let mut reader = control.stream().device_reader(192000, 2).unwrap();
    add(&control, 2, sine(22050, 400.0, 22050), 22050, vec![(0, 11), (22050, 12)]);
    control.written(2, 0, 44100);
    control.play_ring(2, true, 1);
    control.set_frequency(1, 22050).unwrap();
    control.set_position(1, 22050);
    control.set_stereo_gain(1, [0.25, 0.75]);
    no_alloc(|| {
        reader.read(&mut output, 10000);
    });
    control
        .push_pcm(pal_audio::mixer::PcmBlock {
            stream_id: 3,
            guest_pts_us: 0,
            sample_rate: 192000,
            channels: 1,
            samples: vec![0.01; 19200],
        })
        .unwrap_or_else(|_| panic!("PCM rejected"));
    control.end_pcm(3);
    control.stop_ring(1);
    control.remove_pcm(2);
    no_alloc(|| {
        for _ in 0..16 {
            reader.read(&mut output, 10000);
        }
    });
    control.maintain();
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert_eq!(control.stream().metrics().missing_output_frames, 0, "natural PCM tail is not underflow");
}

#[test]
fn stream_reuse_silences_old_generation_but_full_preloaded_static_loops_keep_their_tail() {
    let control = AudioControl::new(false);
    let view = Arc::new(View((0..900).map(|_| AtomicU8::new(192)).collect()));
    control
        .add_ring(
            1,
            RingConfig {
                view: view.clone(),
                bytes: 900,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![(250, 10), (550, 11), (850, 12)],
            },
        )
        .unwrap();
    control.written(1, 0, 300);
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut initial = [0.0; 400];
    reader.read(&mut initial, 0);
    control.written(1, 300, 300);
    reader.read(&mut initial, 0);
    control.written(1, 600, 300);
    reader.read(&mut initial, 0);
    control.stop_ring(1);
    control.set_position(1, 0);
    for byte in &view.0[..300] {
        byte.store(64, Ordering::Relaxed);
    }
    control.written(1, 0, 300);
    control.play_ring(1, true, 2);
    let mut output = [0.0; 1200];
    no_alloc(|| {
        reader.read(&mut output, 0);
    });
    assert_eq!(output[..600], [-0.5; 600]);
    assert_eq!(output[600..], [0.0; 600], "previous role tail must not enter the new stream generation");
    assert_eq!(control.stream().metrics().missing_output_frames, 300);
    drop(reader);

    let control = AudioControl::new(false);
    control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 900,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![(250, 10), (550, 11), (850, 12)],
            },
        )
        .unwrap();
    control.written(1, 0, 900);
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    control.written(1, 300, 100);
    let mut output = [0.0; 3600];
    no_alloc(|| {
        reader.read(&mut output, 0);
    });
    for turn in output.chunks_exact(1800) {
        assert_eq!(turn[..600], [-0.5; 600]);
        assert_eq!(turn[600..], [0.5; 1200]);
    }
    assert_eq!(control.stream().metrics().missing_output_frames, 0, "a static loop's retained bytes are valid");
}

#[test]
fn reopening_after_a_long_idle_gap_reanchors_time_without_skipping_the_new_voice() {
    let control = AudioControl::new(true);
    let view = Arc::new(View((0..1000).map(|_| AtomicU8::new(192)).collect()));
    control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 1000,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![],
            },
        )
        .unwrap();
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut output = [0.0; 20];
    reader.read(&mut output, 0);
    control.stop_ring(1);
    reader.read(&mut output, 0);
    drop(reader);
    let clock = control.playback_clock();
    clock.publish_device(5_000_000, 0);
    control.set_position(1, 0);
    control.play_ring(1, false, 2);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let result = reader.read(&mut output, 0);
    assert!(result.first_pts_us >= 5_000_000, "reopened buffer uses the current device timeline");
    assert_eq!(output, [0.5; 20], "host idle time must not advance the newly started source");
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
}

#[test]
fn saturated_commands_return_pcm_intact_and_pending_play_keeps_the_consumer_active() {
    let control = AudioControl::new(false);
    add(&control, 1, sine(44100, 1000.0, 44100), 44100, vec![]);
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(44100, 2).unwrap();
    let mut accepted = 0;
    for _ in 0..7000 {
        if control.set_gain(1, 0.5) {
            accepted += 1;
        }
    }
    assert!(accepted > 2048 && accepted < 7000, "bounded queue must report saturation");
    let block = pal_audio::mixer::PcmBlock {
        stream_id: 2,
        guest_pts_us: 0,
        sample_rate: 44100,
        channels: 1,
        samples: vec![0.125; 441],
    };
    let returned = control.push_pcm(block).expect_err("saturated PCM must be returned, not panicked or lost");
    assert_eq!(returned.samples, vec![0.125; 441]);
    let mut out = [0.0; 512];
    for _ in 0..5 {
        no_alloc(|| {
            reader.read(&mut out, 0);
        });
        control.maintain();
    }
    assert!(control.stop_ring(1));
    assert!(control.stream().active(), "pending Stop must keep command consumption alive");
    no_alloc(|| {
        reader.read(&mut out, 0);
    });
    assert!(!control.stream().active());
    assert!(control.play_ring(1, true, 2));
    assert!(control.stream().active(), "pending Play cannot be overwritten by consumer activity publication");
    no_alloc(|| {
        reader.read(&mut out, 0);
    });
    assert!(control.stream().active());
}

#[test]
fn cursor_snapshot_is_coherent_while_the_callback_publishes() {
    let control = AudioControl::new(false);
    let view = Arc::new(View((0..1000).map(|_| AtomicU8::new(192)).collect()));
    let state = control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 1000,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![],
            },
        )
        .unwrap();
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = done.clone();
    let writer = std::thread::spawn(move || {
        let mut out = [0.0; 2];
        for _ in 0..100000 {
            reader.read(&mut out, 0);
        }
        finished.store(true, Ordering::Release);
    });
    let mut snapshots = 0;
    while !done.load(Ordering::Acquire) {
        if let Some(cursor) = state.cursor(1_000_000, 1) {
            assert_eq!(cursor, 0, "position and timestamp must come from the same callback publication");
            snapshots += 1;
        }
    }
    writer.join().unwrap();
    assert!(snapshots > 100);
}

#[test]
fn one_hundred_twenty_nine_short_packets_get_backpressure_before_any_pcm_is_lost() {
    let control = AudioControl::new(false);
    for frame in 0..128 {
        assert!(
            control
                .push_pcm(pal_audio::mixer::PcmBlock {
                    stream_id: 1,
                    guest_pts_us: frame * 1000,
                    sample_rate: 1000,
                    channels: 1,
                    samples: vec![frame as f32 / 256.0]
                })
                .is_ok()
        );
    }
    let rejected = control
        .push_pcm(pal_audio::mixer::PcmBlock {
            stream_id: 1,
            guest_pts_us: 128000,
            sample_rate: 1000,
            channels: 1,
            samples: vec![0.5],
        })
        .expect_err("129th packet must be returned before callback queue capacity is exceeded");
    assert_eq!(rejected.samples, vec![0.5]);
    control.end_pcm(1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut output = [0.0; 256];
    no_alloc(|| {
        reader.read(&mut output, 0);
    });
    for (frame, values) in output.chunks_exact(2).enumerate() {
        assert_eq!(values, [frame as f32 / 256.0; 2]);
    }
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
}

#[test]
fn native_consumed_clock_does_not_treat_the_writable_buffer_as_already_played() {
    let control = AudioControl::new(true);
    let view = Arc::new(View((0..1000).map(|_| AtomicU8::new(192)).collect()));
    let state = control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 1000,
                channels: 1,
                bits: 8,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![],
            },
        )
        .unwrap();
    control.set_position(1, 50);
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut output = [0.0; 20];
    let mut first = None;
    no_alloc(|| {
        first = Some(reader.read_with_clock(
            &mut output,
            90000,
            Some(pal_audio::DeviceClock { consumed_frames: 20, queued_frames: 30 }),
        ));
    });
    assert_eq!(first.unwrap().first_pts_us, 50000, "first write is after existing hardware padding");
    assert_eq!(state.cursor(20000, 1), Some(50), "samples scheduled in the future have not advanced the play cursor");
    assert_eq!(state.cursor(55000, 1), Some(55));
    no_alloc(|| {
        reader.read_with_clock(
            &mut output,
            90000,
            Some(pal_audio::DeviceClock { consumed_frames: 30, queued_frames: 30 }),
        );
    });
    assert_eq!(state.cursor(65000, 1), Some(65));
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
    let mut looped = vec![0.0; 4000];
    no_alloc(|| {
        reader.read_with_clock(
            &mut looped,
            90000,
            Some(pal_audio::DeviceClock { consumed_frames: 40, queued_frames: 30 }),
        );
    });
    assert_eq!(state.cursor(1_165_000, 1), Some(165), "absolute phase must still wrap after actual loop consumption");
}

#[test]
fn an_ongoing_loop_advances_across_a_device_gap_while_a_new_play_starts_at_its_own_anchor() {
    let control = AudioControl::new(true);
    let view = Arc::new(View((0..1000i16).flat_map(i16::to_le_bytes).map(AtomicU8::new).collect()));
    control
        .add_ring(
            1,
            RingConfig {
                view,
                bytes: 2000,
                channels: 1,
                bits: 16,
                frequency: 1000,
                output_rate: 44100,
                notifications: vec![],
            },
        )
        .unwrap();
    control.play_ring(1, true, 1);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let mut out = [0.0; 20];
    reader.read(&mut out, 0);
    drop(reader);
    control.playback_clock().publish_device(250_000, 0);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    let first = reader.read(&mut out, 0);
    let expected = (first.first_pts_us / 1000 % 1000) as f32 / 32768.0;
    assert!(
        (out[0] - expected).abs() <= 1.0 / 32768.0,
        "ongoing source phase must follow the device timeline across loss: {} expected {expected}",
        out[0]
    );
    control.stop_ring(1);
    reader.read(&mut out, 0);
    drop(reader);
    control.playback_clock().publish_device(5_250_000, 0);
    control.set_position(1, 80);
    control.play_ring(1, false, 2);
    let mut reader = control.stream().device_reader(1000, 2).unwrap();
    no_alloc(|| {
        reader.read(&mut out, 0);
    });
    assert!(
        (out[0] - 40.0 / 32768.0).abs() <= 1.0 / 32768.0,
        "new Play follows its seek anchor instead of skipping the old five-second idle gap"
    );
}

fn owned_output(
    rate: u32,
    channels: u16,
    source_rate: u32,
    source_channels: u16,
    samples: &[f32],
    packets: bool,
) -> Vec<f32> {
    let control = AudioControl::new(false);
    for (index, block) in
        samples.chunks(if packets { 1024 * usize::from(source_channels) } else { samples.len() }).enumerate()
    {
        let first_frame = if packets { index * 1024 } else { 0 };
        let pts = first_frame as u64 * 1000 / u64::from(source_rate) * 1000;
        control
            .push_pcm(pal_audio::mixer::PcmBlock {
                stream_id: 1,
                guest_pts_us: pts,
                sample_rate: source_rate,
                channels: source_channels,
                samples: block.to_vec(),
            })
            .unwrap_or_else(|_| panic!("owned PCM rejected"));
    }
    control.end_pcm(1);
    let mut reader = control.stream().device_reader(rate, channels).unwrap();
    let frames = (samples.len() as u64 / u64::from(source_channels) * u64::from(rate) / u64::from(source_rate))
        as usize
        + rate as usize / 10;
    let mut out = vec![0.0; frames * usize::from(channels)];
    let mut at = 0;
    for size in [1, 127, 29, 4096, 73, 819, 4224].into_iter().cycle() {
        let end = (at + size * usize::from(channels)).min(out.len());
        no_alloc(|| {
            reader.read(&mut out[at..end], 0);
        });
        at = end;
        if at == out.len() {
            break;
        }
    }
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
    out
}

#[test]
fn owned_pcm_rounded_timestamps_join_and_real_overlap_is_trimmed() {
    let control = AudioControl::new(false);
    for (pts, value, frames) in [(0, 0.25, 100), (99400, 0.5, 100), (150000, 0.75, 100), (100000, 1.0, 10)] {
        control
            .push_pcm(pal_audio::mixer::PcmBlock {
                stream_id: 1,
                guest_pts_us: pts,
                sample_rate: 1000,
                channels: 1,
                samples: vec![value; frames],
            })
            .unwrap_or_else(|_| panic!("PCM rejected"));
    }
    control.end_pcm(1);
    let mut reader = control.stream().device_reader(1000, 1).unwrap();
    let mut out = [0.0; 300];
    no_alloc(|| {
        reader.read(&mut out, 0);
    });
    assert_eq!(&out[..100], &[0.25; 100]);
    assert_eq!(&out[100..200], &[0.5; 100]);
    assert_eq!(&out[200..250], &[0.75; 50]);
    assert_eq!(&out[250..], &[0.0; 50]);
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
    // Fully covered duplicate blocks also return both slot and duration credits.
    control
        .push_pcm(pal_audio::mixer::PcmBlock {
            stream_id: 1,
            guest_pts_us: 300000,
            sample_rate: 1000,
            channels: 1,
            samples: vec![0.0; 2000],
        })
        .unwrap_or_else(|_| panic!("drained owned credits leaked"));
}

#[test]
fn owned_pcm_rounded_packets_match_one_block_at_all_output_rates() {
    for source_rate in [22050, 44100, 48000] {
        for source_channels in [1, 2] {
            let samples: Vec<_> = (0..16384)
                .flat_map(|frame| {
                    (0..source_channels).map(move |channel| {
                        (frame as f64 * std::f64::consts::TAU * 997.0 / f64::from(source_rate) + f64::from(channel))
                            .sin() as f32
                            * 0.3
                    })
                })
                .collect();
            for rate in [32000, 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000] {
                for channels in [1, 2, 6] {
                    let expected = owned_output(rate, channels, source_rate, source_channels, &samples, false);
                    let actual = owned_output(rate, channels, source_rate, source_channels, &samples, true);
                    let delta = actual.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    assert!(
                        delta < 0.00001,
                        "source={source_rate}/{source_channels} output={rate}/{channels} packet delta={delta}"
                    );
                }
            }
        }
    }
}

#[test]
fn owned_finite_pcm_keeps_the_resampler_tail() {
    for rate in [32000, 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000] {
        let mut samples = vec![0.0; 441];
        samples[440] = 0.5;
        let actual = owned_output(rate, 2, 44100, 1, &samples, false);
        samples.resize(1465, 0.0);
        let expected = owned_output(rate, 2, 44100, 1, &samples, false);
        let delta = actual.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(delta < 0.000001, "finite owned tail at {rate}Hz differs by {delta}");
    }
}

#[test]
fn owned_finite_source_stays_active_until_its_filter_tail_is_drained() {
    let control = AudioControl::new(false);
    let mut samples = vec![0.0; 441];
    samples[440] = 0.5;
    control
        .push_pcm(pal_audio::mixer::PcmBlock {
            stream_id: 1,
            guest_pts_us: 0,
            sample_rate: 44100,
            channels: 1,
            samples,
        })
        .unwrap_or_else(|_| panic!("PCM rejected"));
    control.end_pcm(1);
    let mut reader = control.stream().device_reader(192000, 1).unwrap();
    let mut head = vec![0.0; 1921];
    no_alloc(|| {
        reader.read(&mut head, 0);
    });
    assert!(control.active(), "logical EOF must not pause the device before the sinc tail");
    let mut tail = vec![0.0; 19200];
    no_alloc(|| {
        reader.read(&mut tail, 0);
    });
    assert!(tail.iter().any(|sample| sample.abs() > 0.001));
    assert!(!control.active(), "finite tail must terminate");
    assert_eq!(control.stream().metrics().missing_output_frames, 0);
}

#[test]
fn owned_real_timestamp_gap_counts_missing_but_silent_pcm_and_natural_end_do_not() {
    let control = AudioControl::new(false);
    for (pts, value) in [(0, 0.0), (300000, 0.5)] {
        control
            .push_pcm(pal_audio::mixer::PcmBlock {
                stream_id: 1,
                guest_pts_us: pts,
                sample_rate: 1000,
                channels: 1,
                samples: vec![value; 100],
            })
            .unwrap_or_else(|_| panic!("PCM rejected"));
    }
    control.end_pcm(1);
    let mut reader = control.stream().device_reader(1000, 1).unwrap();
    let mut out = [0.0; 500];
    no_alloc(|| {
        reader.read(&mut out, 0);
    });
    assert_eq!(&out[..300], &[0.0; 300]);
    assert_eq!(&out[300..400], &[0.5; 100]);
    assert_eq!(&out[400..], &[0.0; 100]);
    assert_eq!(control.stream().metrics().missing_stream_frames, 200);
    assert_eq!(control.stream().metrics().missing_output_frames, 200);
}
