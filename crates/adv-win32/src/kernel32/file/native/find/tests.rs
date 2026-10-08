//! Authored directory providers exercise live entry calls and native VFS cursors.
use super::*;
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLE: u32 = 0x123abc00;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    path: String,
    calls: usize,
    closes: usize,
}

fn install(env: &mut Env<'_>) -> Outcome {
    for &(entry, target) in &env.ext.get::<Provider>().hooks {
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}

fn open(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(7), 1);
    assert_eq!(env.arg(8), 0x21);
    assert!(env.sys.objects.get(HANDLE).is_none());
    let descriptor = env.cpu.read_u32(env.arg(2) + 8).unwrap();
    let length = u32::from(env.cpu.read_u16(descriptor).unwrap());
    let pointer = env.cpu.read_u32(descriptor + 4).unwrap();
    let units: Vec<_> = (0..length / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    env.ext.get_mut::<Provider>().path = String::from_utf16(&units).unwrap();
    env.cpu.write_u32(env.arg(0), HANDLE).unwrap();
    env.cpu.write_u32(env.arg(3) + 4, 1).unwrap();
    Outcome::Return(0)
}

fn directory(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    assert_eq!(env.arg(7), 3);
    assert_eq!(env.arg(8), 1);
    assert!(env.sys.objects.get(HANDLE).is_none());
    let (iosb, output) = (env.arg(4), env.arg(5));
    let calls = env.ext.get::<Provider>().calls;
    if calls == 0 {
        assert_eq!(env.arg(10), 1);
        let descriptor = env.arg(9);
        assert_ne!(descriptor, 0);
        let size = u32::from(env.cpu.read_u16(descriptor).unwrap());
        let pointer = env.cpu.read_u32(descriptor + 4).unwrap();
        let units: Vec<_> = (0..size / 2)
            .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "*.dat");
    } else {
        assert_eq!(env.arg(9), 0);
        assert_eq!(env.arg(10), 0);
    }
    env.ext.get_mut::<Provider>().calls += 1;
    if calls >= 2 {
        return Outcome::Return(0x80000006);
    }
    let name = if calls == 0 {
        "文件.dat"
    } else {
        "second.dat"
    };
    let bytes: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut entry = vec![0; 94 + bytes.len()];
    entry[40..48].copy_from_slice(&(17u64 + calls as u64).to_le_bytes());
    entry[56..60].copy_from_slice(&0x20u32.to_le_bytes());
    entry[60..64].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    entry[94..].copy_from_slice(&bytes);
    // Delaying inside the native call must retain the guest output allocation.
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu.write(output, &entry).unwrap();
        env.cpu.write_u32(iosb + 4, entry.len() as u32).unwrap();
        Outcome::Return(0)
    })
}

fn close(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    env.ext.get_mut::<Provider>().closes += 1;
    Outcome::Return(0)
}

fn native<B: Backend>(machine: &mut Machine<B>, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let module = modules.by_name("ntdll.dll").unwrap();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            module,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0
}

