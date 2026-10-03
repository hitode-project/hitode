//! Owned DLL fixtures establish their oracle on native Windows before translation.
#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::RunResult;
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod backend;

#[test]
fn repeated_load_dependency_release_failed_attach_and_pin_match_windows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/fixtures-module-lifecycle-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-module-lifecycle.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = directory.join("fixture_module_lifecycle.exe");
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status:?}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native fixture timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let decode = |bytes: Vec<u8>| {
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>()
    };
    let golden = decode(std::fs::read(directory.join("module-lifecycle.bin")).unwrap());
    assert!(
        golden
            .windows(5)
            .any(|part| part == [0x40201, 0x40200, 0, 1114, 0]),
        "native failed attach oracle missing"
    );
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join(name);
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        // Ensure warmed DLL code is compiled before its immediate FreeLibrary;
        // this fixture must exercise invalidation, not race a background compile.
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        let actual = decode(std::fs::read(data.join("overlay/module-lifecycle.bin")).unwrap());
        assert_eq!(
            actual, golden,
            "{name}: module event/return sequence differs from Windows"
        );
        let modules = adv_os::modules(&runtime.machine.ext);
        for unloaded in [
            "lifecycle_dep.dll",
            "lifecycle_parent.dll",
            "lifecycle_fail.dll",
            "lifecycle_tls.dll",
        ] {
            assert!(
                modules.by_name(unloaded).is_none(),
                "{name}: stale {unloaded}"
            );
        }
        let pinned = modules.get(modules.by_name("lifecycle_normal.dll").unwrap());
        assert!(pinned.lifetime.pinned);
        assert!(
            runtime
                .machine
                .sys
                .tls_images
                .iter()
                .all(|image| !image.active)
        );
        // Unlink all three live LDR lists; no retired DLL may remain observable.
        use adv_cpu::backend::GuestExt;
        let cpu = &runtime.machine.cpu;
        let ldr = cpu.read_u32(adv_machine::layout::PEB_ADDR + 0x0c).unwrap();
        for (head_offset, node_offset) in [(0x0c, 0), (0x14, 8), (0x1c, 16)] {
            let head = ldr + head_offset;
            let mut node = cpu.read_u32(head).unwrap();
            let mut count = 0;
            while node != head {
                let base = cpu.read_u32(node - node_offset + 0x18).unwrap();
                assert!(
                    modules.by_handle(base).is_some(),
                    "{name}: retired LDR image {base:x}"
                );
                let next = cpu.read_u32(node).unwrap();
                assert_eq!(cpu.read_u32(next + 4).unwrap(), node);
                node = next;
                count += 1;
                assert!(count <= modules.iter().count());
            }
        }
        if name == "jit" {
            assert!(runtime.machine.cpu.stats().native_entries > 0);
            assert!(
                runtime.machine.cpu.stats().invalidations > 0,
                "unloading warmed DLL code must invalidate its native regions"
            );
            // The old monotonic LDR allocator exhausted its 2 MiB window after
            // roughly 500 loads. Storage-only repetition deliberately runs no
            // guest entry points; the native fixture above covers callbacks.
            let count = modules.iter().count();
            let (win32, modules) = runtime
                .machine
                .ext
                .get2_mut::<adv_win32::Win32, adv_os::Modules>();
            let mut loader = adv_os::Loader::new(
                &mut runtime.machine.cpu,
                &mut runtime.machine.sys,
                modules,
                &win32.vfs,
            );
            let mut first = None;
            for _ in 0..600 {
                let id = loader.load("lifecycle_tls.dll").unwrap();
                let module = loader.modules.get(id);
                let storage = (module.base, module.ldr_entry, module.lifetime.tls_index);
                if let Some(first) = first {
                    assert_eq!(storage, first);
                } else {
                    first = Some(storage);
                }
                loader.inits.clear();
                loader.retire(id).unwrap();
                assert_eq!(loader.modules.iter().count(), count);
                assert!(loader.sys.tls_images.iter().all(|image| !image.active));
                assert!(loader.sys.vm.region(storage.0).is_none());
            }
            let source = std::fs::read(directory.join("lifecycle_parent.dll")).unwrap();
            drop(loader);
            struct BrokenImport {
                bytes: Vec<u8>,
                directory: std::path::PathBuf,
            }
            impl adv_os::FileSource for BrokenImport {
                fn read(&self, path: &str) -> Option<Vec<u8>> {
                    if path == "broken.dll" {
                        Some(self.bytes.clone())
                    } else {
                        std::fs::read(self.directory.join(path)).ok()
                    }
                }
            }
            // A dependency loads successfully, then its requested export fails.
            // The parent and newly loaded child must both roll back, including LDR.
            let mut broken = source.clone();
            let import = broken
                .windows(11)
                .position(|part| part == b"Dependency\0")
                .unwrap();
            broken[import] = b'X';
            let files = BrokenImport {
                bytes: broken,
                directory: directory.clone(),
            };
            let mut loader = adv_os::Loader::new(
                &mut runtime.machine.cpu,
                &mut runtime.machine.sys,
                runtime.machine.ext.get_mut::<adv_os::Modules>(),
                &files,
            );
            assert!(matches!(
                loader.load("broken.dll"),
                Err(adv_os::LoadError::MissingExport { .. })
            ));
            assert!(loader.inits.is_empty());
            assert_eq!(loader.modules.iter().count(), count);
            assert!(loader.modules.by_name("lifecycle_dep.dll").is_none());
            drop(loader);
            // A malformed resource directory fails before a Module is published;
            // its reserved image address must still be released.
            let mut broken = source;
            let pe = u32::from_le_bytes(broken[0x3c..0x40].try_into().unwrap()) as usize;
            broken[pe + 24 + 28..pe + 24 + 32].copy_from_slice(&0x24000000u32.to_le_bytes());
            broken[pe + 24 + 96 + 16..pe + 24 + 96 + 20]
                .copy_from_slice(&0xfffffff0u32.to_le_bytes());
            broken[pe + 24 + 96 + 20..pe + 24 + 96 + 24].copy_from_slice(&64u32.to_le_bytes());
            let files = BrokenImport {
                bytes: broken,
                directory: directory.clone(),
            };
            let mut loader = adv_os::Loader::new(
                &mut runtime.machine.cpu,
                &mut runtime.machine.sys,
                runtime.machine.ext.get_mut::<adv_os::Modules>(),
                &files,
            );
            assert!(matches!(
                loader.load("broken.dll"),
                Err(adv_os::LoadError::Pe { .. })
            ));
            assert_eq!(loader.modules.iter().count(), count);
            assert!(loader.sys.vm.region(0x24000000).is_none());
        }
    }
}
