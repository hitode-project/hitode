//! A Wine session as the shell runs one: a kept prefix, real time, and a GPU
//! whose completions wake the run loop.
//!
//! It needs `ADV_WINE_DIST`, as the probe tests do.
#![cfg(all(any(target_vendor = "apple", target_os = "android"), feature = "gpu"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

// spec: WINE-SES-10, WINE-SES-8, WINE-SES-3, WINE-SES-1, WINE-DLL-3, WINE-SES-2
#[test]
fn sequential_sessions_reclaim_clients_and_keep_the_prefix() {
    if wine::wine().is_none() {
        return;
    }
    assert!(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "sequential_sessions_child",
                "--nocapture"
            ])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn sequential_sessions_child() {
    use adv_runtime::wine::session::{Host, SessionOptions};
    let wine::Wine { dist, probes } = wine::wine().unwrap();
    let root = std::env::temp_dir().join(format!("adv-wine-lifecycle-{}", std::process::id()));
    let first = root.join("first");
    let second = root.join("second");
    for directory in [&first, &second] {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::copy(
            probes.join("session_lifecycle.exe"),
            directory.join("session_lifecycle.exe"),
        )
        .unwrap();
    }
    let prefix = root.join("prefix");
    let config = Config {
        dist: &dist,
        prefix: &prefix,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &first.join("session_lifecycle.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let mut host = Host::default();
    let mut installed = None;
    #[cfg(target_os = "macos")]
    let mut descriptors = Vec::new();
    for turn in 0..4 {
        let directory = if turn < 2 { &first } else { &second };
        let config = Config {
            exe: &directory.join("session_lifecycle.exe"),
            ..config
        };
        if turn == 1 {
            std::fs::write(directory.join("hold"), []).unwrap();
            std::fs::write(directory.join("spawn"), []).unwrap();
            std::fs::remove_file(directory.join("ready")).unwrap();
        }
        let mut options = SessionOptions::default();
        if turn < 2 {
            options
                .fonts
                .families
                .insert("HitodeLifetimeAlias".into(), "Arial".into());
        }
        let wake = adv_sync::WakeEvent::new().unwrap();
        let mut session = host
            .start(&config, wake.clone(), &options, |_| Ok(()))
            .unwrap();
        session
            .machine
            .visit_context(None, |env| {
                let key = r"\Registry\Machine\Hardware\Description\System\BIOS";
                let value =
                    adv_nt::registry::platform_value(env, key, "SystemProductName").unwrap();
                let expected = if turn == 0 {
                    "Hitode Virtual Machine"
                } else {
                    "Hitode session marker"
                };
                let expected: Vec<_> = expected
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                assert_eq!(
                    value,
                    Some((1, expected)),
                    "server boot state was reinitialized"
                );
                adv_nt::registry::set_volatile_string(
                    env,
                    key,
                    "SystemProductName",
                    "Hitode session marker",
                )
                .unwrap();
            })
            .unwrap();
        {
            use std::os::unix::fs::MetadataExt;
            let system = prefix.join("drive_c/windows/system32");
            let files = ["ntdll.dll", "ddraw.dll", "winmm.dll"]
                .map(|name| std::fs::symlink_metadata(system.join(name)).unwrap().ino());
            if let Some(installed) = installed {
                assert_eq!(files, installed, "unchanged prefix files were replaced");
            }
            installed = Some(files);
        }
        assert!(
            host.start(
                &config,
                adv_sync::WakeEvent::new().unwrap(),
                &SessionOptions::default(),
                |_| Ok(())
            )
            .is_err()
        );
        let until = Instant::now() + Duration::from_secs(30);
        let stop = loop {
            assert!(Instant::now() < until, "session {turn} timed out");
            match session.run(Instant::now() + Duration::from_millis(20)) {
                Stop::Paused if turn == 1 && directory.join("ready").exists() => {
                    break Stop::Paused;
                }
                Stop::Paused => {}
                stop => break stop,
            }
        };
        if turn == 1 {
            assert!(matches!(stop, Stop::Paused));
            session.set_suspended(true);
            session.refresh_ui();
            let generation = wake.generation();
            session.refresh_ui();
            assert_eq!(
                wake.generation(),
                generation,
                "idle UI refresh woke execution"
            );
            let paused = session.machine.sys.clock.now_us();
            std::thread::sleep(Duration::from_millis(5));
            assert_eq!(session.machine.sys.clock.now_us(), paused);
            session.set_suspended(false);
        } else {
            assert!(matches!(stop, Stop::Exited(0)), "session {turn}: {stop:?}");
        }
        drop(session);
        #[cfg(target_os = "macos")]
        descriptors.push(std::fs::read_dir("/dev/fd").unwrap().count());
        let observed = [
            (turn as u32 + 1).to_le_bytes(),
            u32::from(turn < 2).to_le_bytes(),
        ]
        .concat();
        assert_eq!(std::fs::read(directory.join("ready")).unwrap(), observed);
        let registry = std::fs::read_to_string(prefix.join("user.reg")).unwrap();
        assert!(
            registry.contains(&format!("\"Runs\"=dword:{:08x}", turn + 1)),
            "registry was not finalized"
        );
        if turn == 1 {
            // A machine preparation failure must leave the server usable too.
            assert!(
                host.start(
                    &config,
                    adv_sync::WakeEvent::new().unwrap(),
                    &SessionOptions::default(),
                    |_| Err("authored startup failure".into())
                )
                .is_err()
            );
        }
    }
    #[cfg(target_os = "macos")]
    assert!(
        descriptors[3] <= descriptors[2],
        "guest descriptors accumulated: {descriptors:?}"
    );
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WINE-SES-1, WINE-SES-2, HOST-SAVE-5
#[test]
fn a_session_runs_a_program_from_its_directory_and_presents() {
    program_session(false);
}

// spec: WINE-GFX-6
#[test]
fn a_child_can_present_native_gpu_frames() {
    assert!(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_child_session",
                "--nocapture"
            ])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn native_child_session() {
    program_session(true);
}

fn program_session(child: bool) {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-session-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("d3d9.exe"), game.join("d3d9.exe")).unwrap();
    std::fs::write(
        game.join("d3d9_inst.xml"),
        b"<InstallSetting><InstallPath></InstallPath><RunExe></RunExe></InstallSetting>",
    )
    .unwrap();
    if child {
        std::fs::write(game.join("spawn-child"), []).unwrap();
    }
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
    let state = std::fs::read_to_string(
        root.join("session/drive_c/Users/Player/AppData/Roaming/SilkysPlus/d3d9/d3d9_inst.xml"),
    )
    .unwrap();
    assert!(state.contains("<InstallPath>D:</InstallPath>"), "{state}");
    assert!(state.contains(r"<RunExe>D:\d3d9.exe</RunExe>"), "{state}"); // privacy: allow authored guest executable path
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("presented=0"), "{text}");
    if child {
        return;
    }
    assert!(frames >= 1, "no frame was presented");
    assert_eq!(size, (64, 32));
    // "Hi" in Wine's Tahoma at -16, non-antialiased, from the native rasterizer.
    assert_eq!(inked, 75);
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

    let Some(wine::Wine { dist, probes }) = wine::wine() else {
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
            match &first {
                None => {
                    assert!(popup.active.load(Ordering::Acquire));
                    first = Some(popup.active);
                    let choice = bar.items[0].children[0].command;
                    assert_eq!(choice.id, 42);
                    assert!(session.menu_event(MenuEvent::Command(choice)));
                }
                Some(earlier)
                    if !std::sync::Arc::ptr_eq(earlier, &popup.active) && second.is_none() =>
                {
                    assert!(!earlier.load(Ordering::Acquire));
                    second = Some(popup.active);
                    assert!(session.menu_event(MenuEvent::Cancel {
                        window: bar.key.window,
                        root: bar.key.menu
                    }));
                }
                Some(_) => {}
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

// spec: WINE-UI-7
#[test]
fn a_message_box_is_projected_natively_from_its_first_frame() {
    // The in-process server can start only once in each host process.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "message_box_child", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// spec: WINE-UI-7
#[test]
#[ignore = "subprocess entry for the message box session test"]
fn message_box_child() {
    use adv_runtime::DialogEvent;

    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-box-{}", std::process::id()));
    let (mut output, stdout) = std::io::pipe().unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join("message_box.exe"),
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
    let mut answered = false;
    let stop = loop {
        assert!(Instant::now() < until, "the message box was not answered");
        session.refresh_ui();
        if let Some(dialog) = session.dialogs().dialogs.first().cloned() {
            assert_ne!(
                session.input_view().top,
                dialog.target.hwnd,
                "message box presented as a frame"
            );
            let unsupported: Vec<_> = dialog
                .controls
                .iter()
                .filter(|control| !control.supported)
                .map(|control| (&control.class, control.style))
                .collect();
            assert!(unsupported.is_empty(), "{unsupported:?}");
            if !answered {
                let ok = dialog
                    .controls
                    .iter()
                    .find(|control| control.target.id == 1)
                    .expect("an OK button");
                answered = session.dialog_event(DialogEvent::Click(ok.target));
            }
        }
        match session.run(Instant::now() + Duration::from_millis(20)) {
            Stop::Paused => {}
            stop => break stop,
        }
    };
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
    assert!(text.contains("answer=1"), "{text}");
}
