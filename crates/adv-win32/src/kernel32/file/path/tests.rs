//! Public path fronts diagnose remaining bypasses, and profiles honor close hooks.
use super::*;
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
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Provider {
    mode: u32,
    entry: u32,
    target: u32,
    calls: usize,
    handle: u32,
    warnings: Vec<usize>,
}
fn install(env: &mut Env<'_>) -> Outcome {
    let provider = env.ext.get::<Provider>();
    let mut jump = Asm::new(provider.entry);
    jump.jmp(provider.target);
    env.cpu.write(provider.entry, &jump.bytes).unwrap();
    Outcome::Return(0)
}
fn capture(env: &mut Env<'_>) -> Outcome {
    let count = env
        .ext
        .try_get::<adv_abi::ApiDiagnostics>()
        .map_or(0, |diagnostics| diagnostics.warnings.len());
    env.ext.get_mut::<Provider>().warnings.push(count);
    Outcome::Return(0)
}
fn rejected_create(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Provider>().calls += 1;
    Outcome::Return(0xc0000022)
}
fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    assert!(env.sys.objects.host::<FileObj>(handle).is_some());
    let provider = env.ext.get_mut::<Provider>();
    provider.calls += 1;
    provider.handle = handle;
    let fail = provider.mode == 2;
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        Outcome::Return(if fail {
            0xc0000022
        } else {
            crate::ntdll::file::close(env, Handle(handle)) as u32
        })
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
fn call(code: &mut Asm, target: u32, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target).mov_mem_eax(result);
}
fn guest<B: Backend>(cpu: B, mode: u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("capture", 0, capture as adv_abi::Handler),
        ("create", 11, rejected_create as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
    ] {
        registry.add(
            "authored-path-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/path")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::create_dir_all(root.join("data/overlay/folder")).unwrap();
    std::fs::write(
        root.join("game/profile.ini"),
        b"[main]\r\nname=provider\r\n",
    )
    .unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUTPUT, 4096, Prot::RW).unwrap();
    for (offset, value) in [
        (0, "folder"),
        (32, "missing"),
        (64, r"C:\Game"),
        (128, "TST"),
        (256, "main"),
        (288, "name"),
        (320, "fallback"),
        (352, r"C:\Game\profile.ini"),
    ] {
        machine
            .cpu
            .write(DATA + offset, &[value.as_bytes(), &[0]].concat())
            .unwrap();
    }
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let entry = native(
        &mut machine,
        if mode == 0 { "NtCreateFile" } else { "NtClose" },
    );
    let target = machine
        .resolve_import(
            "authored-path-provider",
            if mode == 0 { "create" } else { "close" },
        )
        .unwrap();
    machine.ext.insert(Provider {
        mode,
        entry,
        target,
        calls: 0,
        handle: 0,
        warnings: Vec::new(),
    });
    let mut code = Asm::new(CODE);
    code.call(
        machine
            .resolve_import("authored-path-provider", "install")
            .unwrap(),
    );
    if mode == 0 {
        let remove = machine.resolve_import(K32, "RemoveDirectoryA").unwrap();
        let temp = machine.resolve_import(K32, "GetTempFileNameA").unwrap();
        let capture = machine
            .resolve_import("authored-path-provider", "capture")
            .unwrap();
        call(&mut code, remove, &[DATA + 32], DATA + 512);
        call(
            &mut code,
            temp,
            &[DATA + 64, DATA + 128, 79, OUTPUT],
            DATA + 516,
        );
        code.call(capture);
        call(
            &mut code,
            temp,
            &[DATA + 64, DATA + 128, 0, OUTPUT],
            DATA + 520,
        );
        code.call(capture);
        call(&mut code, remove, &[DATA], DATA + 524);
        code.call(capture);
        call(&mut code, remove, &[DATA], DATA + 528);
        code.call(capture);
    } else {
        let profile = machine
            .resolve_import(K32, "GetPrivateProfileStringA")
            .unwrap();
        call(
            &mut code,
            profile,
            &[DATA + 256, DATA + 288, DATA + 320, OUTPUT, 260, DATA + 352],
            DATA + 512,
        );
    }
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored native path boundary", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let provider = machine.ext.get::<Provider>();
    if mode == 0 {
        assert_eq!(provider.calls, 0);
        assert_eq!(provider.warnings, [0, 1, 2, 2]);
        assert_eq!(machine.cpu.read_u32(DATA + 512).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + 516).unwrap(), 79);
        assert_ne!(machine.cpu.read_u32(DATA + 520).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + 524).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(DATA + 528).unwrap(), 0);
        assert!(!root.join("data/overlay/folder").exists());
        let path = String::from_utf8(machine.cpu.read_cstr(OUTPUT, 260).unwrap()).unwrap();
        assert!(machine.ext.get::<Win32>().vfs.lookup(&path).is_some());
        let warnings = &machine.ext.get::<adv_abi::ApiDiagnostics>().warnings;
        assert!(
            warnings
                .keys()
                .any(|(api, _)| api == "kernel32.dll!GetTempFileNameA")
        );
        assert!(
            warnings
                .keys()
                .any(|(api, _)| api == "kernel32.dll!RemoveDirectory")
        );
    } else {
        assert_eq!(provider.calls, 1);
        assert_eq!(machine.cpu.read_u32(DATA + 512).unwrap(), 8);
        assert_eq!(
            machine.cpu.read_cstr(OUTPUT, 260).unwrap(),
            if mode == 1 { b"provider" } else { b"fallback" }
        );
        assert_eq!(
            machine.sys.objects.get(provider.handle).is_none(),
            mode == 1
        );
        assert!(
            machine
                .ext
                .try_get::<adv_abi::ApiDiagnostics>()
                .is_none_or(|diagnostics| diagnostics.warnings.is_empty())
        );
    }
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remaining_path_mutations_diagnose_bypasses_and_profile_reads_route_close_only_hooks() {
    for mode in 0..3 {
        guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
