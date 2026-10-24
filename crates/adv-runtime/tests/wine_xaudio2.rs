//! Native XAudio2 through Wine's loader, COM and guest callbacks, headlessly.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::{
    fs::File,
    os::fd::AsRawFd,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn play(version: u32) {
    // Wine's server can start only once per host process.
    let thread = std::thread::current();
    let case = thread.name().unwrap();
    if !std::env::args().any(|arg| arg == case) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([case, "--exact", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root =
        std::env::temp_dir().join(format!("adv-wine-xaudio-{version}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let exe = probes.join(format!("xaudio2{version}.exe"));
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &exe,
        width: 64,
        height: 32,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session =
        Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |_| Ok(())).unwrap();
    let mut reader = session.audio_stream().device_reader(44100, 2).unwrap();
    let consuming = Arc::new(AtomicBool::new(true));
    let running = consuming.clone();
    let consumer = std::thread::spawn(move || {
        let mut pcm = Vec::new();
        let mut deadline = Instant::now();
        while running.load(Ordering::Acquire) {
            let mut samples = [0.0; 442];
            reader.read(&mut samples, 0);
            pcm.extend_from_slice(&samples);
            deadline += Duration::from_secs_f64(221.0 / 44100.0);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        pcm
    });
    let until = Instant::now() + Duration::from_secs(60);
    let stop = loop {
        let stop = session.run(Instant::now() + Duration::from_millis(5));
        if !matches!(stop, Stop::Paused) || Instant::now() >= until {
            break stop;
        }
    };
    consuming.store(false, Ordering::Release);
    let pcm = consumer.join().unwrap();
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("xaudio ok"), "{text}");
    assert!(pcm.iter().all(|v| v.is_finite()));
    for (level, period) in [(0.125f32, 50), (0.25, 25)] {
        let matched = pcm
            .as_chunks::<2>()
            .0
            .windows(period * 4)
            .filter(|window| {
                let sign = window[0][0].signum();
                window.iter().enumerate().all(|(i, frame)| {
                    let expected = level * sign * if (i / period) % 2 == 0 { 1.0 } else { -1.0 };
                    (frame[0] - expected).abs() < 0.001 && (frame[1] - expected).abs() < 0.001
                })
            })
            .count();
        assert!(
            matched >= 10,
            "{level}/{period}: {matched} matching runs in {} samples",
            pcm.len()
        );
    }
    let constants = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|frame| (frame[0] - 0.375).abs() < 0.001 && (frame[1] - 0.375).abs() < 0.001)
        .count();
    assert!(constants > 10000, "meter and fallback PCM: {constants}");
    if version == 9 {
        let fallback = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|frame| (frame[0] - 0.5625).abs() < 0.001 && (frame[1] - 0.5625).abs() < 0.001)
            .count();
        assert!(
            fallback > 1000,
            "overlapping transferred and foreign-effect voices: {fallback}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WINE-XAUDIO-1, WINE-XAUDIO-2, WINE-XAUDIO-3, WINE-XAUDIO-4
#[test]
fn xaudio27_com_graph_and_callbacks() {
    play(7);
}

// spec: WINE-XAUDIO-1, WINE-XAUDIO-2, WINE-XAUDIO-3, WINE-XAUDIO-4, WINE-XAUDIO-5
#[test]
fn xaudio29_graph_callbacks_and_effect_fallback() {
    play(9);
}

// spec: WINE-XAUDIO-1, WINE-XAUDIO-2, WINE-XAUDIO-3, WINE-XAUDIO-4
#[test]
fn xaudio28_graph_and_callbacks() {
    play(8);
}
