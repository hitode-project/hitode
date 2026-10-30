//! Native WinMM callbacks and host-clock buffer consumption.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::{
    session::{Config, Session, Stop},
    winmm::Stats,
};
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

// spec: WINE-AUD-8, WINE-AUD-9, WINE-AUD-10, WINE-AUD-11
#[test]
fn native_winmm_callbacks_sounds_and_forwarding() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-winmm-{}", std::process::id()));
    let game = root.join("probe");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(
        probes.join("winmm_native.exe"),
        game.join("winmm_native.exe"),
    )
    .unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join("winmm_native.exe"),
        width: 64,
        height: 32,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session =
        Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |_| Ok(())).unwrap();
    let system = root.join("prefix/drive_c/windows/system32");
    let exports = |name| {
        adv_pe::Pe::parse(&std::fs::read(system.join(name)).unwrap())
            .unwrap()
            .exports()
            .unwrap()
            .into_iter()
            .map(|e| (e.ordinal, e.name))
            .collect::<Vec<_>>()
    };
    assert_eq!(exports("winmm.dll"), exports("winewinmm.dll"));
    let mut reader = session.audio_stream().device_reader(44100, 2).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let consuming = running.clone();
    let consumer = std::thread::spawn(move || {
        let mut pcm = Vec::new();
        let mut deadline = Instant::now();
        while consuming.load(Ordering::Acquire) {
            let mut block = [0.0; 442];
            reader.read(&mut block, 0);
            pcm.extend_from_slice(&block);
            deadline += Duration::from_secs_f64(221.0 / 44100.0);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        pcm
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let stop = loop {
        let stop = session.run(Instant::now() + Duration::from_millis(5));
        if !matches!(stop, Stop::Paused) || Instant::now() >= deadline {
            break stop;
        }
    };
    running.store(false, Ordering::Release);
    let pcm = consumer.join().unwrap();
    let stats = *session.machine.ext.get::<Stats>();
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("native winmm ok"), "{text}");
    assert!(
        stats.wave_headers >= 34,
        "{} native headers",
        stats.wave_headers
    );
    assert!(
        stats.wave_completed >= 26,
        "{} completions",
        stats.wave_completed
    );
    assert_eq!(stats.sounds, 9);
    assert!(stats.wave_completed_while_blocked >= 1);
    // Each queue has three authored levels, with both signs on both channels.
    for level in [0.125, 0.25, 0.375] {
        for sign in [-1.0, 1.0] {
            let frames = pcm
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|f| {
                    (f[0] - level * sign).abs() < 0.005 && (f[1] - level * sign).abs() < 0.005
                })
                .count();
            assert!(frames > 500, "{level} / {sign}: {frames} frames");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
