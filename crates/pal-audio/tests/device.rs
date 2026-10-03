#![cfg(feature = "device")]
//! Opt-in device lifecycle check. Uses a quiet, generated 440 Hz sine only.
use pal_audio::Output;
use pal_audio::mixer::PcmBlock;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[test]
#[ignore = "opens the host audio device and emits a generated quiet sine"]
fn real_device_stops_callbacks_while_silent_and_restarts() {
    let mixer = pal_audio::AudioControl::new(true);
    let mut output = Output::new(mixer.stream());
    let metrics = output.metrics();
    output.update();
    assert!(!output.is_running());
    let samples: Vec<f32> =
        (0..4800).map(|i| (i as f32 * std::f32::consts::TAU * 440.0 / 48000.0).sin() * 0.02).collect();
    for pts in [0, 1_000_000] {
        {
            let m = &mixer;
            m.sync_clock(pts);
            assert!(
                m.push_pcm(PcmBlock {
                    stream_id: 1,
                    guest_pts_us: pts,
                    sample_rate: 48000,
                    channels: 1,
                    samples: samples.clone()
                })
                .is_ok()
            );
            m.end_pcm(1);
        }
        output.update();
        assert!(output.is_running(), "default output device must open");
        std::thread::sleep(Duration::from_millis(250));
        output.update();
        assert!(!output.is_running());
        let before = metrics.callbacks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(before, metrics.callbacks.load(Ordering::Relaxed), "silent output must have zero callbacks");
    }
    assert_eq!(metrics.starts.load(Ordering::Relaxed), 2);
    assert_eq!(metrics.stops.load(Ordering::Relaxed), 2);
    assert!(metrics.non_silent_samples.load(Ordering::Relaxed) > 0);
    println!(
        "callbacks={} non_silent_samples={} lock_misses={} starts=2 stops=2 silent_callbacks=0",
        metrics.callbacks.load(Ordering::Relaxed),
        metrics.non_silent_samples.load(Ordering::Relaxed),
        metrics.lock_misses.load(Ordering::Relaxed)
    );
}
