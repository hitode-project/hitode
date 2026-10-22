//! Versioned packet fields and elapsed clocks use the same virtual Windows time.
use adv_abi::{Registry, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
use std::collections::BTreeSet;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn machine(jit: bool, milliseconds: u64) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-time-packet",
            "fixture.exe",
            "/nonexistent/crt-time-packet-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 65536, Prot::RX).unwrap();
    m.cpu.map(DATA, 65536, Prot::RW).unwrap();
    let epoch = m.sys.clock.epoch_us;
    m.sys.clock.advance_to(epoch + milliseconds * 1000);
    m
}
fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(m.resolve_import(module, name).unwrap());
    if !args.is_empty() {
        a.add_esp(args.len() as u8 * 4);
    }
    a.mov_mem_eax(output);
}
fn execute(m: &mut Machine<JitCpu>, mut a: Asm) -> RunResult {
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT time packet", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn no_gaps(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn packet(name: &str) -> bool {
    let n = name.strip_prefix("_o_").unwrap_or(name);
    matches!(
        n,
        "_ftime" | "_ftime32" | "_ftime64" | "_ftime32_s" | "_ftime64_s"
    )
}

// spec: CRT-LIB-26
#[test]
fn every_packet_version_has_native_fields_timezone_sign_and_untouched_padding() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| packet(s.name))
        .collect();
    assert_eq!(declarations.len(), 42);
    let modules: BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    for jit in [false, true] {
        for module in &modules {
            let mut m = machine(jit, 7654);
            let mut a = Asm::new(CODE);
            let mut expected = Vec::new();
            let owner = if module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            invoke(&mut m, &mut a, owner, "_errno", &[], DATA + 0x7000);
            a.bytes.extend_from_slice(&[0xc7, 0, 71, 0, 0, 0]);
            a.push_imm(73)
                .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
            for (i, s) in declarations
                .iter()
                .filter(|s| s.module == *module)
                .enumerate()
            {
                let p = DATA + 128 + i as u32 * 32;
                m.cpu.write(p, &[0xcc; 32]).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    s.name,
                    &[p],
                    DATA + 0x4000 + i as u32 * 4,
                );
                expected.push((p, s.name.contains("64")));
            }
            a.mov_eax_mem(DATA + 0x7000)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x7100);
            a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                .mov_mem_eax(DATA + 0x7104);
            assert!(
                matches!(execute(&mut m, a), RunResult::Exited(0)),
                "{module} {jit}"
            );
            for (i, (p, wide)) in expected.into_iter().enumerate() {
                let offset = if wide { 8 } else { 4 };
                assert_eq!(m.cpu.read_u32(p).unwrap(), 1577836807);
                if wide {
                    assert_eq!(m.cpu.read_u32(p + 4).unwrap(), 0);
                }
                assert_eq!(m.cpu.read_u16(p + offset).unwrap(), 654);
                assert_eq!(m.cpu.read_u16(p + offset + 2).unwrap() as i16, -540);
                assert_eq!(m.cpu.read_u16(p + offset + 4).unwrap(), 0);
                assert_eq!(m.cpu.read_u16(p + offset + 6).unwrap(), 0xcccc);
                assert_eq!(m.cpu.read_u32(DATA + 0x4000 + i as u32 * 4).unwrap(), 0);
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x7100).unwrap(), 71);
            assert_eq!(m.cpu.read_u32(DATA + 0x7104).unwrap(), 73);
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-27
#[test]
fn clock_versions_use_milliseconds_and_select_native_overflow_policy() {
    let modules: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| s.name == "clock")
        .map(|s| s.module)
        .collect();
    assert_eq!(modules.len(), 12);
    for jit in [false, true] {
        for milliseconds in [
            0,
            7654,
            i32::MAX as u64,
            i32::MAX as u64 + 1,
            u32::MAX as u64 + 124,
        ] {
            let mut m = machine(jit, milliseconds);
            let mut a = Asm::new(CODE);
            for (i, module) in modules.iter().enumerate() {
                invoke(&mut m, &mut a, module, "clock", &[], DATA + i as u32 * 4);
            }
            assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
            for (i, module) in modules.iter().enumerate() {
                let modern = *module == "ucrtbase.dll" || module.starts_with("api-ms-win-crt-");
                assert_eq!(
                    m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
                    if modern && milliseconds > i32::MAX as u64 {
                        u32::MAX
                    } else {
                        milliseconds as u32
                    },
                    "{module} {jit} {milliseconds}"
                );
            }
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-26
#[test]
fn timeb32_wraps_only_seconds_and_does_not_overwrite_tail_padding() {
    for jit in [false, true] {
        let mut m = machine(jit, (2147483648 - 1577836800) * 1000 + 999);
        let mut a = Asm::new(CODE);
        for (i, (module, name)) in [
            ("msvcr90.dll", "_ftime32_s"),
            ("ucrtbase.dll", "_ftime64_s"),
        ]
        .into_iter()
        .enumerate()
        {
            m.cpu.write(DATA + i as u32 * 32, &[0xcc; 32]).unwrap();
            invoke(
                &mut m,
                &mut a,
                module,
                name,
                &[DATA + i as u32 * 32],
                DATA + 128 + i as u32 * 4,
            );
        }
        assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0x80000000);
        assert_eq!(m.cpu.read_u16(DATA + 4).unwrap(), 999);
        assert_eq!(m.cpu.read_u16(DATA + 10).unwrap(), 0xcccc);
        assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0x80000000);
        assert_eq!(m.cpu.read_u32(DATA + 36).unwrap(), 0);
        assert_eq!(m.cpu.read_u16(DATA + 40).unwrap(), 999);
        assert_eq!(m.cpu.read_u16(DATA + 46).unwrap(), 0xcccc);
        no_gaps(&m);
    }
}

