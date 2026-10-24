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
    let stop = session.run(Instant::now() + Duration::from_secs(60));
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

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-3, WINE-MEDIA-6
#[test]
fn windows_media_samples() {
    probe("wm", "media wm ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2
#[test]
fn media_foundation_samples() {
    probe("mf", "media mf ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-6
#[test]
fn mci_runs_to_completion() {
    probe("mci", "media mci ok");
}

// spec: WINE-MEDIA-1, WINE-MEDIA-2, WINE-MEDIA-6
#[test]
fn directshow_samples() {
    probe("dshow", "media dshow ok");
}
