//! Authored CRT data only: UTF-16 concatenation, error callbacks and guest memory.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const SOURCE: u32 = DATA + 64;
const ERRNO_POINTER: u32 = DATA + 128;
const RESULT: u32 = DATA + 132;
const ERRNO: u32 = DATA + 136;
const CALLS: u32 = DATA + 140;
const SEEN_ERRNO: u32 = DATA + 144;
const CALLBACK: u32 = CODE + 512;
const STRING_APISET: &str = "api-ms-win-crt-string-l1-1-0.dll";

fn registry() -> Registry {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    adv_win32::msvcrt::register(&mut reg);
    reg
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut machine = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/wide-string",
            "fixture.exe",
            "/nonexistent/wide-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
}
fn write_units<B: Backend>(machine: &mut Machine<B>, pointer: u32, units: &[u16]) {
    let bytes: Vec<_> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn program<B: Backend>(machine: &mut Machine<B>, module: &str, args: [u32; 3], handler: bool) {
    let errno = machine.resolve_import("ucrtbase.dll", "_errno").unwrap();
    let cat = machine.resolve_import(module, "wcscat_s").unwrap();
    let mut code = Asm::new(CODE);
    code.call(errno).mov_mem_eax(ERRNO_POINTER);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]); // *errno = 79
    if handler {
        let setter = machine
            .resolve_import("ucrtbase.dll", "_set_invalid_parameter_handler")
            .unwrap();
        code.push_imm(CALLBACK).call(setter).add_esp(4);
        let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
        let mut callback = Asm::new(CALLBACK);
        callback
            .mov_eax_mem(ERRNO_POINTER)
            .mov_eax_ptr_eax()
            .mov_mem_eax(SEEN_ERRNO);
        callback.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [CALLS]
        callback.bytes.extend_from_slice(&CALLS.to_le_bytes());
        callback.push_imm(1).call(sleep);
        callback.mov_eax_mem(ERRNO_POINTER);
        callback
            .bytes
            .extend_from_slice(&[0xc7, 0x00, 123, 0, 0, 0]);
        callback.ret();
        machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    }
    for arg in args.into_iter().rev() {
        code.push_imm(arg);
    }
    code.call(cat).add_esp(12).mov_mem_eax(RESULT);
    code.mov_eax_mem(ERRNO_POINTER)
        .mov_eax_ptr_eax()
        .mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("wcscat_s data", CODE, &[], 0x10000).unwrap();
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    result: u32,
    errno: u32,
    calls: u32,
    seen_errno: u32,
    buffer: [u16; 8],
}
fn observe<B: Backend>(machine: &Machine<B>) -> Observation {
    let mut bytes = [0; 16];
    machine.cpu.read(DATA, &mut bytes).unwrap();
    let buffer = core::array::from_fn(|i| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]));
    Observation {
        result: machine.cpu.read_u32(RESULT).unwrap(),
        errno: machine.cpu.read_u32(ERRNO).unwrap(),
        calls: machine.cpu.read_u32(CALLS).unwrap(),
        seen_errno: machine.cpu.read_u32(SEEN_ERRNO).unwrap(),
        buffer,
    }
}
struct Case {
    name: &'static str,
    buffer: [u16; 8],
    capacity: u32,
    source: Option<&'static [u16]>,
    null_dest: bool,
}
fn cases() -> Vec<Case> {
    let initial = [65, 0, 0x7777, 0x7777, 0x7777, 0x7777, 0x7777, 0x7777];
    vec![
        Case {
            name: "exact capacity",
            buffer: initial,
            capacity: 5,
            source: Some(&[120, 121, 122, 0]),
            null_dest: false,
        },
        Case {
            name: "empty source",
            buffer: initial,
            capacity: 8,
            source: Some(&[0]),
            null_dest: false,
        },
        Case {
            name: "UTF-16 code units",
            buffer: initial,
            capacity: 5,
            source: Some(&[0x65e5, 0xd83d, 0xde42, 0]),
            null_dest: false,
        },
        Case {
            name: "too small",
            buffer: initial,
            capacity: 4,
            source: Some(&[120, 121, 122, 0]),
            null_dest: false,
        },
        Case {
            name: "zero capacity",
            buffer: initial,
            capacity: 0,
            source: Some(&[120, 0]),
            null_dest: false,
        },
        Case {
            name: "unterminated",
            buffer: [81; 8],
            capacity: 8,
            source: Some(&[120, 0]),
            null_dest: false,
        },
        Case {
            name: "null source",
            buffer: initial,
            capacity: 8,
            source: None,
            null_dest: false,
        },
        Case {
            name: "null destination",
            buffer: initial,
            capacity: 8,
            source: Some(&[120, 0]),
            null_dest: true,
        },
    ]
}
fn expected(case: &Case) -> Observation {
    let mut buffer = case.buffer;
    let error = match case.name {
        "too small" => {
            buffer[..4].copy_from_slice(&[0, 120, 121, 122]);
            34
        }
        "unterminated" | "null source" => {
            buffer[0] = 0;
            22
        }
        "zero capacity" | "null destination" => 22,
        _ => {
            let source = case.source.unwrap();
            buffer[1..1 + source.len()].copy_from_slice(source);
            0
        }
    };
    Observation {
        result: error,
        errno: if error == 0 { 79 } else { 123 },
        calls: u32::from(error != 0),
        seen_errno: error,
        buffer,
    }
}
fn run_case<B: Backend>(cpu: B, module: &str, case: &Case) -> Observation {
    let mut machine = machine(cpu);
    write_units(&mut machine, DATA, &case.buffer);
    if let Some(source) = case.source {
        write_units(&mut machine, SOURCE, source);
    }
    program(
        &mut machine,
        module,
        [
            if case.null_dest { 0 } else { DATA },
            case.capacity,
            if case.source.is_none() { 0 } else { SOURCE },
        ],
        true,
    );
    assert!(
        matches!(machine.run_virtual(1_000_000), RunResult::Exited(0)),
        "{}",
        case.name
    );
    observe(&machine)
}

