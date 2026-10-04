//! Authored secure-copy buffers and callbacks, independent of any game data.
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
const SOURCE: u32 = DATA + 64;
const ERRNO_POINTER: u32 = DATA + 128;
const RESULT: u32 = DATA + 132;
const ERRNO: u32 = DATA + 136;
const CALLS: u32 = DATA + 140;
const SEEN_ERRNO: u32 = DATA + 144;
const SEEN_BUFFER: u32 = DATA + 148;
const LAST_ERROR: u32 = DATA + 152;
const STACK_BEFORE: u32 = DATA + 156;
const STACK_AFTER: u32 = DATA + 160;
const READ_ONLY: u32 = 0x420000;
const CALLBACK: u32 = CODE + 1024;
const STRING_APISET: &str = "api-ms-win-crt-string-l1-1-0.dll";
const MODULES: [&str; 5] = [
    "msvcrt.dll",
    "msvcr80.dll",
    "msvcr90.dll",
    "ucrtbase.dll",
    STRING_APISET,
];

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    capacity: u32,
    count: u32,
    null_dest: bool,
    null_source: bool,
    overlap: Option<(usize, usize)>,
}
fn cases() -> Vec<Case> {
    let ordinary = Case {
        name: "ordinary",
        capacity: 16,
        count: 8,
        null_dest: false,
        null_source: false,
        overlap: None,
    };
    vec![
        ordinary,
        Case {
            name: "zero count with null pointers",
            capacity: 0,
            count: 0,
            null_dest: true,
            null_source: true,
            ..ordinary
        },
        Case {
            name: "unused capacity",
            capacity: u32::MAX,
            count: 4,
            ..ordinary
        },
        Case {
            name: "null destination",
            null_dest: true,
            ..ordinary
        },
        Case {
            name: "null source",
            null_source: true,
            ..ordinary
        },
        Case {
            name: "both pointers null",
            null_dest: true,
            null_source: true,
            ..ordinary
        },
        Case {
            name: "short capacity",
            capacity: 3,
            ..ordinary
        },
        Case {
            name: "zero capacity",
            capacity: 0,
            count: 1,
            ..ordinary
        },
        Case {
            name: "null source takes precedence over short capacity",
            null_source: true,
            capacity: 2,
            ..ordinary
        },
        Case {
            name: "overlap forward",
            capacity: 14,
            count: 10,
            overlap: Some((0, 2)),
            ..ordinary
        },
        Case {
            name: "overlap backward",
            capacity: 16,
            count: 10,
            overlap: Some((2, 0)),
            ..ordinary
        },
    ]
}
fn initial(case: &Case) -> [u8; 16] {
    if case.overlap.is_some() {
        core::array::from_fn(|i| i as u8 + 1)
    } else {
        [0x77; 16]
    }
}
fn source_bytes() -> [u8; 16] {
    core::array::from_fn(|i| 0xa0 + i as u8)
}
fn expected_error(case: &Case) -> u32 {
    if case.count == 0 {
        0
    } else if case.null_dest || case.null_source {
        22
    } else if case.capacity < case.count {
        34
    } else {
        0
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Observation {
    result: u32,
    errno: u32,
    calls: u32,
    seen_errno: u32,
    buffer: [u8; 16],
}
fn expected(case: &Case, moving: bool) -> Observation {
    let error = expected_error(case);
    let mut buffer = initial(case);
    if case.count != 0 {
        if error == 0 {
            if let Some((from, to)) = case.overlap {
                buffer.copy_within(from..from + case.count as usize, to);
            } else {
                buffer[..case.count as usize]
                    .copy_from_slice(&source_bytes()[..case.count as usize]);
            }
        } else if !moving && !case.null_dest {
            buffer[..case.capacity as usize].fill(0);
        }
    }
    Observation {
        result: error,
        errno: if error == 0 { 79 } else { 123 },
        calls: u32::from(error != 0),
        seen_errno: error,
        buffer,
    }
}
fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    registry
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut machine = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/secure-memory",
            "fixture.exe",
            "/nonexistent/secure-memory-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
}
fn program<B: Backend>(
    machine: &mut Machine<B>,
    module: &str,
    moving: bool,
    args: [u32; 4],
    handler: bool,
) {
    let canonical = if module == STRING_APISET {
        "ucrtbase.dll"
    } else {
        module
    };
    let errno = machine.resolve_import(canonical, "_errno").unwrap();
    let copy = machine
        .resolve_import(module, if moving { "memmove_s" } else { "memcpy_s" })
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(73).call(set_error);
    code.call(errno).mov_mem_eax(ERRNO_POINTER);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    if handler {
        let setter = machine
            .resolve_import(canonical, "_set_invalid_parameter_handler")
            .unwrap();
        code.push_imm(CALLBACK).call(setter).add_esp(4);
        let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
        let mut callback = Asm::new(CALLBACK);
        callback
            .mov_eax_mem(ERRNO_POINTER)
            .mov_eax_ptr_eax()
            .mov_mem_eax(SEEN_ERRNO);
        callback.mov_eax_mem(DATA).mov_mem_eax(SEEN_BUFFER);
        callback.bytes.extend_from_slice(&[0xff, 0x05]);
        callback.bytes.extend_from_slice(&CALLS.to_le_bytes());
        callback.push_imm(1).call(sleep);
        callback.mov_eax_mem(ERRNO_POINTER);
        callback
            .bytes
            .extend_from_slice(&[0xc7, 0x00, 123, 0, 0, 0]);
        callback.ret();
        machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    }
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&STACK_BEFORE.to_le_bytes());
    for arg in args.into_iter().rev() {
        code.push_imm(arg);
    }
    code.call(copy).add_esp(16).mov_mem_eax(RESULT);
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&STACK_AFTER.to_le_bytes());
    code.mov_eax_mem(ERRNO_POINTER)
        .mov_eax_ptr_eax()
        .mov_mem_eax(ERRNO);
    code.call(get_error).mov_mem_eax(LAST_ERROR);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("secure byte copy", CODE, &[], 65536).unwrap();
}
fn observation<B: Backend>(machine: &Machine<B>) -> Observation {
    let mut buffer = [0; 16];
    machine.cpu.read(DATA, &mut buffer).unwrap();
    Observation {
        result: machine.cpu.read_u32(RESULT).unwrap(),
        errno: machine.cpu.read_u32(ERRNO).unwrap(),
        calls: machine.cpu.read_u32(CALLS).unwrap(),
        seen_errno: machine.cpu.read_u32(SEEN_ERRNO).unwrap(),
        buffer,
    }
}
fn data_case<B: Backend>(cpu: B, module: &str, moving: bool, case: &Case) {
    let mut machine = machine(cpu);
    machine.cpu.write(DATA, &initial(case)).unwrap();
    machine.cpu.write(SOURCE, &source_bytes()).unwrap();
    let args = [
        if case.null_dest {
            0
        } else {
            DATA + case.overlap.map_or(0, |(_, dest)| dest as u32)
        },
        case.capacity,
        if case.null_source {
            0
        } else if let Some((src, _)) = case.overlap {
            DATA + src as u32
        } else {
            SOURCE
        },
        case.count,
    ];
    let error = expected_error(case);
    program(&mut machine, module, moving, args, error != 0);
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{module}/{} moving={moving}: {result:?}",
        case.name
    );
    let expected = expected(case, moving);
    assert_eq!(
        observation(&machine),
        expected,
        "{module}/{} moving={moving}",
        case.name
    );
    assert_eq!(machine.cpu.read_u32(LAST_ERROR).unwrap(), 73);
    assert_eq!(
        machine.cpu.read_u32(STACK_BEFORE).unwrap(),
        machine.cpu.read_u32(STACK_AFTER).unwrap()
    );
    if error != 0 {
        assert_eq!(
            machine.cpu.read_u32(SEEN_BUFFER).unwrap(),
            u32::from_le_bytes(expected.buffer[..4].try_into().unwrap())
        );
    }
}
#[test]
fn byte_copies_errors_waiting_callbacks_and_memmove_overlap_match_across_crts_and_backends() {
    for moving in [false, true] {
        for case in cases() {
            if !moving && case.overlap.is_some() {
                continue;
            } // memcpy overlap is undefined.
            for module in MODULES {
                if module == "msvcrt.dll" && expected_error(&case) != 0 {
                    continue;
                } // No native handler-setter export.
                for jit in [false, true] {
                    data_case(JitCpu::new(PARK_ADDR, jit).unwrap(), module, moving, &case);
                }
                data_case(UnicornCpu::new(PARK_ADDR).unwrap(), module, moving, &case);
            }
        }
    }
}
fn fault<B: Backend>(cpu: B, moving: bool, args: [u32; 4]) {
    let mut machine = machine(cpu);
    machine.cpu.write(DATA, &[0x77; 16]).unwrap();
    machine.cpu.write(SOURCE, &source_bytes()).unwrap();
    program(&mut machine, STRING_APISET, moving, args, true);
    let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
        panic!("invalid memory accepted");
    };
    assert!(
        failure.message.contains("ABI guest memory fault"),
        "{}",
        failure.message
    );
    assert_eq!(observation(&machine).buffer, [0x77; 16]);
    assert_eq!(machine.cpu.read_u32(CALLS).unwrap(), 0);
    let errno = machine.cpu.read_u32(ERRNO_POINTER).unwrap();
    assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
}
#[test]
fn actual_copy_and_error_clear_ranges_enforce_permissions_before_modification() {
    for moving in [false, true] {
        let mut inputs = vec![
            [DATA, 16, 0x500000, 8],
            [READ_ONLY, 16, SOURCE, 8],
            [u32::MAX - 3, 8, SOURCE, 8],
            [DATA, 16, DATA + 4094, 8],
        ];
        if !moving {
            inputs.extend([[READ_ONLY, 8, 0, 4], [DATA, u32::MAX, 0, 4]]);
        }
        for args in inputs {
            for jit in [false, true] {
                fault(JitCpu::new(PARK_ADDR, jit).unwrap(), moving, args);
            }
            fault(UnicornCpu::new(PARK_ADDR).unwrap(), moving, args);
        }
    }
}
#[test]
fn unused_capacity_and_zero_count_do_not_access_unmapped_pages() {
    fn run<B: Backend>(cpu: B, moving: bool, zero: bool) {
        let mut machine = machine(cpu);
        machine.cpu.write(DATA + 4092, &[1, 2, 3, 4]).unwrap();
        machine.cpu.write(SOURCE, &source_bytes()).unwrap();
        let args = if zero {
            [u32::MAX, u32::MAX, 0x500000, 0]
        } else {
            [DATA + 4092, u32::MAX, SOURCE, 4]
        };
        program(&mut machine, STRING_APISET, moving, args, false);
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        assert_eq!(machine.cpu.read_u32(RESULT).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(ERRNO).unwrap(), 79);
    }
    for moving in [false, true] {
        for zero in [false, true] {
            for jit in [false, true] {
                run(JitCpu::new(PARK_ADDR, jit).unwrap(), moving, zero);
            }
            run(UnicornCpu::new(PARK_ADDR).unwrap(), moving, zero);
        }
    }
}
#[test]
fn missing_returning_invalid_handler_is_explicit_and_never_fakes_success() {
    fn run<B: Backend>(cpu: B, module: &str, moving: bool) {
        let mut machine = machine(cpu);
        program(&mut machine, module, moving, [DATA, 0, SOURCE, 1], false);
        let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
            panic!("unhandled invalid parameter accepted");
        };
        assert!(
            failure
                .message
                .contains("invalid parameter without a returning guest handler")
        );
    }
    for module in ["msvcrt.dll", STRING_APISET] {
        for moving in [false, true] {
            for jit in [false, true] {
                run(JitCpu::new(PARK_ADDR, jit).unwrap(), module, moving);
            }
            run(UnicornCpu::new(PARK_ADDR).unwrap(), module, moving);
        }
    }
}
#[test]
fn generated_secure_byte_copy_signatures_and_aliases_match_available_exports() {
    let registry = registry();
    assert!(registry.signature_conflicts().is_empty());
    for module in MODULES {
        for name in ["memcpy_s", "memmove_s"] {
            assert_eq!(
                registry.level(module, name),
                Some(ImplementationLevel::Implement)
            );
            assert!(registry.dynamic_available(module, name));
            assert_eq!(
                adv_abi::signature::lookup(module, name).unwrap().conv,
                Some(adv_machine::Conv::Cdecl { argc: 4 })
            );
        }
    }
    assert!(!registry.dynamic_available("msvcrt.dll", "_set_invalid_parameter_handler"));
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        cell::Cell,
        os::windows::process::CommandExt,
        process::{Child, Command},
        time::{Duration, Instant},
    };
    type Handler = unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize);
    #[link(name = "ucrt")]
    unsafe extern "C" {
        #[link_name = "memcpy_s"]
        fn host_copy(dest: *mut u8, capacity: usize, source: *const u8, count: usize) -> i32;
        #[link_name = "memmove_s"]
        fn host_move(dest: *mut u8, capacity: usize, source: *const u8, count: usize) -> i32;
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
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if self.0.try_wait().is_ok_and(|status| status.is_some()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    #[test]
    fn release_ucrt_defined_buffer_cases_match_with_bounded_owned_child() {
        if std::env::var_os("ADV_SECURE_COPY_ORACLE_CHILD").is_none() {
            let child = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("native::release_ucrt_defined_buffer_cases_match_with_bounded_owned_child")
                .arg("--nocapture")
                .env("ADV_SECURE_COPY_ORACLE_CHILD", "1")
                .creation_flags(0x08000000)
                .spawn()
                .unwrap();
            let mut child = OwnedChild(child);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success(), "native oracle failed: {status}");
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "native oracle exceeded five seconds"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let _restore = unsafe {
            Restore {
                errno: *host_errno(),
                handler: _set_thread_local_invalid_parameter_handler(Some(returning_handler)),
            }
        };
        for moving in [false, true] {
            for case in cases() {
                if !moving && case.overlap.is_some() {
                    continue;
                }
                let mut buffer = initial(&case);
                let source = source_bytes();
                CALLS.set(0);
                SEEN.set(0);
                unsafe {
                    *host_errno() = 79;
                }
                let dest = if case.null_dest {
                    core::ptr::null_mut()
                } else {
                    unsafe {
                        buffer
                            .as_mut_ptr()
                            .add(case.overlap.map_or(0, |(_, dest)| dest))
                    }
                };
                let src = if case.null_source {
                    core::ptr::null()
                } else if let Some((src, _)) = case.overlap {
                    unsafe { buffer.as_ptr().add(src) }
                } else {
                    source.as_ptr()
                };
                let result = unsafe {
                    (if moving { host_move } else { host_copy })(
                        dest,
                        case.capacity as usize,
                        src,
                        case.count as usize,
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
                    expected(&case, moving),
                    "{} moving={moving}",
                    case.name
                );
            }
        }
    }
}
