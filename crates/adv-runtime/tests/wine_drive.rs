//! Drive C: may live outside the prefix, as on iOS, where the player keeps it
//! in Documents: the session installs its host modules there.
//!
//! It needs `ADV_WINE_DIST` and `ADV_WINE_PROBES`, as the probe tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-SES-1
#[test]
fn host_modules_go_to_a_drive_c_outside_the_prefix() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-drive-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("identity.exe"), game.join("identity.exe")).unwrap();
    let (prefix, drive) = (root.join("session"), root.join("C"));
    let config = Config {
        dist: &dist,
        prefix: &prefix,
        drive: Some(&drive),
        locale: 0x409,
        fonts: "",
        exe: &game.join("identity.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    let system = drive.join("windows/system32");
    let installed = ["winmm.dll", "dsound.dll", "ddraw.dll", "wineadv.drv"]
        .map(|name| std::fs::symlink_metadata(system.join(name)).map(|m| m.is_file()));
    let inside = prefix.join("drive_c").exists();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}");
    assert!(
        installed.iter().all(|m| matches!(m, Ok(true))),
        "{installed:?}"
    );
    assert!(!inside);
}