#[test]
fn secure_concatenation_and_waiting_error_callbacks_match_across_backends() {
    for case in cases() {
        let expected = expected(&case);
        for module in ["ucrtbase.dll", STRING_APISET] {
            for jit in [false, true] {
                assert_eq!(
                    run_case(JitCpu::new(PARK_ADDR, jit).unwrap(), module, &case),
                    expected,
                    "{} / {module} / jit={jit}",
                    case.name
                );
            }
            assert_eq!(
                run_case(UnicornCpu::new(PARK_ADDR).unwrap(), module, &case),
                expected,
                "{} / {module} / Unicorn",
                case.name
            );
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::cell::Cell;
    type Handler = unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize);
    #[link(name = "ucrt")]
    unsafe extern "C" {
        #[link_name = "wcscat_s"]
        fn host_cat(destination: *mut u16, capacity: usize, source: *const u16) -> i32;
        #[link_name = "_errno"]
        fn host_errno() -> *mut i32;
        fn _set_thread_local_invalid_parameter_handler(handler: Option<Handler>)
        -> Option<Handler>;
    }
    thread_local! {
        static CALLS: Cell<u32> = const { Cell::new(0) };
        static SEEN: Cell<u32> = const { Cell::new(0) };
    }
    unsafe extern "C" fn returning_handler(
        _: *const u16,
        _: *const u16,
        _: *const u16,
        _: u32,
        _: usize,
    ) {
        CALLS.set(CALLS.get() + 1);
        unsafe {
            SEEN.set(*host_errno() as u32);
            *host_errno() = 123;
        }
    }
    struct Restore {
        handler: Option<Handler>,
        errno: i32,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                _set_thread_local_invalid_parameter_handler(self.handler);
                *host_errno() = self.errno;
            }
        }
    }
    #[test]
    fn authored_error_cases_match_release_ucrt_without_global_host_state_changes() {
        let _restore = unsafe {
            Restore {
                errno: *host_errno(),
                handler: _set_thread_local_invalid_parameter_handler(Some(returning_handler)),
            }
        };
        for case in cases() {
            let mut buffer = case.buffer;
            CALLS.set(0);
            SEEN.set(0);
            unsafe {
                *host_errno() = 79;
            }
            let result = unsafe {
                host_cat(
                    if case.null_dest {
                        core::ptr::null_mut()
                    } else {
                        buffer.as_mut_ptr()
                    },
                    case.capacity as usize,
                    case.source.map_or(core::ptr::null(), |s| s.as_ptr()),
                )
            };
            assert_eq!(
                Observation {
                    result: result as u32,
                    errno: unsafe { *host_errno() as u32 },
                    calls: CALLS.get(),
                    seen_errno: SEEN.get(),
                    buffer
                },
                expected(&case),
                "{}",
                case.name
            );
        }
    }
}

