//! Native DirectMusic software synthesizer through Wine's dmusic and dmloader.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::{
    fs::File,
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

// spec: WINE-AUD-13
#[test]
fn dmusic_default_gm_collection_and_native_synth_produce_pcm() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-dmusic-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("dmusic.exe"), game.join("dmusic.exe")).unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&game.join("dmusic.exe")),
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
    let stats = adv_dx::dmsynth::stats(&session.machine);
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("dmusic ok"), "{text}");
    assert!(stats.downloads >= 1, "{stats:?}");
    assert!(stats.unloads >= 1, "{stats:?}");
    assert!(stats.short_events >= 2, "{stats:?}");
    assert!(stats.sysex_events >= 1, "{stats:?}");
    assert!(stats.rendered_frames > 0, "{stats:?}");
    assert!(
        nonzero > 1000,
        "no synthesized DirectMusic audio: {nonzero}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
