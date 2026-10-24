//! Wine's PE media APIs over host Unix tables, with synthetic FFmpeg fixtures.
#![cfg(all(target_vendor = "apple", feature = "media"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn probe(name: &str, expected: &str) {
    // wineserver owns C globals and can run only once in a host process.
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
        std::env::var_os("ADV_WINE_DIST"),
        std::env::var_os("ADV_WINE_PROBES"),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-media-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join(format!("media_{name}.exe"));
    std::fs::copy(
        PathBuf::from(probes).join(format!("media_{name}.exe")),
        &exe,
    )
    .unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
    for name in [
        "synthetic.mpg",
        "synthetic.wmv",
        "synthetic.mp4",
        "synthetic.avi",
    ] {
        std::fs::copy(fixtures.join(name), root.join(name)).unwrap();
    }
    let mut fds = [0; 2];
    // SAFETY: two fresh descriptors, transferred once to File ownership.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let dist = PathBuf::from(dist);
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &exe,
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    drop(stdout);
    let until = Instant::now() + Duration::from_secs(60);
    let mut pixels = false;
    let stop = loop {
        let stop = session.run_ui(until);
        if name == "video" {
            let store = session
                .machine
                .ext
                .get::<adv_device::surface::SurfaceStore>();
            if let Some(frame) = store.latest_frame() {
                for (_, layer) in frame.video_layers() {
                    if layer.frame.bgra[2] > 230 {
                        assert_eq!((layer.destination.x, layer.destination.y), (28, 32));
                        let mut snapshot = adv_device::surface::Snapshot::default();
                        frame.snapshot(&mut snapshot).unwrap();
                        let pixel = ((32 * snapshot.width + 28) * 4) as usize;
                        assert!(snapshot.bgra[pixel] < 12 && snapshot.bgra[pixel + 2] > 230);
                        let outside = ((31 * snapshot.width + 28) * 4) as usize;
                        assert_ne!(
                            &snapshot.bgra[outside..outside + 3],
                            &snapshot.bgra[pixel..pixel + 3]
                        );
                        pixels = true;
                    }
                }
            }
        }
        if !matches!(stop, Stop::Paused) || Instant::now() >= until {
            break stop;
        }
    };
    if matches!(stop, Stop::Exited(0)) {
        match name {
            "dshow" | "mci" | "video" => {
                let stats = session.machine.ext.get::<adv_dx::dshow::Playback>();
                if name != "dshow" {
                    assert!(stats.audio_frames > 0, "native graph decoded no audio");
                }
                // Real-time playback coalesces late frames; the video case
                // checks the delivered pixels and geometry above.
                assert!(
                    stats.sources > 0 && stats.video_frames > 0 && stats.completions > 0,
                    "native graph: sources={} video={} completed={}",
                    stats.sources,
                    stats.video_frames,
                    stats.completions
                );
            }
            "wm" => {
                let stats = session.machine.ext.get::<adv_dx::wmvcore::NativeSamples>();
                assert_eq!(stats.readers, 1);
                assert_eq!(stats.samples, 31);
            }
            _ => {}
        }
        if name == "video" {
            assert!(pixels, "no presented native video pixels");
        }
    }
    drop(session);
    // SAFETY: output owns this descriptor; a failed guest may leave a write alias.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains(expected), "{text}");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-3, WINE-MEDIA-6, WINE-MEDIA-9
#[test]
fn windows_media_samples() {
    probe("wm", "media wm ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2
#[test]
fn media_foundation_samples() {
    probe("mf", "media mf ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-6, WINE-MEDIA-8, WINE-MEDIA-11
#[test]
fn mci_runs_to_completion() {
    probe("mci", "media mci ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-6, WINE-MEDIA-8
#[test]
fn directshow_samples() {
    probe("dshow", "media dshow ok");
}

// spec: WINE-MEDIA-8, WINE-MEDIA-10, WINE-MEDIA-11
#[test]
fn native_video_at_client_position() {
    probe("video", "media video ok");
}

// spec: WINE-MEDIA-12
#[test]
fn original_quartz_class_and_exports() {
    probe("fallback", "media fallback ok");
}