fn faults<B: Backend>(cpu: B, args: [u32; 3], partial_prefix: bool) {
    let mut machine = machine(cpu);
    write_units(&mut machine, DATA, &[65, 0]);
    write_units(&mut machine, READ_ONLY, &[65, 0]);
    write_units(&mut machine, SOURCE, &[120, 0]);
    write_units(&mut machine, DATA + 4094, &[120]);
    program(&mut machine, STRING_APISET, args, true);
    let RunResult::Failed(error) = machine.run_virtual(1_000_000) else {
        panic!("invalid memory accepted");
    };
    assert!(
        error.message.contains("ABI guest memory fault"),
        "{}",
        error.message
    );
    assert_eq!(machine.cpu.read_u32(CALLS).unwrap(), 0);
    let errno = machine.cpu.read_u32(ERRNO_POINTER).unwrap();
    assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        if partial_prefix { 0x0078_0041 } else { 65 }
    );
}
#[test]
fn actual_guest_accesses_enforce_protection_and_preserve_written_prefix_on_later_fault() {
    for (args, partial) in [
        ([READ_ONLY, 8, SOURCE], false),
        ([DATA, 8, 0x500000], false),
        ([DATA, 8, DATA + 4094], true),
        ([u32::MAX, 8, SOURCE], false),
    ] {
        for jit in [false, true] {
            faults(JitCpu::new(PARK_ADDR, jit).unwrap(), args, partial);
        }
        faults(UnicornCpu::new(PARK_ADDR).unwrap(), args, partial);
    }
}
#[test]
fn unused_capacity_does_not_require_unmapped_tail_pages() {
    fn run<B: Backend>(cpu: B) {
        let mut machine = machine(cpu);
        write_units(&mut machine, DATA + 4092, &[65, 0]);
        write_units(&mut machine, SOURCE, &[0]);
        program(
            &mut machine,
            STRING_APISET,
            [DATA + 4092, u32::MAX, SOURCE],
            true,
        );
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        assert_eq!(machine.cpu.read_u32(RESULT).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(ERRNO).unwrap(), 79);
    }
    for jit in [false, true] {
        run(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn missing_invalid_parameter_handler_fails_explicitly() {
    fn run<B: Backend>(cpu: B) {
        let mut machine = machine(cpu);
        program(&mut machine, STRING_APISET, [DATA, 0, SOURCE], false);
        let RunResult::Failed(error) = machine.run_virtual(1_000_000) else {
            panic!("invalid call was accepted");
        };
        assert!(
            error
                .message
                .contains("wcscat_s: invalid parameter without a returning guest handler")
        );
    }
    for jit in [false, true] {
        run(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn generated_declarations_and_dynamic_availability_match_typed_implementation() {
    let reg = registry();
    for module in ["ucrtbase.dll", STRING_APISET] {
        assert!(reg.dynamic_available(module, "wcscat_s"));
        assert_eq!(
            reg.level(module, "wcscat_s"),
            Some(ImplementationLevel::Implement)
        );
        let signature = adv_abi::signature::lookup(module, "wcscat_s").unwrap();
        assert_eq!(signature.conv, Some(adv_machine::Conv::Cdecl { argc: 3 }));
    }
    assert!(reg.signature_conflicts().is_empty());
}
