//! A session's prefix comes from the template and its identity is fixed: a
//! program sees the template's user and the virtual machine, never the host.
//!
//! It needs `ADV_WINE_DIST` and `ADV_WINE_PROBES`, as the probe tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-SES-3, WINE-SES-4
#[test]
fn a_session_sees_the_template_user_and_the_virtual_machine() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-prefix-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("identity.exe"), game.join("identity.exe")).unwrap();

    let mut fds = [0; 2];
    // SAFETY: fds has room for both ends.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: both descriptors are new and owned by nobody else.
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let session_root = root.join("session");
    let config = Config {
        dist: &dist,
        prefix: &session_root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join("identity.exe"),
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
    let user_reg = std::fs::read_to_string(session_root.join("user.reg")).unwrap();

    // SAFETY: output owns the read end of the pipe.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    for expected in [
        "USERNAME=Player",
        r"USERPROFILE=C:\users\Player",
        r"APPDATA=C:\users\Player\AppData\Roaming",
        "COMPUTERNAME=HITODE",
        r"ProgramFiles=C:\Program Files",
        r"ALLUSERSPROFILE=C:\ProgramData",
        r"SystemRoot=C:\windows",
        "user=Player",
        "computer=HITODE",
        "bios=Hitode",
        "product=Hitode Virtual Machine",
        "processor=ADV virtual x86 processor",
    ] {
        assert!(
            lines.contains(&expected),
            "{expected} missing from:\n{text}"
        );
    }
    // The template selects the session's audio driver.
    assert!(user_reg.contains(r#""Audio"="advaudio""#));
    let host = std::env::var("USER").unwrap_or_default();
    if !host.is_empty() && host != "Player" {
        assert!(!text.contains(&host), "the host user leaked:\n{text}");
    }
}
