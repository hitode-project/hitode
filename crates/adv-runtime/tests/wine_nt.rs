//! The file-system NT package through Wine's Win32 entry points.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-NT-101, WINE-NT-102, WINE-NT-103, WINE-THR-1
#[test]
fn j_file_calls() {
    // Each package gets a process: the in-process Wine server is global.
    let child_args = ["j_file_calls", "--exact", "--nocapture"];
    if !std::env::args().skip(1).eq(child_args) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(child_args)
            .status()
            .unwrap();
        assert!(status.success(), "J-file probe failed: {status}");
        return;
    }
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-nt-j-file-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("nt_j_file.exe"), game.join("nt_j_file.exe")).unwrap();
    let mut fds = [0; 2];
    // SAFETY: fds holds both descriptors, and each gets one owner below.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let config = Config {
        dist: &dist,
        prefix: &root.join("session"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join("nt_j_file.exe"),
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
    // SAFETY: output owns the pipe's read descriptor.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
    eprintln!("{text}");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    for check in [
        "flush_file",
        "flush_ex",
        "flush_access",
        "lock",
        "lock_conflict",
        "lock_wait",
        "unlock",
        "lock_woken",
        "delete",
        "mailslot",
        "notify_filter",
        "notify",
        "cancel",
        "cancel_ex",
        "cancel_ex_result",
        "cancel_sync",
        "gather",
        "scatter",
        "device_invalid",
        "device_disk",
        "device_server",
        "flush_pipe",
        "pipe_read",
    ] {
        assert!(
            text.lines().any(|line| line == format!("{check}=ok")),
            "{check}: {text}"
        );
    }
}
