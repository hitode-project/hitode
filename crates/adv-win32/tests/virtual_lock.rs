//! Authored private buffers; host Locked bits prove physical pins, not residency.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const BUFFER: u32 = 0x600000;
const MARKER: u32 = 0x24681357;

#[derive(Default)]
struct Snapshots {
    host: Option<usize>,
    values: Vec<(bool, u32)>,
}

#[cfg(windows)]
fn host_locked(host: usize) -> u32 {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct WorkingSet {
        address: usize,
        attributes: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32QueryWorkingSetEx(process: *mut c_void, data: *mut WorkingSet, size: u32) -> i32;
    }
    let mut pages = [
        WorkingSet {
            address: host,
            attributes: 0,
        },
        WorkingSet {
            address: host + 4096,
            attributes: 0,
        },
    ];
    // SDK PSAPI_WORKING_SET_EX_BLOCK: Valid bit 0 and Locked bit 22. Querying
    // an owned reserved/uncommitted address never dereferences those bytes.
    assert_ne!(
        unsafe {
            K32QueryWorkingSetEx(
                GetCurrentProcess(),
                pages.as_mut_ptr(),
                std::mem::size_of_val(&pages) as u32,
            )
        },
        0
    );
    pages.iter().enumerate().fold(0, |mask, (i, page)| {
        mask | (u32::from(page.attributes & 1 != 0 && page.attributes & (1 << 22) != 0) << i)
    })
}

fn snapshot(env: &mut Env<'_>) -> Outcome {
    let ledger = env.cpu.has_locked_pages(BUFFER, 0x20000);
    let host = env.ext.get_or_default::<Snapshots>().host.or_else(|| {
        env.cpu
            .shared_bytes(BUFFER, 1)
            .map(|bytes| bytes.as_ptr() as usize)
    });
    #[cfg(windows)]
    let mask = host.map_or(0, host_locked);
    #[cfg(not(windows))]
    let mask = 0;
    let snapshots = env.ext.get_or_default::<Snapshots>();
    snapshots.host = host;
    snapshots.values.push((ledger, mask));
    Outcome::Return(0)
}

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add(
        "lock-fixture.dll",
        "Snapshot",
        Conv::Stdcall { argc: 0 },
        snapshot,
    );
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new("/nonexistent/lock", "fixture.exe", "/nonexistent/lock-data"),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}

fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], index: u32) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let api = m.resolve_import("kernel32.dll", name).unwrap();
    let snapshot = m.resolve_import("lock-fixture.dll", "Snapshot").unwrap();
    code.push_imm(MARKER).call(set);
    for &argument in args.iter().rev() {
        code.push_imm(argument);
    }
    code.call(api).mov_mem_eax(DATA + index * 8);
    code.call(get).mov_mem_eax(DATA + index * 8 + 4);
    code.call(snapshot);
}

fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm, process_exit: bool) {
    if process_exit {
        let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
        code.push_imm(0).call(exit);
    } else {
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
    }
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("physical lock fixture", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

#[cfg(windows)]
#[test]
fn private_pages_really_pin_without_counts_and_retire_on_noaccess_release_and_process_exit() {
    for (jit, native, process_exit) in [(false, false, false), (true, true, true)] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        if native {
            cpu.executor_mut().enable_native_protection().unwrap();
        }
        let mut m = machine(cpu);
        let mut code = Asm::new(CODE);
        let cases: &[(&str, &[u32], u32, u32, bool, u32)] = &[
            (
                "VirtualAlloc",
                &[BUFFER, 0x20000, 0x3000, 4],
                BUFFER,
                MARKER,
                false,
                0,
            ),
            ("VirtualLock", &[BUFFER + 4095, 2], 1, MARKER, true, 3),
            ("VirtualLock", &[BUFFER, 8192], 1, MARKER, true, 3),
            ("VirtualUnlock", &[BUFFER, 4096], 1, MARKER, true, 2),
            ("VirtualUnlock", &[BUFFER, 8192], 0, 158, true, 2),
            ("VirtualUnlock", &[BUFFER + 4096, 4096], 1, MARKER, false, 0),
            ("VirtualUnlock", &[BUFFER + 4096, 4096], 0, 158, false, 0),
            ("VirtualLock", &[BUFFER, 0], 0, 87, false, 0),
            ("VirtualLock", &[0, 4096], 0, 998, false, 0),
            ("VirtualUnlock", &[0, 4096], 0, 158, false, 0),
            ("VirtualLock", &[BUFFER, 0x20000], 1, MARKER, true, 3),
            // The process-memory implementation now supports real decommit:
            // the first page and its physical pin retire; the second stays live.
            ("VirtualFree", &[BUFFER, 4096, 0x4000], 1, MARKER, true, 2),
            ("VirtualAlloc", &[BUFFER, 4096, 0x1000, 4], BUFFER, MARKER, true, 2),
            (
                "VirtualProtect",
                &[BUFFER, 4096, 1, DATA + 0x800],
                1,
                MARKER,
                true,
                2,
            ),
            ("VirtualLock", &[BUFFER, 4096], 0, 998, true, 2),
            ("VirtualUnlock", &[BUFFER, 4096], 0, 158, true, 2),
            ("VirtualFree", &[BUFFER, 0, 0x8000], 1, MARKER, false, 0),
            (
                "VirtualAlloc",
                &[BUFFER, 0x20000, 0x3000, 4],
                BUFFER,
                MARKER,
                false,
                0,
            ),
            ("VirtualUnlock", &[BUFFER, 0x20000], 0, 158, false, 0),
            ("VirtualLock", &[BUFFER, 4096], 1, MARKER, true, 1),
        ];
        for (i, &(name, arguments, ..)) in cases.iter().enumerate() {
            call(&mut m, &mut code, name, arguments, i as u32);
        }
        run(&mut m, code, process_exit);
        for (i, &(name, _, result, error, ledger, mask)) in cases.iter().enumerate() {
            assert_eq!(
                m.cpu.read_u32(DATA + i as u32 * 8).unwrap(),
                result,
                "{name} case {i}"
            );
            assert_eq!(
                m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
                error,
                "{name} case {i}"
            );
            assert_eq!(
                m.ext.get::<Snapshots>().values[i],
                (ledger, mask),
                "{name} case {i}"
            );
        }
        assert!(!m.cpu.has_locked_pages(BUFFER, 0x20000));
        assert_eq!(host_locked(m.ext.get::<Snapshots>().host.unwrap()), 0);
        assert!(
            m.cpu.accessible(BUFFER, 4096, Prot::READ),
            "exit retained report-readable memory"
        );
        assert!(
            m.ext.try_get::<ApiDiagnostics>().is_none_or(|diagnostics| {
                !diagnostics
                    .warnings
                    .keys()
                    .any(|(api, _)| api.ends_with("VirtualFree"))
            }),
            "supported decommit/release do not report an unavailable operation"
        );
    }
}

#[test]
fn unsupported_backing_and_backend_never_advertise_successful_pins() {
    let mut m = machine(UnicornCpu::new(PARK_ADDR).unwrap());
    let mut code = Asm::new(CODE);
    call(
        &mut m,
        &mut code,
        "VirtualAlloc",
        &[BUFFER, 8192, 0x3000, 4],
        0,
    );
    call(&mut m, &mut code, "VirtualLock", &[BUFFER, 8192], 1);
    call(&mut m, &mut code, "VirtualUnlock", &[BUFFER, 8192], 2);
    run(&mut m, code, false);
    for i in [1, 2] {
        assert_eq!(m.cpu.read_u32(DATA + i * 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + i * 8 + 4).unwrap(), 50);
    }
    let diagnostics = m.ext.get::<ApiDiagnostics>();
    for name in ["VirtualLock", "VirtualUnlock"] {
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(api, _)| api.ends_with(name))
        );
    }
    let mut memory = adv_cpu::Memory::new(0, 4096).unwrap();
    memory
        .protect(0, 4096, adv_cpu::READ | adv_cpu::WRITE)
        .unwrap();
    assert_eq!(
        memory.lock_pages(0, 4096),
        Err(adv_cpu::backend::PageLockError::Unsupported)
    );
}
