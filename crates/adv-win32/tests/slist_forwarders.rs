//! Authored x86 SList entries traverse the fixed Win32-to-RTL forwarders.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HEAD: u32 = DATA + 0x200;
const FIRST: u32 = DATA + 0x210;
const SECOND: u32 = DATA + 0x220;
fn public(machine: &mut Machine<JitCpu>, module: &str, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let id = modules.by_name(module).unwrap();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            id,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0
}
fn call(code: &mut Asm, entry: u32, args: &[u32], output: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(entry).mov_mem_eax(output);
}
#[test]
fn fixed_slist_forwarders_resolve_to_rtl_and_share_initialize_push_pop_flush_and_depth() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        for (name, argc) in [
            ("RtlInitializeSListHead", 1),
            ("RtlInterlockedPushEntrySList", 2),
            ("RtlInterlockedPopEntrySList", 1),
            ("RtlInterlockedFlushSList", 1),
            ("RtlQueryDepthSList", 1),
        ] {
            assert_eq!(
                registry.get("ntdll.dll", name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("ntdll.dll", name));
        }
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/slist-forwarders",
                "slist.exe",
                "/nonexistent/slist-forwarders-data",
            ),
        )
        .unwrap();
        Win32::prepare_process_infrastructure(&mut machine).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let mut entries = vec![];
        for (front, native) in [
            ("InitializeSListHead", "RtlInitializeSListHead"),
            ("InterlockedPushEntrySList", "RtlInterlockedPushEntrySList"),
            ("InterlockedPopEntrySList", "RtlInterlockedPopEntrySList"),
            ("InterlockedFlushSList", "RtlInterlockedFlushSList"),
            ("QueryDepthSList", "RtlQueryDepthSList"),
        ] {
            let kernel = public(&mut machine, "kernel32.dll", front);
            let rtl = public(&mut machine, "ntdll.dll", native);
            assert_eq!(
                kernel, rtl,
                "the catalog forwards to the implemented native entry"
            );
            entries.push((kernel, rtl));
        }
        let mut code = Asm::new(CODE);
        call(&mut code, entries[0].0, &[HEAD], DATA);
        call(&mut code, entries[4].1, &[HEAD], DATA + 4);
        call(&mut code, entries[1].0, &[HEAD, FIRST], DATA + 8);
        call(&mut code, entries[4].1, &[HEAD], DATA + 12);
        call(&mut code, entries[1].1, &[HEAD, SECOND], DATA + 16);
        call(&mut code, entries[4].0, &[HEAD], DATA + 20);
        call(&mut code, entries[2].0, &[HEAD], DATA + 24);
        call(&mut code, entries[4].1, &[HEAD], DATA + 28);
        call(&mut code, entries[3].1, &[HEAD], DATA + 32);
        call(&mut code, entries[4].0, &[HEAD], DATA + 36);
        call(&mut code, entries[1].1, &[HEAD, FIRST], DATA + 40);
        call(&mut code, entries[1].0, &[HEAD, SECOND], DATA + 44);
        call(&mut code, entries[3].0, &[HEAD], DATA + 48);
        call(&mut code, entries[4].1, &[HEAD], DATA + 52);
        call(&mut code, entries[2].1, &[HEAD], DATA + 56);
        code.push_imm(0).call(
            machine
                .resolve_import("kernel32.dll", "ExitProcess")
                .unwrap(),
        );
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine.spawn("SList caller", CODE, &[], 0x10000).unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        let outputs: Vec<_> = (1..15)
            .map(|i| machine.cpu.read_u32(DATA + i * 4).unwrap())
            .collect();
        assert_eq!(
            outputs,
            [
                0, 0, 1, FIRST, 2, SECOND, 1, FIRST, 0, 0, FIRST, SECOND, 0, 0
            ]
        );
        assert_eq!(machine.cpu.read_u32(HEAD).unwrap(), 0);
        assert_eq!(machine.cpu.read_u16(HEAD + 4).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(SECOND).unwrap(), FIRST);
        assert_eq!(machine.cpu.read_u32(FIRST).unwrap(), 0);
    }
}