fn guest<B: Backend>(cpu: B, live: bool, wide: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("open", 11, open as adv_abi::Handler),
        ("directory", 11, directory as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
    ] {
        registry.add(
            "authored-directory-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/find")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/文件.dat"), [0; 17]).unwrap();
    std::fs::write(root.join("game/second.dat"), [0; 18]).unwrap();
    std::fs::write(root.join("game/ignored.txt"), []).unwrap();
    let mut config = crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data"));
    config.os.codepage = 936;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"*.dat\0").unwrap();
    machine
        .cpu
        .write(
            DATA + 32,
            &[b'*', 0, b'.', 0, b'd', 0, b'a', 0, b't', 0, 0, 0],
        )
        .unwrap();
    let mut provider = Provider::default();
    if live {
        let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
        Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
            .load("ntdll.dll")
            .unwrap();
        for (name, hook) in [
            ("NtCreateFile", "open"),
            ("NtQueryDirectoryFile", "directory"),
            ("NtClose", "close"),
        ] {
            let entry = native(&mut machine, name);
            let target = machine
                .resolve_import("authored-directory-provider", hook)
                .unwrap();
            provider.hooks.push((entry, target));
        }
    }
    machine.ext.insert(provider);
    let first = machine
        .resolve_import(
            "kernel32.dll",
            if wide {
                "FindFirstFileW"
            } else {
                "FindFirstFileA"
            },
        )
        .unwrap();
    let next = machine
        .resolve_import(
            "kernel32.dll",
            if wide {
                "FindNextFileW"
            } else {
                "FindNextFileA"
            },
        )
        .unwrap();
    let close = machine.resolve_import("kernel32.dll", "FindClose").unwrap();
    let error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    if live {
        code.call(
            machine
                .resolve_import("authored-directory-provider", "install")
                .unwrap(),
        );
    }
    code.push_imm(DATA + 512)
        .push_imm(DATA + if wide { 32 } else { 0 })
        .call(first)
        .mov_mem_eax(DATA + 64);
    for (index, output) in [(0, DATA + 1152), (1, DATA + 1792)] {
        code.push_imm(output).bytes.extend_from_slice(&[0xff, 0x35]);
        code.bytes.extend_from_slice(&(DATA + 64).to_le_bytes());
        code.call(next).mov_mem_eax(DATA + 68 + index * 8);
        code.call(error).mov_mem_eax(DATA + 72 + index * 8);
    }
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&(DATA + 64).to_le_bytes());
    code.call(close).mov_mem_eax(DATA + 84);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored directory scan", CODE, &[], 65536)
        .unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    assert_eq!(machine.cpu.read_u32(DATA + 68).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 76).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 80).unwrap(), 18);
    assert_eq!(machine.cpu.read_u32(DATA + 84).unwrap(), 1);
    if live {
        assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), HANDLE);
        assert_eq!(machine.ext.get::<Provider>().calls, 3);
        assert_eq!(machine.ext.get::<Provider>().closes, 1);
        assert!(machine.ext.get::<Provider>().path.starts_with(r"\??\C:\"));
    }
    let names: Vec<_> = [DATA + 512 + 44, DATA + 1152 + 44]
        .into_iter()
        .map(|pointer| {
            if wide {
                String::from_utf16_lossy(&crate::util::wstr(&machine.cpu, pointer))
            } else {
                machine
                    .ext
                    .get::<Win32>()
                    .codec
                    .decode(&crate::util::cstr(&machine.cpu, pointer))
            }
        })
        .collect();
    assert!(names.contains(&"文件.dat".into()) && names.contains(&"second.dat".into()));
    assert!(machine.ext.get::<Finds>().0.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_and_live_directory_cursors_preserve_cp936_names_and_continuations() {
    for live in [false, true] {
        for wide in [false, true] {
            guest(JitCpu::new(PARK_ADDR, false).unwrap(), live, wide);
            guest(UnicornCpu::new(PARK_ADDR).unwrap(), live, wide);
        }
    }
}

#[test]
fn direct_native_close_and_handle_reuse_cannot_redirect_find_next_or_find_close() {
    use crate::kernel32::file::locking::tests::{args, env, fixture, returned};
    for closing in [false, true] {
        let (mut machine, root) = fixture(if closing {
            "find-close-reuse"
        } else {
            "find-next-reuse"
        });
        std::fs::write(root.join("game/first.dat"), b"first").unwrap();
        std::fs::write(root.join("game/second.dat"), b"second").unwrap();
        let handle = returned(first(
            &mut env(&mut machine),
            "*.dat".into(),
            DATA + 256,
            true,
        ));
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let original = machine.sys.objects.object_id(handle).unwrap();
        assert_eq!(
            crate::ntdll::file::close(&mut env(&mut machine), adv_abi::Handle(handle)),
            0
        );
        assert!(
            machine
                .sys
                .objects
                .insert_host_at(adv_machine::Namespace::DEFAULT, handle, 79u32)
        );
        let replacement = machine.sys.objects.object_id(handle).unwrap();
        assert_ne!(original, replacement);
        let result = if closing {
            super::close(&mut env(&mut machine), handle)
        } else {
            args(&mut machine, &[handle, DATA + 256]);
            next(&mut env(&mut machine), true)
        };
        assert_eq!(returned(result), 0);
        assert_eq!(crate::util::last_error(&env(&mut machine)), 6);
        assert_eq!(machine.sys.objects.host::<u32>(handle), Some(&79));
        assert_eq!(machine.sys.objects.object_id(handle), Some(replacement));
        assert!(!machine.ext.get::<Finds>().0.contains_key(&handle));
        std::fs::remove_dir_all(root).unwrap();
    }
}
