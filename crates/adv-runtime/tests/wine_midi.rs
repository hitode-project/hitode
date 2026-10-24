//! Native MIDI stream scheduling and Wine MCI over the software synthesizer.
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

// spec: WINE-AUD-5, WINE-AUD-6, WINE-AUD-12
#[test]
fn midi_messages_streams_and_mci_produce_pcm() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-midi-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("midi.exe"), game.join("midi.exe")).unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join("midi.exe"),
        width: 64,
        height: 32,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session =
        Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |_| Ok(())).unwrap();
    let mut reader = session.audio_stream().device_reader(44100, 2).unwrap();
    // Keep consuming during SoundFont loading, as the host audio callback does.
    let consuming = Arc::new(AtomicBool::new(true));
    let running = consuming.clone();
    let consumer = std::thread::spawn(move || {
        let mut nonzero = 0;
        let mut deadline = Instant::now();
        while running.load(Ordering::Acquire) {
            let mut samples = [0.0; 442];
            reader.read(&mut samples, 0);
            assert!(samples.iter().all(|v| v.is_finite()));
            nonzero += samples.iter().filter(|&&v| v.abs() > 0.0001).count();
            deadline += Duration::from_secs_f64(221.0 / 44100.0);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        nonzero
    });
    let until = Instant::now() + Duration::from_secs(60);
    let stop = loop {
        let stop = session.run(Instant::now() + Duration::from_millis(5));
        if !matches!(stop, Stop::Paused) || Instant::now() >= until {
            break stop;
        }
    };
    consuming.store(false, Ordering::Release);
    let nonzero = consumer.join().unwrap();
    let native = session.machine.ext.get::<adv_runtime::wine::winmm::Stats>();
    assert_eq!(native.stream_events, 7);
    assert_eq!(native.stream_time_us, 500_000);
    assert_eq!(native.stream_headers, 1);
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("midi ok"), "{text}");
    assert!(nonzero > 1000, "no synthesized audio: {nonzero}");
    std::fs::remove_dir_all(root).unwrap();
}
