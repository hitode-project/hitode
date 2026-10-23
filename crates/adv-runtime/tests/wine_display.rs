//! A window shown through a UI's frame mailbox: DirectDraw blits text Wine's
//! GDI drew to the primary surface, and the frame reaches the mailbox while
//! the program waits for messages, with nothing reading it back.
//!
//! It needs `ADV_WINE_DIST` and `ADV_WINE_PROBES`, as the probe tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_device::surface::{Snapshot, SurfaceStore, frame_mailbox};
use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-GFX-7
#[test]
fn a_waiting_program_publishes_its_frame_to_the_ui() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-display-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("show.exe"), game.join("show.exe")).unwrap();
    let session_root = root.join("session");
    let config = Config {
        dist: &dist,
        prefix: &session_root,
        drive: None,
        exe: &game.join("show.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let (producer, mut consumer) = frame_mailbox();
    let ui = adv_sync::WakeEvent::new().unwrap();
    let published = ui.clone();
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, move |machine| {
        machine
            .ext
            .get_mut::<SurfaceStore>()
            .set_frame_mailbox(producer, published);
        machine.set_frame_handoff(false);
        Ok(())
    })
    .unwrap();
    // The probe never exits; it waits for messages once it has painted.
    let stop = session.run(Instant::now() + Duration::from_secs(5));
    drop(session);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Paused), "{stop:?}");
    assert!(consumer.update(), "no frame reached the mailbox");
    let mut frame = Snapshot::default();
    consumer.current().snapshot(&mut frame).unwrap();
    assert_eq!((frame.width, frame.height), (320, 120));
    let pixels = frame.bgra.as_chunks::<4>().0;
    let white = pixels.iter().filter(|p| p[..3] == [0xff; 3]).count();
    let yellow = pixels.iter().filter(|p| p[..3] == [0, 0xff, 0xff]).count();
    assert!(white > 100 && yellow > 50, "white {white}, yellow {yellow}");
}
