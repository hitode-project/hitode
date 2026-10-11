//! Synthetic video verifies MCI A/W control over the existing media graph.
#![cfg(all(windows, feature = "media"))]
use adv_machine::{RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use std::{
    os::windows::process::CommandExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn digital_video_open_wait_status_and_close_match_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/mci-video-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-mmio.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let ffmpeg =
        Path::new(&std::env::var_os("FFMPEG_DIR").expect("FFMPEG_DIR for the synthetic clip"))
            .join("bin/ffmpeg.exe");
    let clip = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x32:rate=25",
            "-t",
            "0.4",
            "-c:v",
            "wmv2",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(output.join("clip.wmv"))
        .output()
        .unwrap();
    assert!(
        clip.status.success(),
        "{}",
        String::from_utf8_lossy(&clip.stderr)
    );
    let exe = output.join("fixture_mci_video.exe");
    let mut native = Command::new(&exe)
        .current_dir(&output)
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = native.try_wait().unwrap() {
            assert!(status.success(), "native MCI: {status}");
            break;
        }
        if Instant::now() > deadline {
            native.kill().unwrap();
            panic!("native MCI deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let expected = std::fs::read(output.join("mci-video.bin")).unwrap();
    for backend in ["interpreter", "jit", "unicorn"] {
        let data = output.join(backend);
        let mut runtime =
            Runtime::with_cpu(&exe, &data, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "MCI {backend} deadline");
            match runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded | SliceResult::Complete(RunResult::Idle { .. }) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0, "{backend}");
                    break;
                }
                other => panic!("MCI {backend}: {other:?}"),
            }
        }
        assert_eq!(
            std::fs::read(data.join("overlay/mci-video.bin")).unwrap(),
            expected,
            "{backend}"
        );
    }
}
