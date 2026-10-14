//! Native copies consume provider bytes, short transfers and instance identities.
use super::*;
use crate::FileObj;
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
const SOURCE: u32 = 0x123abc00;
const TARGET: u32 = SOURCE + 4;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    source: Vec<u8>,
    target: Vec<u8>,
    source_at: u64,
    target_at: u64,
    closes: Vec<u32>,
    allocations: Vec<u32>,
    reads: usize,
    writes: usize,
    mode: u32,
    flushed: bool,
    deleted: bool,
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
    let access = env.arg(1);
    let disposition = env.arg(7);
    let object = env.arg(2);
    env.ext.get_mut::<Provider>().allocations.push(object);
    let handle = if access & (0x80000000 | 0x10000) != 0 {
        assert_eq!(disposition, 1);
        SOURCE
    } else {
        assert!(matches!(disposition, 2 | 3));
        TARGET
    };
    if handle == TARGET && disposition == 2 && env.ext.get::<Provider>().mode == 2 {
        return Outcome::Return(0xc0000035);
    }
    env.cpu.write_u32(env.arg(0), handle).unwrap();
    Outcome::Return(0)
}
fn open_existing(env: &mut Env<'_>) -> Outcome {
    assert!(env.arg(1) & 0x10000 != 0);
    env.cpu.write_u32(env.arg(0), SOURCE).unwrap();
    Outcome::Return(0)
}
fn query(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(4), 6);
    assert_eq!(env.arg(3), 8);
    let id = if env.arg(0) == SOURCE || env.ext.get::<Provider>().mode == 1 {
        7u64
    } else {
        8
    };
    env.cpu.write(env.arg(2), &id.to_le_bytes()).unwrap();
    Outcome::Return(0)
}
fn volume(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(4), 1);
    env.cpu.write_u32(env.arg(2) + 8, 79).unwrap();
    Outcome::Return(0)
}
fn set(env: &mut Env<'_>) -> Outcome {
    if env.arg(4) == 10 {
        assert_eq!(env.cpu.read_u32(env.arg(2)).unwrap() & 0xff, 1);
        return Outcome::Return(if env.ext.get::<Provider>().mode >= 7 {
            0
        } else {
            0xc00000d4
        });
    }
    if env.arg(4) == 64 {
        let provider = env.ext.get_mut::<Provider>();
        assert!(provider.flushed);
        provider.deleted = true;
        return Outcome::Return(if provider.mode == 6 { 0xc0000121 } else { 0 });
    }
    let mut bytes = [0; 8];
    env.cpu.read(env.arg(2), &mut bytes).unwrap();
    let value = u64::from_le_bytes(bytes);
    let handle = env.arg(0);
    let class = env.arg(4);
    let provider = env.ext.get_mut::<Provider>();
    match (handle, class) {
        (SOURCE, 14) => provider.source_at = value,
        (TARGET, 14) => provider.target_at = value,
        (TARGET, 20) => provider.target.resize(value as usize, 0),
        other => panic!("unexpected set {other:?}"),
    }
    Outcome::Return(0)
}
fn read(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), SOURCE);
    assert_eq!(env.arg(7), 0);
    assert!(env.arg(6) <= CHUNK);
    let (iosb, buffer, length) = (env.arg(4), env.arg(5), env.arg(6));
    let provider = env.ext.get_mut::<Provider>();
    provider.allocations.push(buffer - 48);
    let at = provider.source_at as usize;
    let count = (provider.source.len() - at)
        .min(length as usize)
        .min(if provider.reads == 0 { 13 } else { usize::MAX });
    provider.reads += 1;
    let bytes = provider.source[at..at + count].to_vec();
    provider.source_at += count as u64;
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu.write(buffer, &bytes).unwrap();
        env.cpu.write_u32(iosb + 4, count as u32).unwrap();
        Outcome::Return(if count == 0 { END_OF_FILE } else { 0 })
    })
}
fn write(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), TARGET);
    assert_eq!(env.arg(7), 0);
    if env.ext.get::<Provider>().mode == 3 {
        return Outcome::Return(0xc0000022);
    }
    let count = env.arg(6).min(5000);
    let mut bytes = vec![0; count as usize];
    env.cpu.read(env.arg(5), &mut bytes).unwrap();
    let provider = env.ext.get_mut::<Provider>();
    let at = provider.target_at as usize;
    if provider.target.len() < at + bytes.len() {
        provider.target.resize(at + bytes.len(), 0);
    }
    provider.target[at..at + bytes.len()].copy_from_slice(&bytes);
    provider.target_at += u64::from(count);
    provider.writes += 1;
    env.cpu.write_u32(env.arg(4) + 4, count).unwrap();
    Outcome::Return(0)
}
fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    env.ext.get_mut::<Provider>().closes.push(handle);
    Outcome::Return(0)
}
fn host_close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    assert!(env.sys.objects.host::<FileObj>(handle).is_some());
    env.ext.get_mut::<Provider>().closes.push(handle);
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        Outcome::Return(crate::ntdll::file::close(env, adv_abi::Handle(handle)) as u32)
    })
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

