//! Owned local regular-file oracle, including ordinary I/O, mapped pages,
//! COPY isolation, and original-file to overlay identity continuity.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[path = "support/cpu.rs"]
mod cpu;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn ordinary_io_and_shared_views_follow_owned_native_local_file_observations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/file-coherence-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-file-coherence.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    std::fs::write(native.join("coherence-base.bin"), [0; 8192]).unwrap();
    let executable = directory.join("fixture_file_coherence.exe");
    let mut child = OwnedChild(
        Command::new(&executable)
            .current_dir(&native)
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "native status {status:?}");
            break;
        }
        if Instant::now() >= deadline {
            panic!("own coherence child timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let reference = std::fs::read(native.join("file-coherence.bin")).unwrap();
    assert_eq!(reference.len(), 65 * 4);
    let words: Vec<_> = reference
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect();
    assert_eq!(
        words[6], 0xaaaaaaaa,
        "unflushed mapped byte observed by ReadFile on this local host"
    );
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    assert_eq!(
        &words[12..17],
        &[0x12345678, 0x12345678, 0xc0c0c0c0, 0xaaaaaaaa, 0x10000001]
    );
    for backend in ["interpreter", "jit", "unicorn"] {
        // This authored asset belongs to the test. The runtime must preserve it
        // while making its writable overlay visible through the older view.
        std::fs::write(directory.join("coherence-base.bin"), [0; 8192]).unwrap();
        let profile = directory.join(format!("{backend}-{}-{stamp}", std::process::id()));
        let mut runtime = Runtime::with_cpu(
            &executable,
            &profile,
            cpu::Cpu::new(backend).unwrap(),
            32,
            32,
        )
        .unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(5)));
        assert!(
            matches!(runtime.machine.run_virtual(5_000_000), RunResult::Exited(0)),
            "{backend}: {}",
            runtime.report()
        );
        let actual = std::fs::read(profile.join("overlay/file-coherence.bin")).unwrap();
        for (index, (actual, expected)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(actual, expected, "{backend} word {index}");
        }
        assert_eq!(actual.len(), reference.len());
        assert_eq!(
            std::fs::read(directory.join("coherence-base.bin")).unwrap(),
            [0; 8192]
        );
        assert_eq!(
            &std::fs::read(profile.join("overlay/coherence-base.bin")).unwrap()[..4],
            &0xfacecafeu32.to_le_bytes()
        );
    }
}
