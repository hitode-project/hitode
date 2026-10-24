//! Package probes through Wine's PE modules in a headless session.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-NT-401, WINE-NT-402, WINE-NT-403, WINE-NT-404
#[test]
fn m_system() {
    // The in-process Wine server has global state; packages run in separate processes.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["m_system_probe", "--exact", "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// spec: WINE-NT-401, WINE-NT-402, WINE-NT-403, WINE-NT-404
#[test]
#[ignore = "run by m_system in a separate process"]
fn m_system_probe() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-nt-m-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut fds = [0; 2];
    // SAFETY: fds holds both newly owned descriptors.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let exe = probes.join("nt_m_system.exe");
    let config = Config {
        dist: &dist,
        prefix: &root,
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
    let mut session =
        Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |_| Ok(())).unwrap();
    drop(stdout);
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    // SAFETY: output owns the read descriptor; the guest has stopped writing.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
    print!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    for check in [
        "create-section-ex",
        "map-view-ex-shared",
        "map-view-ex-alignment",
        "prefetch-memory",
        "shared-virtual-lock",
        "shared-virtual-unlock",
        "unmap-view-ex",
        "map-view-ex-placeholder",
        "unmap-preserve-placeholder",
        "flush-view",
        "flush-file-data",
        "virtual-lock",
        "virtual-unlock",
        "flush-write-buffers",
        "execution-state-set",
        "execution-state-previous",
        "set-default-locale",
        "system-locale-isolation",
        "set-system-locale",
        "user-locale-isolation",
        "license-size",
        "license-registry-data",
        "wow64-allocate-validation",
        "wow64-allocate",
        "wow64-query-process",
        "wow64-read-empty",
        "wow64-write-empty",
        "wow64-read-access",
        "wow64-write-access",
        "debug-attach-self",
        "debug-kill-on-exit",
        "debug-continue-invalid",
        "debug-detach-unattached",
        "debug-wait-timeout",
    ] {
        assert!(
            text.lines().any(|line| line == format!("{check}: ok")),
            "{check}: {text}"
        );
    }
}
