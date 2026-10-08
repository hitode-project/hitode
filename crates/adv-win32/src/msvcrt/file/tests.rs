//! Authored CRT/Win32 streams share a cursor and obey live NT/RTL callbacks.
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
const FILE: u32 = DATA + 3072;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    cursor: u32,
    path: String,
    reads: u32,
    closes: u32,
    frees: u32,
    protect_output: bool,
}

fn install(env: &mut Env<'_>) -> Outcome {
    for &(entry, target) in &env.ext.get::<Provider>().hooks {
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}
fn original_handle(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(
        env.ext
            .get::<CrtState>()
            .files
            .get(&env.arg(0))
            .unwrap()
            .handle,
    )
}
fn open(env: &mut Env<'_>) -> Outcome {
    let descriptor = env.cpu.read_u32(env.arg(2) + 8).unwrap();
    let length = u32::from(env.cpu.read_u16(descriptor).unwrap());
    let pointer = env.cpu.read_u32(descriptor + 4).unwrap();
    let units: Vec<_> = (0..length / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    env.ext.get_mut::<Provider>().path = String::from_utf16(&units).unwrap();
    assert_eq!(env.arg(7), 1);
    assert_eq!(env.arg(8), 0x60);
    env.cpu.write_u32(env.arg(0), HANDLE).unwrap();
    env.cpu.write_u32(env.arg(3) + 4, 1).unwrap();
    Outcome::Return(0)
}
fn read(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    assert_eq!(env.arg(7), 0);
    let (iosb, output, size) = (env.arg(4), env.arg(5), env.arg(6));
    let cursor = env.ext.get::<Provider>().cursor;
    let count = size
        .min(4 - cursor)
        .min(if cursor == 0 { 1 } else { u32::MAX });
    env.ext.get_mut::<Provider>().cursor += count;
    env.ext.get_mut::<Provider>().reads += 1;
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu
            .write(output, &b"live"[cursor as usize..(cursor + count) as usize])
            .unwrap();
        env.cpu.write_u32(iosb + 4, count).unwrap();
        Outcome::Return(if count == 0 { 0xc0000011 } else { 0 })
    })
}
fn query(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    assert_eq!(env.arg(4), 14);
    env.cpu
        .write(
            env.arg(2),
            &u64::from(env.ext.get::<Provider>().cursor).to_le_bytes(),
        )
        .unwrap();
    Outcome::Return(0)
}
fn close(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    env.ext.get_mut::<Provider>().closes += 1;
    Outcome::Return(0)
}
fn allocate(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), env.ext.get::<Heaps>().process_heap);
    assert_eq!((env.arg(1), env.arg(2)), (8, 32));
    env.cpu.write(FILE, &[0; 32]).unwrap();
    if env.ext.get::<Provider>().protect_output {
        env.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    }
    Outcome::Return(FILE)
}
fn free(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(2), FILE);
    assert!(!env.ext.get::<CrtState>().files.contains_key(&FILE));
    env.ext.get_mut::<Provider>().frees += 1;
    Outcome::Return(1)
}

