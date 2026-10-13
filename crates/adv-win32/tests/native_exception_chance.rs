//! Exhausted Windows handler search must reach the observable NT second chance.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_win32::{Win32, Win32Config};

#[test]
// spec: WIN32-EXC-7
fn unhandled_first_chance_reenters_live_nt_raise_once_with_false() {
    for (jit, observed) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            Win32Config::new(
                "/nonexistent/nt-chance",
                "chance.exe",
                "/nonexistent/nt-chance-data",
            ),
        )
        .unwrap();
        Win32::prepare_process_infrastructure(&mut m).unwrap();
        const CODE: u32 = 0x400000;
        const DATA: u32 = 0x410000;
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let original = m.resolve_import("ntdll.dll", "NtRaiseException").unwrap();
        let body = m
            .sys
            .thunks
            .pristine_entry((original - THUNK_BASE) / THUNK_STRIDE);
        m.cpu.write(CODE + 0x800, &body).unwrap();
        let mut hook = Asm::new(CODE + 0x400);
        hook.mov_eax_esp(12);
        // Increment separate FALSE/TRUE counters, then execute the original entry.
        hook.bytes
            .extend_from_slice(&[0x83, 0xe0, 1, 0xc1, 0xe0, 2, 0xff, 0x80]);
        hook.bytes.extend_from_slice(&DATA.to_le_bytes());
        hook.jmp(CODE + 0x800);
        m.cpu.write(CODE + 0x400, &hook.bytes).unwrap();
        let modules = adv_os::modules(&m.ext);
        let nt = modules.get(modules.by_name("ntdll.dll").unwrap());
        let entry = match nt.exports_by_name["NtRaiseException"] {
            adv_pe::ExportTarget::Rva(rva) => nt.base + rva,
            _ => panic!("native entry"),
        };
        let mut patch = Asm::new(entry);
        patch.jmp(CODE + 0x400);
        if observed {
            m.cpu.write(entry, &patch.bytes).unwrap();
        }
        let raise = m.resolve_import("kernel32.dll", "RaiseException").unwrap();
        // A declining VEH changes the record seen by the subsequent NT service.
        let mut handler = Asm::new(CODE + 0xc00);
        handler.mov_eax_esp(4).mov_eax_ptr_eax();
        handler.bytes.extend_from_slice(&[0xc7, 0x00]);
        handler
            .bytes
            .extend_from_slice(&0xe0421002u32.to_le_bytes());
        handler.bytes.extend_from_slice(&[0x31, 0xc0]);
        handler.ret_n(4);
        m.cpu.write(CODE + 0xc00, &handler.bytes).unwrap();
        let add = m
            .resolve_import("kernel32.dll", "AddVectoredExceptionHandler")
            .unwrap();
        let mut code = Asm::new(CODE);
        code.push_imm(CODE + 0xc00).push_imm(1).call(add);
        code.push_imm(0)
            .push_imm(0)
            .push_imm(0)
            .push_imm(0xe0421001)
            .call(raise)
            .hlt();
        m.cpu.write(CODE, &code.bytes).unwrap();
        m.spawn("unhandled caller", CODE, &[], 65536).unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(m.sys.guest_failure_code, Some(0xe0421002));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::from(observed));
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), u32::from(observed));
    }
}
