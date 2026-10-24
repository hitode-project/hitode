//! A Wine session as the shell runs one: a kept prefix, real time, and a GPU
//! whose completions wake the run loop.
//!
//! It needs `ADV_WINE_DIST` and `ADV_WINE_PROBES`, as the probe tests do.
#![cfg(all(any(target_vendor = "apple", target_os = "android"), feature = "gpu"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-SES-1, WINE-SES-2
#[test]
fn a_session_runs_a_program_from_its_directory_and_presents() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-session-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("d3d9.exe"), game.join("d3d9.exe")).unwrap();
    // What an earlier run left: drive D: on another program's directory.
    let devices = root.join("session/dosdevices");
    std::fs::create_dir_all(&devices).unwrap();
    std::os::unix::fs::symlink(&probes, devices.join("d:")).unwrap();

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
        exe: &game.join("d3d9.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let gpu_wake = wake.clone();
    let mut session = Session::start(&config, wake, move |machine| {
        let gpu =
            pollster::block_on(adv_device::gpu::Gpu::request(false)).map_err(|e| e.to_string())?;
        gpu.context()
            .set_execution_wake(gpu_wake.clone())
            .map_err(|_| "the GPU already has a wake")?;
        Session::attach_gpu(machine, gpu, gpu_wake)
    })
    .unwrap();
    drop(stdout);
    assert_eq!(
        std::fs::read_link(devices.join("d:")).unwrap(),
        std::path::absolute(&game).unwrap()
    );
    // The guest waits on the GPU with no deadline; only its wake ends that.
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    session.capture().unwrap();
    let frame = session.frame();
    let inked = frame
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[..3] == [0xff, 0xff, 0xff])
        .count();
    let (size, frames) = ((frame.width, frame.height), frame.frames);
    drop(session);

    // SAFETY: output owns the read end of the pipe.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(frames >= 1, "no frame was presented");
    assert_eq!(size, (64, 32));
    assert_eq!(inked, 43);
}

// spec: WINE-UI-1, WINE-UI-2
#[test]
fn popup_choice_and_cancel_run_wines_modal_loop() {
    // The in-process server can start only once in each host process.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "popup_choice_and_cancel_child",
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

// spec: WINE-UI-1, WINE-UI-2
#[test]
#[ignore = "subprocess entry for the popup session test"]
fn popup_choice_and_cancel_child() {
    use adv_runtime::MenuEvent;
    use std::sync::atomic::Ordering;

    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-popup-{}", std::process::id()));
    let (mut output, stdout) = std::io::pipe().unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join("popup_menu.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session =
        Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |_| Ok(())).unwrap();
    drop(stdout);
    let until = Instant::now() + Duration::from_secs(60);
    let mut first = None;
    let mut second = None;
    let stop = loop {
        assert!(Instant::now() < until, "popup did not complete");
        session.refresh_ui();
        let bar = session.menu_bar();
        if let Some(popup) = bar.popup {
            assert_eq!((popup.x, popup.y), (12, 16));
            assert_ne!(
                session.input_view().top,
                bar.key.window,
                "popup presented as a frame"
            );
            if first.is_none() {
                assert!(popup.active.load(Ordering::Acquire));
                first = Some(popup.active);
                let choice = bar.items[0].children[0].command;
                assert_eq!(choice.id, 42);
                assert!(session.menu_event(MenuEvent::Command(choice)));
            } else if !std::sync::Arc::ptr_eq(first.as_ref().unwrap(), &popup.active)
                && second.is_none()
            {
                assert!(!first.as_ref().unwrap().load(Ordering::Acquire));
                second = Some(popup.active);
                assert!(session.menu_event(MenuEvent::Cancel {
                    window: bar.key.window,
                    root: bar.key.menu
                }));
            }
        }
        match session.run(Instant::now() + Duration::from_millis(20)) {
            Stop::Paused => {}
            stop => break stop,
        }
    };
    let completed = (first, second);
    assert!(session.menu_bar().popup.is_none());
    drop(session);
    // SAFETY: output owns the pipe's read descriptor.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(!completed.0.unwrap().load(Ordering::Acquire));
    assert!(!completed.1.unwrap().load(Ordering::Acquire));
    assert!(text.contains("select=42 flags="), "{text}");
    assert!(
        text.contains("picked=42 initialized=2 commands=0 syscommands=0"),
        "{text}"
    );
    assert!(
        text.contains("cancelled=0 initialized=0 commands=0 syscommands=0"),
        "{text}"
    );
}
