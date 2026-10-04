//! Own raw-file PE oracle, without third-party/game/DAC execution.
#![cfg(windows)]
use adv_cpu::backend::Guest;
use adv_machine::{Clock, RunResult};
use adv_os::ModuleKind;
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
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn datafile_views_match_native_without_executable_lifetime_or_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/load-datafile-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-load-datafile.ps1"))
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
    for file in [
        "fixture_datafile_native.exe",
        "datafile.dll",
        "special_dep.dll",
    ] {
        std::fs::copy(directory.join(file), native.join(file)).unwrap();
    }
    let native_report = native.join("load-datafile.bin");
    if native_report.exists() {
        std::fs::remove_file(&native_report).unwrap();
    }
    let mut child = OwnedChild(
        Command::new(native.join("fixture_datafile_native.exe"))
            .current_dir(&native)
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "own native DATAFILE: {status:?}");
            break;
        }
        assert!(Instant::now() < deadline, "own DATAFILE child timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    let golden = std::fs::read(native_report).unwrap();
    assert_eq!(golden.len(), 110 * 4);
    let words: Vec<_> = golden
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    assert_eq!(&words[..4], &[1, 8, 0x12345678, 0x9abcdef0]);
    assert_eq!(&words[8..14], &[2, 2, 0x40000, 0, 1, 1]);
    assert_eq!(
        &words[32..38],
        &[0xfeedcafe, 4, 0x9abcdef0, 1, 1, 0x1111beef]
    );
    let original = std::fs::read(directory.join("datafile.dll")).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for backend in ["interpreter", "jit", "unicorn"] {
        let profile = directory.join(format!("{backend}-{}-{stamp}", std::process::id()));
        let mut runtime = adv_runtime::boot::Runtime::with_cpu(
            directory.join("fixture_datafile_native.exe"),
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
        let result = runtime.machine.run_virtual(5_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}; {}",
            runtime.report()
        );
        let actual = std::fs::read(profile.join("overlay/load-datafile.bin")).unwrap();
        assert_eq!(actual.len(), golden.len());
        for (index, (actual, expected)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(golden.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(
                actual,
                expected,
                "{backend} word {index}; {}",
                runtime.report()
            );
        }
        assert_eq!(
            std::fs::read(directory.join("datafile.dll")).unwrap(),
            original
        );
        let modules = adv_os::modules(&runtime.machine.ext);
        let raw: Vec<_> = modules
            .iter()
            .filter(|(_, module)| module.kind == ModuleKind::DataFile)
            .collect();
        assert_eq!(
            raw.len(),
            1,
            "only the intentionally forgotten view may remain"
        );
        let module = raw[0].1;
        assert_eq!(module.ldr_entry, 0);
        assert_eq!(module.lifetime.external_refs, 1);
        assert!(modules.by_name("datafile.dll").is_none());
        assert!(modules.containing(module.base).is_none());
        assert!(runtime.machine.cpu.accessible(
            module.base,
            module.size,
            adv_cpu::backend::Prot::READ
        ));
        assert!(
            !runtime
                .machine
                .cpu
                .accessible(module.base, 1, adv_cpu::backend::Prot::EXEC)
        );
        let weak = std::rc::Rc::downgrade(&module.data_file.as_ref().unwrap().section);
        drop(runtime);
        assert!(
            weak.upgrade().is_none(),
            "forgotten section retained after process owner Drop"
        );

        // Unsupported flags differ deliberately from native supported modes.
        // Validate explicit rejection separately, never call them a differential pass.
        let profile = directory.join(format!(
            "unsupported-{backend}-{}-{stamp}",
            std::process::id()
        ));
        let mut runtime = adv_runtime::boot::Runtime::with_cpu(
            directory.join("fixture_datafile_unsupported.exe"),
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
        let mut expected: Vec<_> = (0..8).flat_map(|_| [1u32, 50]).collect();
        expected.extend_from_slice(&[0, 1]);
        assert_eq!(
            std::fs::read(profile.join("overlay/load-datafile.bin")).unwrap(),
            expected
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert!(
            adv_os::modules(&runtime.machine.ext)
                .iter()
                .all(|(_, m)| m.kind != ModuleKind::DataFile)
        );
        assert!(
            adv_os::modules(&runtime.machine.ext)
                .by_name("datafile.dll")
                .is_none()
        );
    }
}