fn native<B: Backend>(machine: &mut Machine<B>, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<crate::Win32, Modules>();
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
fn push(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}

fn guest<B: Backend>(cpu: B, module: &str, live: bool, wide: bool, protect_output: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("handle", 1, original_handle as adv_abi::Handler),
        ("open", 11, open as adv_abi::Handler),
        ("read", 9, read as adv_abi::Handler),
        ("query", 5, query as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
        ("allocate", 3, allocate as adv_abi::Handler),
        ("free", 3, free as adv_abi::Handler),
    ] {
        registry.add(
            "authored-crt-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/crt")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/文件.dat"), b"host").unwrap();
    let mut config = crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data"));
    config.os.codepage = 936;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let mut name = machine.ext.get::<crate::Win32>().codec.encode("文件.dat");
    name.push(0);
    machine.cpu.write(DATA, &name).unwrap();
    machine
        .cpu
        .write(
            DATA + 64,
            &"文件.dat"
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    machine.cpu.write(DATA + 128, b"rb\0").unwrap();
    machine
        .cpu
        .write(DATA + 160, &[b'r', 0, b'b', 0, 0, 0])
        .unwrap();
    let mut provider = Provider {
        protect_output,
        ..Default::default()
    };
    if live {
        let (win32, modules) = machine.ext.get2_mut::<crate::Win32, Modules>();
        Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
            .load("ntdll.dll")
            .unwrap();
        for (name, hook) in [
            ("NtCreateFile", "open"),
            ("NtReadFile", "read"),
            ("NtQueryInformationFile", "query"),
            ("NtClose", "close"),
            ("RtlAllocateHeap", "allocate"),
            ("RtlFreeHeap", "free"),
        ] {
            let entry = native(&mut machine, name);
            let target = machine
                .resolve_import("authored-crt-provider", hook)
                .unwrap();
            provider.hooks.push((entry, target));
        }
    }
    machine.ext.insert(provider);
    let open = machine
        .resolve_import(
            module,
            if protect_output {
                "fopen_s"
            } else if module == "ucrtbase.dll" {
                "_wfopen_s"
            } else if wide {
                "_wfopen"
            } else {
                "fopen"
            },
        )
        .unwrap();
    let read = machine.resolve_import(module, "fread").unwrap();
    let tell = machine.resolve_import(module, "ftell").unwrap();
    let close = machine.resolve_import(module, "fclose").unwrap();
    let winread = machine.resolve_import("kernel32.dll", "ReadFile").unwrap();
    let original = machine
        .resolve_import("authored-crt-provider", "handle")
        .unwrap();
    let mut code = Asm::new(CODE);
    if live {
        code.call(
            machine
                .resolve_import("authored-crt-provider", "install")
                .unwrap(),
        );
    }
    code.push_imm(DATA + if wide { 160 } else { 128 })
        .push_imm(DATA + if wide { 64 } else { 0 });
    if module == "ucrtbase.dll" || protect_output {
        code.push_imm(DATA + 256).call(open).add_esp(12);
    } else {
        code.call(open).add_esp(8).mov_mem_eax(DATA + 256);
    }
    push(&mut code, DATA + 256);
    code.push_imm(2)
        .push_imm(1)
        .push_imm(DATA + 512)
        .call(read)
        .add_esp(16)
        .mov_mem_eax(DATA + 260);
    push(&mut code, DATA + 256);
    code.call(original).mov_mem_eax(DATA + 280);
    code.push_imm(0)
        .push_imm(DATA + 288)
        .push_imm(2)
        .push_imm(DATA + 514);
    push(&mut code, DATA + 280);
    code.call(winread).mov_mem_eax(DATA + 264);
    push(&mut code, DATA + 256);
    code.call(tell).add_esp(4).mov_mem_eax(DATA + 268);
    push(&mut code, DATA + 256);
    code.call(close).add_esp(4).mov_mem_eax(DATA + 272);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored CRT file flow", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    if protect_output {
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert!(machine.ext.get::<CrtState>().files.is_empty());
        let provider = machine.ext.get::<Provider>();
        assert_eq!((provider.closes, provider.frees), (1, 1));
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 260).unwrap(), 2);
    assert_eq!(machine.cpu.read_u32(DATA + 264).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 268).unwrap(), 4);
    assert_eq!(machine.cpu.read_u32(DATA + 272).unwrap(), 0);
    let mut bytes = [0; 4];
    machine.cpu.read(DATA + 512, &mut bytes).unwrap();
    assert_eq!(&bytes, if live { b"live" } else { b"host" });
    assert!(machine.ext.get::<CrtState>().files.is_empty());
    if live {
        let provider = machine.ext.get::<Provider>();
        assert!(provider.path.ends_with("\\文件.dat"));
        assert_eq!((provider.reads, provider.closes, provider.frees), (3, 1, 1));
        assert_eq!(machine.cpu.read_u32(DATA + 256).unwrap(), FILE);
        assert!(machine.sys.objects.get(HANDLE).is_none());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cp936_crt_streams_share_win32_cursors_and_resume_nt_rtl_callbacks() {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll", "ucrtbase.dll"] {
        for live in [false, true] {
            for wide in [false, true] {
                if module == "ucrtbase.dll" && !wide {
                    continue;
                }
                guest(
                    JitCpu::new(PARK_ADDR, false).unwrap(),
                    module,
                    live,
                    wide,
                    false,
                );
                guest(
                    UnicornCpu::new(PARK_ADDR).unwrap(),
                    module,
                    live,
                    wide,
                    false,
                );
            }
        }
    }
}

#[test]
fn secure_open_final_output_fault_rolls_back_the_original_nt_and_rtl_owners() {
    guest(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        "msvcrt.dll",
        true,
        false,
        true,
    );
    guest(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        "msvcrt.dll",
        true,
        false,
        true,
    );
}
