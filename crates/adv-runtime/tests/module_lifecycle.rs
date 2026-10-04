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
fn resource_handles_are_stable_image_entries_and_observe_live_metadata() {
    let directory = fixtures();
    let exe = directory.join("fixture_resource_handles.exe");
    let _ = std::fs::remove_file(directory.join("resource-handles.bin"));
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native resource handles: {status:?}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native resource handles timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let golden = std::fs::read(directory.join("resource-handles.bin")).unwrap();
    assert_eq!(
        golden,
        [
            8u32, 0x12345678, 0x9abcdef0, 1, 1, 1, 1, 1, 1, 4, 0x9abcdef0, 0, 1, 0
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>()
    );
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join("resources").join(name);
        let _ = std::fs::remove_file(data.join("overlay/resource-handles.bin"));
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        assert_eq!(
            std::fs::read(data.join("overlay/resource-handles.bin")).unwrap(),
            golden,
            "{name}: image-owned resource identity or metadata differs from Windows"
        );
        assert!(
            adv_os::modules(&runtime.machine.ext)
                .by_name("lifecycle_resources.dll")
                .is_none()
        );
    }
}

#[test]
fn dynamic_forwarders_initialize_or_roll_back_before_returning() {
    let directory = fixtures();
    let exe = directory.join("fixture_module_forwarder.exe");
    let _ = std::fs::remove_file(directory.join("module-forwarder.bin"));
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native forwarder: {status:?}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native forwarder timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let golden = std::fs::read(directory.join("module-forwarder.bin")).unwrap();
    let words: Vec<_> = golden
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    assert!(
        words
            .windows(5)
            .any(|part| part == [0x40201, 0x40200, 0, 127, 0])
    );
    assert!(
        words
            .windows(4)
            .any(|part| part == [0xa0201, 0xa0200, 0x50100, 0x50200])
    );
    assert_eq!(&words[..6], &[0x80201, 0, 0x20201, 2, 1, 1]);
    assert!(words.ends_with(&[0x80200, 0x20200, 0, 0]));
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join("forwarder").join(name);
        let _ = std::fs::remove_file(data.join("overlay/module-forwarder.bin"));
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        assert_eq!(
            std::fs::read(data.join("overlay/module-forwarder.bin")).unwrap(),
            golden,
            "{name}: target initialization, feature probe or unload differs from Windows"
        );
        // Storage-only failures must undo new edges on an already loaded image.
        // No callback runs here; native notification behavior is checked above.
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
        let forwarder = loader.load("lifecycle_forward.dll").unwrap();
        loader.inits.clear();
        let edges = loader.modules.get(forwarder).lifetime.dependencies.clone();
        let count = loader.modules.iter().count();
        for _ in 0..32 {
            assert!(matches!(
                loader.load("lifecycle_badforward.dll"),
                Err(adv_os::LoadError::MissingExport { .. })
            ));
            assert!(loader.inits.is_empty());
            assert_eq!(loader.modules.iter().count(), count);
            assert_eq!(loader.modules.get(forwarder).lifetime.dependencies, edges);
            for export in ["Cycle", "AbsentGuest"] {
                let symbol = adv_pe::ImportName::Name {
                    hint: 0,
                    name: export.into(),
                };
                assert!(loader.resolve_dynamic(forwarder, &symbol).is_err());
                assert!(loader.inits.is_empty());
                assert_eq!(loader.modules.iter().count(), count);
                assert_eq!(loader.modules.get(forwarder).lifetime.dependencies, edges);
                assert!(loader.modules.by_name("lifecycle_dep.dll").is_none());
                assert!(loader.modules.unreachable().is_empty());
            }
        }
    }
}

#[test]
fn freeing_the_executing_dll_and_exiting_its_thread_matches_windows() {
    let directory = fixtures();
    let exe = directory.join("fixture_module_self_exit.exe");
    let _ = std::fs::remove_file(directory.join("module-self-exit.bin"));
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native self unload: {status:?}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native self unload timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let decode = |bytes: Vec<u8>| {
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>()
    };
    let golden = decode(std::fs::read(directory.join("module-self-exit.bin")).unwrap());
    assert!(
        golden
            .windows(2)
            .any(|part| part == [0xa0000017, 0xb0000001])
    );
    assert!(
        golden
            .windows(2)
            .any(|part| part == [0xa0000017, 0xb0000000])
    );
    // The retained image receives THREAD_DETACH once, then only PROCESS_DETACH
    // when the second worker releases the final reference before exiting.
    assert_eq!(golden.iter().filter(|&&event| event == 0x70203).count(), 1);
    assert_eq!(golden.iter().filter(|&&event| event == 0x70200).count(), 1);
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join("self-exit").join(name);
        let _ = std::fs::remove_file(data.join("overlay/module-self-exit.bin"));
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        assert_eq!(
            decode(std::fs::read(data.join("overlay/module-self-exit.bin")).unwrap()),
            golden,
            "{name}: unloading current code must not return through the retired image"
        );
        assert!(
            adv_os::modules(&runtime.machine.ext)
                .by_name("lifecycle_self.dll")
                .is_none()
        );
    }
}

#[test]
fn startup_gate_and_primary_thread_exit_match_windows() {
    let directory = fixtures();
    let exe = directory.join("fixture_thread_exit.exe");
    let _ = std::fs::remove_file(directory.join("thread-exit.bin"));
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(19));
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native thread exit timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let golden = std::fs::read(directory.join("thread-exit.bin")).unwrap();
    assert_eq!(
        golden,
        [0u32, 0, 7]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join("thread-exit").join(name);
        let _ = std::fs::remove_file(data.join("overlay/thread-exit.bin"));
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(19)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        assert_eq!(
            std::fs::read(data.join("overlay/thread-exit.bin")).unwrap(),
            golden,
            "{name}: worker ran before initialization or primary thread ended the process"
        );
    }
}

#[test]
fn thread_and_process_notifications_match_windows_including_disabled_tls_modules() {
    let directory = fixtures();
    let _ = std::fs::remove_file(directory.join("module-notifications.bin"));
    let exe = directory.join("fixture_module_notifications.exe");
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native notifications timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let decode = |bytes: Vec<u8>| {
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>()
    };
    let golden = decode(std::fs::read(directory.join("module-notifications.bin")).unwrap());
    assert!(golden.ends_with(&[0x50100, 0x50280, 0x10280]));
    for name in ["jit", "interpreter", "unicorn"] {
        let data = directory.join("notifications").join(name);
        let _ = std::fs::remove_file(data.join("overlay/module-notifications.bin"));
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(name).unwrap(), 64, 32).unwrap();
        runtime.machine.sys.clock = adv_machine::Clock::virtual_time();
        runtime.machine.cpu.set_async_compilation(false);
        let result = runtime.machine.run_virtual(10_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{name}: {result:?}; {:?}",
            runtime.machine.sys.recent_calls()
        );
        let actual = decode(std::fs::read(data.join("overlay/module-notifications.bin")).unwrap());
        assert_eq!(
            actual, golden,
            "{name}: thread/process notification sequence differs from Windows"
        );
    }
}

fn fixtures() -> std::path::PathBuf {
    static DIRECTORY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
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
            directory
        })
        .clone()
}

#[test]
fn repeated_load_dependency_release_failed_attach_and_pin_match_windows() {
    let directory = fixtures();
    let _ = std::fs::remove_file(directory.join("module-lifecycle.bin"));
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
        let _ = std::fs::remove_file(data.join("overlay/module-lifecycle.bin"));
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
