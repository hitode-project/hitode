//! Wine's own winmm and dsound feed a host-output reader in a headless session.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-AUD-1, WINE-AUD-2, WINE-AUD-3, WINE-AUD-4, WINE-THR-1
#[test]
fn wine_audio_reaches_the_host_stream() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-audio-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut fds = [0; 2];
    // SAFETY: fds holds both descriptors, and each gets one owner below.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join("audio.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    drop(stdout);
    let mut reader = session.audio_stream().device_reader(44100, 2).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pcm = Vec::new();
    let stop = loop {
        let stop = session.run(Instant::now() + Duration::from_millis(5));
        let mut samples = [0.0; 442];
        reader.read(&mut samples, 0);
        pcm.extend_from_slice(&samples);
        if !matches!(stop, Stop::Paused) || Instant::now() >= deadline {
            break stop;
        }
    };
    drop(reader);
    drop(session);
    // SAFETY: output owns this valid descriptor.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("midi_out=0 midi_in=0 aux=0"), "{text}");
    assert!(text.contains("render=1 capture=0"), "{text}");
    assert!(text.contains("wave_done=1 bytes=35280"), "{text}");
    assert!(text.contains("ds_advanced=1"), "{text}");
    // Long runs of both square-wave levels and periods, on both channels.
    for (level, half_period) in [(0.25, 50), (0.5, 25)] {
        let matches = pcm
            .as_chunks::<2>()
            .0
            .windows(half_period * 4)
            .filter(|window| {
                let sign = window[0][0].signum();
                window.iter().enumerate().all(|(i, frame)| {
                    let expected = level
                        * sign
                        * if (i / half_period) % 2 == 0 {
                            1.0
                        } else {
                            -1.0
                        };
                    (frame[0] - expected).abs() < 0.005 && (frame[1] - expected).abs() < 0.005
                })
            })
            .count();
        assert!(
            matches >= 10,
            "square wave {level}/{half_period}: {matches} matches in {} samples",
            pcm.len()
        );
    }
}
