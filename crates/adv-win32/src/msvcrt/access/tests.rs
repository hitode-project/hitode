//! Public CRT access fronts consume native provider attributes and errors.
use super::*;
use crate::Win32;
use adv_abi::Conv;
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
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Provider {
    entry: u32,
    target: u32,
    full: bool,
    calls: usize,
    allocations: Vec<u32>,
}
fn install(env: &mut Env<'_>) -> Outcome {
    let provider = env.ext.get::<Provider>();
    let mut jump = Asm::new(provider.entry);
    jump.jmp(provider.target);
    env.cpu.write(provider.entry, &jump.bytes).unwrap();
    Outcome::Return(0)
}
fn query(env: &mut Env<'_>) -> Outcome {
    let (object, output) = (env.arg(0), env.arg(1));
    let descriptor = env.cpu.read_u32(object + 8).unwrap();
    let length = u32::from(env.cpu.read_u16(descriptor).unwrap());
    let pointer = env.cpu.read_u32(descriptor + 4).unwrap();
    let units: Vec<_> = (0..length / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), r"\??\C:\Game\文件.dat");
    let provider = env.ext.get_mut::<Provider>();
    provider.allocations.push(object);
    let call = provider.calls;
    provider.calls += 1;
    let offset = if provider.full { 48 } else { 32 };
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        let attributes = match call {
            0 => 0x20,
            1 => 0x21,
            _ => 0x11,
        };
        env.cpu.write_u32(output + offset, attributes).unwrap();
        Outcome::Return(match call {
            2 => 0xc0000022,
            3 => 0xc0000034,
            _ => 0,
        })
    })
}

fn guest<B: Backend>(cpu: B, full: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    registry.add(
        "authored-access-provider",
        "install",
        Conv::Stdcall { argc: 0 },
        install,
    );
    registry.add(
        "authored-access-provider",
        "query",
        Conv::Stdcall { argc: 2 },
        query,
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/access")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut config = crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data"));
    config.os.codepage = 936;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let bytes = machine.ext.get::<Win32>().codec.encode("文件.dat");
    machine
        .cpu
        .write(DATA, &[bytes.as_slice(), &[0]].concat())
        .unwrap();
    let units: Vec<_> = "文件.dat"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(DATA + 64, &units).unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let module = modules.by_name("ntdll.dll").unwrap();
    let entry = Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            module,
            &ImportName::Name {
                hint: 0,
                name: if full {
                    "NtQueryFullAttributesFile"
                } else {
                    "NtQueryAttributesFile"
                }
                .into(),
            },
        )
        .unwrap()
        .0;
    let target = machine
        .resolve_import("authored-access-provider", "query")
        .unwrap();
    machine.ext.insert(Provider {
        entry,
        target,
        full,
        calls: 0,
        allocations: Vec::new(),
    });
    let install = machine
        .resolve_import("authored-access-provider", "install")
        .unwrap();
    let access = machine.resolve_import("msvcrt.dll", "_access").unwrap();
    let waccess = machine.resolve_import("msvcrt.dll", "_waccess").unwrap();
    let errno = machine.resolve_import("msvcrt.dll", "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(install);
    for index in 0..5 {
        code.call(errno);
        code.bytes.extend([0xc7, 0x00]);
        code.bytes.extend(79u32.to_le_bytes());
        code.bytes.extend([0xc7, 0x40, 4]);
        code.bytes.extend(91u32.to_le_bytes());
        code.push_imm(0x12345678).call(set_error);
        code.push_imm(2)
            .push_imm(if index % 2 == 0 { DATA } else { DATA + 64 });
        code.call(if index % 2 == 0 { access } else { waccess })
            .add_esp(8);
        let output = DATA + 256 + index * 16;
        code.mov_mem_eax(output)
            .call(errno)
            .mov_eax_ptr_eax()
            .mov_mem_eax(output + 4);
        code.call(errno);
        code.bytes.extend([0x83, 0xc0, 4]);
        code.mov_eax_ptr_eax().mov_mem_eax(output + 8);
        code.call(get_error).mov_mem_eax(output + 12);
    }
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored native CRT access", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, expected) in [
        [0, 79, 91, 0x12345678],
        [u32::MAX, 13, 5, 0x12345678],
        [u32::MAX, 13, 5, 5],
        [u32::MAX, 2, 2, 2],
        [0, 79, 91, 0x12345678],
    ]
    .into_iter()
    .enumerate()
    {
        for (field, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                machine
                    .cpu
                    .read_u32(DATA + 256 + index as u32 * 16 + field as u32 * 4)
                    .unwrap(),
                expected,
                "result {index}/{field}"
            );
        }
    }
    let provider = machine.ext.get::<Provider>();
    assert_eq!(provider.calls, 5);
    for allocation in &provider.allocations {
        assert!(
            machine
                .ext
                .get::<adv_os::Heaps>()
                .owner(&machine.cpu, *allocation)
                .is_none()
        );
    }
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
fn access_uses_live_basic_and_full_attribute_results_with_cp936_paths_and_errno() {
    for full in [false, true] {
        guest(JitCpu::new(PARK_ADDR, false).unwrap(), full);
        guest(UnicornCpu::new(PARK_ADDR).unwrap(), full);
    }
}