// spec: CRT-LIB-26, PERS-PROFILE-9
#[test]
fn null_packet_validation_returns_through_the_handler_or_terminates() {
    for jit in [false, true] {
        for handler in [false, true] {
            for module in ["msvcr80.dll", "msvcr90.dll", "ucrtbase.dll"] {
                let mut m = machine(jit, 0);
                let mut a = Asm::new(CODE);
                if handler {
                    m.cpu.write(CODE + 0xf000, &[0xc3]).unwrap();
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        "_set_invalid_parameter_handler",
                        &[CODE + 0xf000],
                        DATA + 128,
                    );
                }
                invoke(&mut m, &mut a, module, "_ftime64_s", &[0], DATA);
                invoke(&mut m, &mut a, module, "_errno", &[], DATA + 4);
                a.mov_eax_ptr_eax().mov_mem_eax(DATA + 8);
                let result = execute(&mut m, a);
                assert!(
                    matches!(result,RunResult::Exited(code)if code==if handler{0}else{0xc0000417}),
                    "{module} {jit} {handler}: {result:?}"
                );
                if handler {
                    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 22);
                    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 22);
                }
                no_gaps(&m);
            }
        }
    }
}

// spec: CRT-LIB-26, PERS-ABI-1
#[test]
fn protected_time_fields_fail_after_native_earlier_field_writes() {
    for jit in [false, true] {
        for wide in [false, true] {
            let mut m = machine(jit, 7654);
            let width = if wide { 8 } else { 4 };
            let pointer = DATA + 4096 - width;
            m.cpu.write(pointer, &[0xcc; 24]).unwrap();
            m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
            let mut a = Asm::new(CODE);
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                if wide { "_ftime64_s" } else { "_ftime32_s" },
                &[pointer],
                DATA + 0x4000,
            );
            assert!(matches!(execute(&mut m, a), RunResult::Failed(_)));
            assert_eq!(m.cpu.read_u32(pointer).unwrap(), 0xcccccccc);
            assert_eq!(m.cpu.read_u16(pointer + width).unwrap(), 654);
            assert_eq!(m.cpu.read_u16(pointer + width + 2).unwrap() as i16, -540);
            assert_eq!(m.cpu.read_u16(pointer + width + 4).unwrap(), 0);
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-26, CRT-LIB-27
#[test]
fn sleeping_advances_windows_and_crt_clocks_without_changing_error_cells() {
    for jit in [false, true] {
        for module in [
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-time-l1-1-0.dll",
        ] {
            let mut m = machine(jit, 0);
            let mut a = Asm::new(CODE);
            let owner = if module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            for (name, slot, value) in [
                ("_errno", DATA + 128, 71u32),
                ("__doserrno", DATA + 132, 83),
            ] {
                invoke(&mut m, &mut a, owner, name, &[], slot);
                a.bytes.extend_from_slice(&[0xc7, 0]);
                a.bytes.extend_from_slice(&value.to_le_bytes());
            }
            a.push_imm(73)
                .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
            invoke(&mut m, &mut a, module, "clock", &[], DATA);
            a.call(m.resolve_import("kernel32.dll", "GetTickCount").unwrap())
                .mov_mem_eax(DATA + 4);
            a.push_imm(5)
                .call(m.resolve_import("kernel32.dll", "Sleep").unwrap());
            invoke(&mut m, &mut a, module, "clock", &[], DATA + 8);
            a.call(m.resolve_import("kernel32.dll", "GetTickCount").unwrap())
                .mov_mem_eax(DATA + 12);
            for (slot, output) in [(DATA + 128, DATA + 16), (DATA + 132, DATA + 20)] {
                a.mov_eax_mem(slot).mov_eax_ptr_eax().mov_mem_eax(output);
            }
            a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                .mov_mem_eax(DATA + 24);
            assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
            assert_eq!(
                m.cpu.read_u32(DATA + 8).unwrap() - m.cpu.read_u32(DATA).unwrap(),
                5,
                "{module} {jit}"
            );
            assert_eq!(
                m.cpu.read_u32(DATA + 12).unwrap() - m.cpu.read_u32(DATA + 4).unwrap(),
                5
            );
            assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 71);
            assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 83);
            assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), 73);
            no_gaps(&m);
        }
    }
}