fn flush(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    let provider = env.ext.get_mut::<Provider>();
    assert!(!provider.deleted);
    if provider.mode >= 7 {
        assert_eq!(handle, SOURCE);
        assert!(provider.closes.is_empty());
    } else {
        assert_eq!(handle, TARGET);
        assert_eq!(provider.target, provider.source);
        assert_eq!(provider.closes, vec![SOURCE]);
    }
    provider.flushed = true;
    Outcome::Return(if matches!(provider.mode, 5 | 8) {
        0xc0000022
    } else {
        0
    })
}

fn guest<B: Backend>(cpu: B, mode: u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("open", 11, open as adv_abi::Handler),
        ("query", 5, query as adv_abi::Handler),
        ("volume", 5, volume as adv_abi::Handler),
        ("set", 5, set as adv_abi::Handler),
        ("read", 9, read as adv_abi::Handler),
        ("write", 9, write as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
        ("flush", 2, flush as adv_abi::Handler),
        ("open_existing", 6, open_existing as adv_abi::Handler),
    ] {
        registry.add(
            "authored-copy-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/copy")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/source.dat"), b"physical-source").unwrap();
    std::fs::write(root.join("game/target.dat"), b"physical-target").unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"source.dat\0").unwrap();
    machine.cpu.write(DATA + 64, b"target.dat\0").unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let payload: Vec<_> = (0..CHUNK + 17).map(|index| (index % 251) as u8).collect();
    let old = b"provider-old-target".to_vec();
    let mut provider = Provider {
        source: payload.clone(),
        target: old.clone(),
        mode,
        ..Default::default()
    };
    for (name, hook) in [
        ("NtCreateFile", "open"),
        ("NtQueryInformationFile", "query"),
        ("NtQueryVolumeInformationFile", "volume"),
        ("NtSetInformationFile", "set"),
        ("NtReadFile", "read"),
        ("NtWriteFile", "write"),
        ("NtClose", "close"),
        ("NtFlushBuffersFile", "flush"),
    ] {
        let entry = native(&mut machine, name);
        let target = machine
            .resolve_import("authored-copy-provider", hook)
            .unwrap();
        provider.hooks.push((entry, target));
    }
    if mode >= 4 {
        let entry = native(&mut machine, "NtOpenFile");
        let target = machine
            .resolve_import("authored-copy-provider", "open_existing")
            .unwrap();
        provider.hooks.push((entry, target));
    }
    machine.ext.insert(provider);
    let mut code = Asm::new(CODE);
    code.call(
        machine
            .resolve_import("authored-copy-provider", "install")
            .unwrap(),
    );
    code.push_imm(if mode >= 4 { 11 } else { u32::from(mode == 2) })
        .push_imm(DATA + 64)
        .push_imm(DATA)
        .call(
            machine
                .resolve_import(
                    "kernel32.dll",
                    if mode >= 4 {
                        "MoveFileExA"
                    } else {
                        "CopyFileA"
                    },
                )
                .unwrap(),
        )
        .mov_mem_eax(DATA + 128);
    code.call(
        machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap(),
    )
    .mov_mem_eax(DATA + 132);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored native copy", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let provider = machine.ext.get::<Provider>();
    assert!(machine.sys.objects.get(SOURCE).is_none() && machine.sys.objects.get(TARGET).is_none());
    if mode >= 7 {
        assert_eq!(
            machine.cpu.read_u32(DATA + 128).unwrap(),
            u32::from(mode == 7)
        );
        if mode == 8 {
            assert_eq!(machine.cpu.read_u32(DATA + 132).unwrap(), 5);
        }
        assert_eq!(provider.reads, 0);
        assert_eq!(provider.writes, 0);
        assert!(provider.flushed && !provider.deleted);
    } else if mode == 0 || matches!(mode, 4 | 6) {
        assert_eq!(machine.cpu.read_u32(DATA + 128).unwrap(), 1);
        assert_eq!(provider.target, payload);
        assert!(provider.reads >= 3 && provider.writes > 2);
    } else {
        assert_eq!(machine.cpu.read_u32(DATA + 128).unwrap(), 0);
        assert_eq!(
            machine.cpu.read_u32(DATA + 132).unwrap(),
            match mode {
                1 => 32,
                2 => 80,
                3 | 5 => 5,
                _ => unreachable!(),
            }
        );
        if mode == 5 {
            assert_eq!(provider.target, payload);
        } else if mode != 3 {
            assert_eq!(provider.target, old);
        }
    }
    assert_eq!(
        provider.closes,
        if mode >= 7 {
            vec![SOURCE]
        } else if mode >= 4 {
            assert!(provider.flushed);
            assert_eq!(provider.deleted, mode != 5);
            if mode == 5 {
                vec![SOURCE, TARGET, SOURCE]
            } else {
                vec![SOURCE, TARGET, SOURCE, SOURCE]
            }
        } else if mode == 2 {
            vec![SOURCE]
        } else {
            vec![TARGET, SOURCE]
        }
    );
    for pointer in &provider.allocations {
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, *pointer)
                .is_none()
        );
    }
    assert_eq!(
        std::fs::read(root.join("game/source.dat")).unwrap(),
        b"physical-source"
    );
    assert_eq!(
        std::fs::read(root.join("game/target.dat")).unwrap(),
        b"physical-target"
    );
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|diagnostics| diagnostics.warnings.is_empty())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn copies_use_live_nt_bytes_and_close_both_owners_on_alias_collision_and_write_failures() {
    for mode in 0..4 {
        guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}

// spec: HOST-SAVE-4
#[test]
fn move_copy_observes_native_flush_before_delete_and_propagates_flush_failure() {
    for mode in 4..9 {
        guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}

fn host_copy<B: Backend>(cpu: B) {
    use std::io::{Read as _, Write as _};
    const CHUNKS: u32 = 1024;
    const TAIL: usize = 17;

    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add(
        "authored-copy-provider",
        "install",
        Conv::Stdcall { argc: 0 },
        install,
    );
    registry.add(
        "authored-copy-provider",
        "host_close",
        Conv::Stdcall { argc: 1 },
        host_close,
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/copy")
        .join(format!(
            "host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let payload: Vec<_> = (0..CHUNK).map(|index| (index % 251) as u8).collect();
    let mut source = std::fs::File::create(root.join("game/source.dat")).unwrap();
    for _ in 0..CHUNKS {
        source.write_all(&payload).unwrap();
    }
    source.write_all(&payload[..TAIL]).unwrap();
    drop(source);
    std::fs::write(root.join("game/target.dat"), b"old-target").unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"source.dat\0").unwrap();
    machine.cpu.write(DATA + 64, b"target.dat\0").unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let entry = native(&mut machine, "NtClose");
    let target = machine
        .resolve_import("authored-copy-provider", "host_close")
        .unwrap();
    machine.ext.insert(Provider {
        hooks: vec![(entry, target)],
        ..Default::default()
    });
    let mut code = Asm::new(CODE);
    code.call(
        machine
            .resolve_import("authored-copy-provider", "install")
            .unwrap(),
    );
    code.push_imm(0)
        .push_imm(DATA + 64)
        .push_imm(DATA)
        .call(machine.resolve_import("kernel32.dll", "CopyFileA").unwrap())
        .mov_mem_eax(DATA + 128);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored synchronous native copy", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 128).unwrap(), 1);
    let provider = machine.ext.get::<Provider>();
    assert_eq!(provider.closes.len(), 2);
    assert_ne!(provider.closes[0], provider.closes[1]);
    for handle in &provider.closes {
        assert!(machine.sys.objects.get(*handle).is_none());
    }
    let target = machine.ext.get::<Win32>().vfs.lookup("target.dat").unwrap();
    let mut target = std::fs::File::open(target).unwrap();
    assert_eq!(
        target.metadata().unwrap().len(),
        u64::from(CHUNKS) * u64::from(CHUNK) + TAIL as u64
    );
    let mut bytes = vec![0; CHUNK as usize];
    for _ in 0..CHUNKS {
        target.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, payload);
    }
    target.read_exact(&mut bytes[..TAIL]).unwrap();
    assert_eq!(bytes[..TAIL], payload[..TAIL]);
    assert_eq!(target.read(&mut bytes).unwrap(), 0);
    drop(target);
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|diagnostics| diagnostics.warnings.is_empty())
    );
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn large_copies_with_only_nt_close_hook_use_bounded_host_stack() {
    std::thread::Builder::new()
        .name("bounded-native-copy".into())
        .stack_size(512 * 1024)
        .spawn(|| {
            host_copy(JitCpu::new(PARK_ADDR, false).unwrap());
            host_copy(UnicornCpu::new(PARK_ADDR).unwrap());
        })
        .unwrap()
        .join()
        .unwrap();
}
