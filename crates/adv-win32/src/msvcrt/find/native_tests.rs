//! Authored CRT searches preserve native providers, original owners and callbacks.
use super::*;
use crate::Win32;
use adv_abi::{Conv, Handle};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = 0x420000;
const OPAQUE: u32 = 0x123abc00;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    mode: u32,
    handle: u32,
    queries: usize,
    closes: usize,
    allocations: Vec<u32>,
}
fn install(env: &mut Env<'_>) -> Outcome {
    let provider = env.ext.get::<Provider>();
    for (index, &(entry, target)) in provider.hooks.iter().enumerate() {
        if provider.mode == 5 && index != 0 {
            continue;
        }
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}
fn install_tail(env: &mut Env<'_>) -> Outcome {
    for &(entry, target) in &env.ext.get::<Provider>().hooks[1..] {
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}
fn open(env: &mut Env<'_>) -> Outcome {
    let args: [u32; 11] = std::array::from_fn(|index| env.arg(index as u32));
    assert_eq!(args[7], 1);
    assert_eq!(args[8], 0x21);
    env.ext.get_mut::<Provider>().allocations.push(args[2]);
    let handle = if env.ext.get::<Provider>().mode >= 5 {
        let status = crate::ntdll::file::create(
            env,
            GuestPtr::new(args[0]),
            args[1],
            GuestPtr::new(args[2]),
            GuestPtr::new(args[3]),
            GuestPtr::new(args[4]),
            args[5],
            args[6],
            args[7],
            args[8],
            GuestPtr::new(args[9]),
            args[10],
        );
        assert_eq!(status, 0);
        env.cpu.read_u32(args[0]).unwrap()
    } else {
        assert!(env.sys.objects.get(OPAQUE).is_none());
        env.cpu.write_u32(args[0], OPAQUE).unwrap();
        OPAQUE
    };
    env.ext.get_mut::<Provider>().handle = handle;
    env.cpu.write_u32(DATA + 160, handle).unwrap();
    Outcome::Return(0)
}
fn record(name: &str, size: u64) -> Vec<u8> {
    let name: Vec<_> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut bytes = vec![0; 94 + name.len()];
    for (offset, seconds) in [(8, 100u64), (16, 200), (24, 300)] {
        let ticks = 116_444_736_000_000_000 + seconds * 10_000_000;
        bytes[offset..offset + 8].copy_from_slice(&ticks.to_le_bytes());
    }
    bytes[40..48].copy_from_slice(&size.to_le_bytes());
    bytes[56..60].copy_from_slice(&0x20u32.to_le_bytes());
    bytes[60..64].copy_from_slice(&(name.len() as u32).to_le_bytes());
    bytes[94..].copy_from_slice(&name);
    bytes
}
fn directory(env: &mut Env<'_>) -> Outcome {
    let (handle, iosb, output) = (env.arg(0), env.arg(4), env.arg(5));
    assert_eq!(handle, env.ext.get::<Provider>().handle);
    assert_eq!(env.arg(7), 3);
    assert_eq!(env.arg(8), 1);
    let provider = env.ext.get_mut::<Provider>();
    let (call, mode) = (provider.queries, provider.mode);
    provider.queries += 1;
    if mode == 5 || call != 0 {
        assert_eq!(env.arg(9), 0);
        assert_eq!(env.arg(10), 0);
    } else {
        assert_ne!(env.arg(9), 0);
        assert_eq!(env.arg(10), 1);
    }
    if mode == 2 {
        return Outcome::Return(0xc0000022);
    }
    if call >= if mode == 5 { 1 } else { 2 } {
        return Outcome::Return(0x80000006);
    }
    let name = if mode == 5 {
        "provider-after-open.dat"
    } else if call == 0 {
        "文件.dat"
    } else {
        "second.dat"
    };
    let size = if mode == 1 && call == 1 {
        u64::from(u32::MAX) + 1
    } else {
        17 + call as u64
    };
    let bytes = record(name, size);
    env.ext.get_mut::<Provider>().allocations.push(iosb - 48);
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu.write(output, &bytes).unwrap();
        env.cpu.write_u32(iosb + 4, bytes.len() as u32).unwrap();
        if mode == 4 {
            env.cpu.protect(OUTPUT, 4096, Prot::READ).unwrap();
        }
        Outcome::Return(0)
    })
}
fn close_native(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    assert_eq!(handle, env.ext.get::<Provider>().handle);
    env.ext.get_mut::<Provider>().closes += 1;
    let provider = env.ext.get::<Provider>();
    if provider.mode == 3 && provider.closes == 1 {
        return Outcome::Return(0xc0000022);
    }
    if provider.mode >= 5 {
        Outcome::Return(crate::ntdll::file::close(env, Handle(handle)) as u32)
    } else {
        Outcome::Return(0)
    }
}
fn recycle(env: &mut Env<'_>) -> Outcome {
    let handle = env.ext.get::<Provider>().handle;
    assert_eq!(crate::ntdll::file::close(env, Handle(handle)), 0);
    assert!(
        env.sys
            .objects
            .insert_host_at(Namespace::DEFAULT, handle, 79u32)
    );
    Outcome::Return(0)
}
fn cdecl(code: &mut Asm, target: u32, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target)
        .add_esp((args.len() * 4).try_into().unwrap())
        .mov_mem_eax(result);
}
fn with_handle(code: &mut Asm, target: u32, output: Option<u32>, result: u32) {
    if let Some(output) = output {
        code.push_imm(output);
    }
    code.mov_eax_mem(DATA + 128)
        .push_eax()
        .call(target)
        .add_esp(if output.is_some() { 8 } else { 4 })
        .mov_mem_eax(result);
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
fn guest<B: Backend>(cpu: B, mode: u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("tail", 0, install_tail as adv_abi::Handler),
        ("open", 11, open as adv_abi::Handler),
        ("query", 11, directory as adv_abi::Handler),
        ("close", 1, close_native as adv_abi::Handler),
        ("recycle", 0, recycle as adv_abi::Handler),
    ] {
        registry.add(
            "authored-crt-find-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/crt-find-native")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/original-a.dat"), b"a").unwrap();
    std::fs::write(root.join("game/original-b.dat"), b"bb").unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUTPUT, 8192, Prot::RW).unwrap();
    machine.cpu.write(OUTPUT, &[0xcc; 560]).unwrap();
    machine.cpu.write(OUTPUT + 4096, &[0xcc; 560]).unwrap();
    let pattern: Vec<_> = "*.dat"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(DATA, &pattern).unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let mut provider = Provider {
        mode,
        ..Default::default()
    };
    for (name, hook) in [
        ("NtCreateFile", "open"),
        ("NtQueryDirectoryFile", "query"),
        ("NtClose", "close"),
    ] {
        let entry = native(&mut machine, name);
        let target = machine
            .resolve_import("authored-crt-find-provider", hook)
            .unwrap();
        provider.hooks.push((entry, target));
    }
    machine.ext.insert(provider);
    let first = machine
        .resolve_import("msvcr90.dll", "_wfindfirst64i32")
        .unwrap();
    let next = machine
        .resolve_import("msvcrt.dll", "_wfindnext64i32")
        .unwrap();
    let close = machine.resolve_import("msvcr80.dll", "_findclose").unwrap();
    let errno = machine.resolve_import("msvcrt.dll", "_errno").unwrap();
    let mut code = Asm::new(CODE);
    // Materialize each frontend's errno owner before native scratch can be
    // recycled into a later module's first error cell.
    for module in MODULES {
        code.call(machine.resolve_import(module, "_errno").unwrap());
    }
    code.call(
        machine
            .resolve_import("authored-crt-find-provider", "install")
            .unwrap(),
    );
    cdecl(&mut code, first, &[DATA, OUTPUT], DATA + 128);
    if mode == 5 {
        code.call(
            machine
                .resolve_import("authored-crt-find-provider", "tail")
                .unwrap(),
        );
    }
    if mode == 6 {
        code.call(
            machine
                .resolve_import("authored-crt-find-provider", "recycle")
                .unwrap(),
        );
    }
    if mode != 2 && mode != 4 {
        with_handle(&mut code, next, Some(OUTPUT + 4096), DATA + 132);
        code.call(errno).mov_eax_ptr_eax().mov_mem_eax(DATA + 164);
        if mode != 1 && mode != 6 {
            with_handle(&mut code, next, Some(OUTPUT + 4096), DATA + 136);
        }
        with_handle(&mut code, close, None, DATA + 140);
        if mode == 3 {
            with_handle(&mut code, close, None, DATA + 144);
        }
    }
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored native CRT search", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    let provider = machine.ext.get::<Provider>();
    let searches = machine.ext.try_get::<Searches>();
    assert!(searches.is_none_or(|searches| {
        searches
            .namespace
            .is_none_or(|namespace| machine.sys.objects.alias_count(namespace) == 0)
    }));
    for pointer in &provider.allocations {
        assert!(
            machine.ext.get::<adv_os::Heaps>().owner(*pointer).is_none(),
            "mode {mode}: scratch owner was not released"
        );
    }
    if mode == 4 {
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(provider.closes, 1);
    } else {
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        if mode == 2 {
            assert_eq!(machine.cpu.read_u32(DATA + 128).unwrap(), u32::MAX);
            assert_eq!(provider.closes, 1);
        } else {
            assert_ne!(machine.cpu.read_u32(DATA + 128).unwrap(), provider.handle);
            assert_eq!(machine.cpu.read_u32(OUTPUT + 4).unwrap(), 0xcccccccc);
            assert_eq!(machine.cpu.read_u32(OUTPUT + 556).unwrap(), 0xcccccccc);
            if mode == 1 || mode == 6 {
                assert_eq!(machine.cpu.read_u32(DATA + 132).unwrap(), u32::MAX);
                assert_eq!(
                    machine.cpu.read_u32(DATA + 164).unwrap(),
                    if mode == 1 { 40 } else { 22 }
                );
                assert_eq!(machine.cpu.read_u32(OUTPUT + 4096).unwrap(), 0xcccccccc);
            } else {
                assert_eq!(machine.cpu.read_u32(DATA + 132).unwrap(), 0);
                assert_eq!(machine.cpu.read_u32(DATA + 136).unwrap(), u32::MAX);
                assert_eq!(
                    machine.cpu.read_u32(OUTPUT + 4096 + 32).unwrap(),
                    if mode == 5 { 17 } else { 18 }
                );
            }
            if mode == 3 {
                assert_eq!(machine.cpu.read_u32(DATA + 140).unwrap(), u32::MAX);
                assert_eq!(machine.cpu.read_u32(DATA + 144).unwrap(), 0);
            } else {
                assert_eq!(
                    machine.cpu.read_u32(DATA + 140).unwrap(),
                    if mode == 6 { u32::MAX } else { 0 }
                );
            }
            if mode == 6 {
                assert_eq!(machine.sys.objects.host::<u32>(provider.handle), Some(&79));
                assert_eq!(provider.queries, 1);
                assert_eq!(provider.closes, 0);
            } else {
                assert_eq!(provider.closes, if mode == 3 { 2 } else { 1 });
            }
            if mode != 5 {
                for (offset, expected) in [(8, 100), (16, 200), (24, 300)] {
                    let mut bytes = [0; 8];
                    machine.cpu.read(OUTPUT + offset, &mut bytes).unwrap();
                    assert_eq!(u64::from_le_bytes(bytes), expected);
                }
                let units: Vec<_> = (0..6)
                    .map(|index| machine.cpu.read_u16(OUTPUT + 36 + index * 2).unwrap())
                    .collect();
                assert_eq!(String::from_utf16(&units).unwrap(), "文件.dat");
            }
        }
        let warnings = machine.ext.try_get::<adv_abi::ApiDiagnostics>();
        if mode == 1 {
            assert!(warnings.is_some_and(|diagnostics| {
                diagnostics
                    .warnings
                    .keys()
                    .any(|(_, reason)| reason.contains("UINT32_MAX"))
            }));
        } else {
            assert!(warnings.is_none_or(|diagnostics| diagnostics.warnings.is_empty()));
        }
    }
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
// spec: PERS-BRIDGE-2
fn crt_searches_use_native_rows_callbacks_late_hooks_and_owner_identity() {
    for mode in 0..7 {
        guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
