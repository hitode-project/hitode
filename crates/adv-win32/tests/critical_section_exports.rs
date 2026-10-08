//! Authored RTL/Win32 critical-section export identity and result checks.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules, Process};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

fn export(machine: &mut Machine<JitCpu>, module: usize, name: &str) -> u32 {
    Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
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

#[test]
fn rtl_forwarded_entries_preserve_shared_state_and_success_status() {
    const CODE: u32 = 0x400000;
    const DATA: u32 = 0x410000;
    const SECTION: u32 = DATA + 0x100;
    for jit in [false, true] {
        for guest_code in [false, true] {
            let mut registry = Registry::default();
            adv_win32::register(&mut registry);
            let mut machine = Machine::new(
                JitCpu::new(PARK_ADDR, jit).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            Win32::install(
                &mut machine,
                Win32Config::new(
                    "/nonexistent/critical-probe",
                    "probe.exe",
                    "/nonexistent/critical-probe-data",
                ),
            )
            .unwrap();
            let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
            let native = machine.load(&Empty, "ntdll.dll").unwrap();
            assert_eq!(
                export(&mut machine, kernel, "EnterCriticalSection"),
                export(&mut machine, native, "RtlEnterCriticalSection")
            );
            machine
                .sys
                .thunks
                .set_guest_code(&mut machine.cpu, guest_code)
                .unwrap();
            machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
            machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
            let mut code = Asm::new(CODE);
            for (index, name) in [
                "RtlInitializeCriticalSection",
                "RtlEnterCriticalSection",
                "RtlEnterCriticalSection",
                "RtlLeaveCriticalSection",
                "RtlLeaveCriticalSection",
                "RtlTryEnterCriticalSection",
                "RtlLeaveCriticalSection",
                "RtlDeleteCriticalSection",
            ]
            .iter()
            .enumerate()
            {
                let entry = export(&mut machine, native, name);
                code.push_imm(SECTION)
                    .call(entry)
                    .mov_mem_eax(DATA + index as u32 * 4);
            }
            code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            machine.cpu.write(CODE, &code.bytes).unwrap();
            machine.start_main(CODE, CODE, 65536, "probe.exe").unwrap();
            assert!(matches!(machine.run_virtual(100000), RunResult::Exited(0)));
            let results: Vec<_> = (0..8)
                .map(|index| machine.cpu.read_u32(DATA + index * 4).unwrap())
                .collect();
            assert_eq!(results, [0, 0, 0, 0, 0, 1, 0, 0]);
            assert_eq!(machine.cpu.read_u32(SECTION + 8).unwrap(), 0);
        }
    }
}
