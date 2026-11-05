//! A window shown to the UI: DirectDraw blits text Wine's GDI drew to the
//! primary surface, and the frame reaches the UI while the program waits for
//! messages, with nothing reading it back. GPU builds show software frames on
//! the native display; other builds hand them to the UI's frame mailbox.
//!
//! It needs `ADV_WINE_DIST`, as the probe tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::time::{Duration, Instant};

use adv_device::surface::{Snapshot, SurfaceStore};
use adv_runtime::wine::session::{Config, Session, Stop};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

/// What the UI receives from a session.
struct Ui {
    #[cfg(feature = "gpu")]
    capture: adv_device::gpu::test_support::SurfaceCapture,
    #[cfg(not(feature = "gpu"))]
    mailbox: (
        Option<adv_device::surface::FrameProducer>,
        adv_device::surface::FrameConsumer,
    ),
}
impl Ui {
    /// Before the session starts, so the capture precedes its GPU facades.
    fn new() -> Self {
        Self {
            #[cfg(feature = "gpu")]
            capture: adv_device::gpu::test_support::SurfaceCapture::new().unwrap(),
            #[cfg(not(feature = "gpu"))]
            mailbox: {
                let (producer, consumer) = adv_device::surface::frame_mailbox();
                (Some(producer), consumer)
            },
        }
    }
    /// From the session's `prepare`.
    fn attach(&mut self, machine: &mut adv_machine::Machine<adv_cpu::Cpu>) {
        #[cfg(not(feature = "gpu"))]
        machine.ext.get_mut::<SurfaceStore>().set_frame_mailbox(
            self.mailbox.0.take().unwrap(),
            adv_sync::WakeEvent::new().unwrap(),
        );
        machine.set_frame_handoff(false);
    }
    /// The latest frame the foreground process showed, if any.
    fn frame(&mut self, session: &mut Session) -> Option<Snapshot> {
        let mut frame = Snapshot::default();
        #[cfg(feature = "gpu")]
        {
            let store = session.machine.ext.get_mut::<SurfaceStore>();
            self.capture.update(store);
            std::mem::swap(&mut frame, &mut store.presenter);
        }
        #[cfg(not(feature = "gpu"))]
        {
            let _ = session;
            let consumer = &mut self.mailbox.1;
            if consumer.update() {
                consumer.current().snapshot(&mut frame).unwrap();
            }
        }
        (frame.frames > 0).then_some(frame)
    }
}

// spec: PROC-DEV-1
#[test]
fn a_child_inherits_the_ui_mailbox_when_its_launcher_exits() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "display_child_session",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn display_child_session() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-display-child-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join("display_child.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let mut ui = Ui::new();
    let mut session = Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |machine| {
        ui.attach(machine);
        Ok(())
    })
    .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(5));
    assert!(matches!(stop, Stop::Paused), "{stop:?}");
    let frame = ui
        .frame(&mut session)
        .expect("child frame did not reach the UI");
    assert_eq!((frame.width, frame.height), (80, 40));
    assert!(
        frame
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [255; 3])
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WINE-GFX-7
#[test]
fn a_waiting_program_publishes_its_frame_to_the_ui() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
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
        locale: 0x409,
        fonts: "",
        exe: &game.join("show.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let mut ui = Ui::new();
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |machine| {
        ui.attach(machine);
        Ok(())
    })
    .unwrap();
    // The probe never exits; it waits for messages once it has painted.
    let stop = session.run(Instant::now() + Duration::from_secs(5));
    assert!(matches!(stop, Stop::Paused), "{stop:?}");
    let frame = ui.frame(&mut session).expect("no frame reached the UI");
    drop(session);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!((frame.width, frame.height), (320, 120));
    let pixels = frame.bgra.as_chunks::<4>().0;
    let white = pixels.iter().filter(|p| p[..3] == [0xff; 3]).count();
    let yellow = pixels.iter().filter(|p| p[..3] == [0, 0xff, 0xff]).count();
    assert!(white > 100 && yellow > 50, "white {white}, yellow {yellow}");
}
