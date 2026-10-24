//! Hardware input enters Wine on the owning guest thread and wakes its
//! message wait through the driver's queue fd, without a host window.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::{RunResult, SliceResult};
use adv_runtime::wine::session::{Config, Session, Stop};

fn read(output: &mut File, text: &mut String) {
    let _ = output.read_to_string(text);
}

fn run(probe: &str, gpu: bool) {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-{probe}-{}", std::process::id()));
    let mut fds = [0; 2];
    // SAFETY: fds has room for both ends.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: both descriptors are new and owned by nobody else.
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    // SAFETY: output owns this descriptor; reads must not block the test.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join(probe),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake.clone(), |machine| {
        #[cfg(feature = "gpu")]
        if gpu {
            let gpu = pollster::block_on(adv_device::gpu::Gpu::request(false))
                .map_err(|e| e.to_string())?;
            gpu.context()
                .set_execution_wake(wake.clone())
                .map_err(|_| "GPU wake already installed")?;
            Session::attach_gpu(machine, gpu, wake.clone())?;
        }
        #[cfg(not(feature = "gpu"))]
        let _ = (machine, gpu);
        Ok(())
    })
    .unwrap();
    drop(stdout);
    let mut text = String::new();
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let stop = session.run(Instant::now() + Duration::from_millis(20));
        read(&mut output, &mut text);
        assert!(matches!(stop, Stop::Paused), "{stop:?}: {text}");
        session.capture().unwrap();
        if text.contains("ready=") && session.frame().frames > 0 {
            break;
        }
        assert!(Instant::now() < until, "window did not present: {text}");
    }
    // Every presenter's frames carry the client area, so host points are
    // client points.
    let point = 17 << 16 | 13;
    // Every event starts with every guest context blocked indefinitely.
    let events = [
        (0x200, 0, point, "mouse=200 point=13,17 data=0"),
        (0x201, 1, point, "mouse=201 point=13,17 data=0"),
        (0x202, 0, point, "mouse=202 point=13,17 data=0"),
        (0x204, 2, point, "mouse=204 point=13,17 data=0"),
        (0x205, 0, point, "mouse=205 point=13,17 data=0"),
        (0x207, 16, point, "mouse=207 point=13,17 data=0"),
        (0x208, 0, point, "mouse=208 point=13,17 data=0"),
        (0x20a, 120 << 16, point, "mouse=20a point=13,17 data=120"),
        (0x100, 0x41, 0x001e0001, "key=100 vk=65 scan=30 extended=0"),
        (0x101, 0x41, 0xc01e0001, "key=101 vk=65 scan=30 extended=0"),
        (0x100, 0x27, 0x014d0001, "key=100 vk=39 scan=77 extended=1"),
        (0x101, 0x27, 0xc14d0001, "key=101 vk=39 scan=77 extended=1"),
        (0x104, 0x12, 0x00380001, "key=104 vk=18 scan=56 extended=0"),
        (0x104, 0x42, 0x20300001, "key=104 vk=66 scan=48 extended=0"),
        (0x105, 0x42, 0xe0300001, "key=105 vk=66 scan=48 extended=0"),
        (0x105, 0x12, 0xc0380001, "key=101 vk=18 scan=56 extended=0"),
    ];
    for (message, wparam, lparam, expected) in events {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let idle = session.machine.run_slice(Duration::from_millis(1));
            if matches!(
                idle,
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: None
                })
            ) {
                break;
            }
            assert!(
                Instant::now() < until,
                "guest is not waiting: {idle:?}: {text}"
            );
            let stop = session.run(Instant::now() + Duration::from_millis(20));
            read(&mut output, &mut text);
            assert!(matches!(stop, Stop::Paused), "{stop:?}: {text}");
        }
        session.input(message, wparam, lparam).unwrap();
        let stop = session.run(Instant::now() + Duration::from_millis(50));
        read(&mut output, &mut text);
        assert!(matches!(stop, Stop::Paused), "{stop:?}: {text}");
        assert!(
            text.lines().any(|line| line == expected),
            "missing {expected}: {text}"
        );
    }
    session.input(0x100, 0x7b, 0x00580001).unwrap();
    session.input(0x101, 0x7b, 0xc0580001).unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(10));
    read(&mut output, &mut text);
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("key=101 vk=123 scan=88 extended=0"), "{text}");
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WINE-IN-1, WINE-IN-2, WINE-IN-3
#[test]
#[ignore = "run by host_input_in_each_presenter: one wineserver per process"]
fn window_surface_input_wakes_a_message_wait() {
    run("input.exe", false);
}

// spec: WINE-IN-1, WINE-IN-2, WINE-IN-3
#[test]
#[ignore = "run by host_input_in_each_presenter: one wineserver per process"]
fn directdraw_input_uses_client_coordinates() {
    run("input_ddraw.exe", false);
}

// spec: WINE-IN-1, WINE-IN-2, WINE-IN-3
#[cfg(feature = "gpu")]
#[test]
#[ignore = "run by host_input_in_each_presenter: one wineserver per process"]
fn d3d9_input_uses_client_coordinates() {
    run("input_d3d9.exe", true);
}

#[test]
fn host_input_in_each_presenter() {
    // Wine's server can start only once in a process.
    for case in [
        "window_surface_input_wakes_a_message_wait",
        "directdraw_input_uses_client_coordinates",
        #[cfg(feature = "gpu")]
        "d3d9_input_uses_client_coordinates",
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([case, "--exact", "--ignored", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
