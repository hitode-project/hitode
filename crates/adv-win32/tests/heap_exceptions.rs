//! Authored heap requests follow the native Windows oracle's exception contract.
//! The companion C oracle covers allocation/reallocation success, overflow failure
//! and vectored continuation; these guests additionally verify shared heap locks.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Heaps, Loader, Modules};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x420000;
const OBSERVED: u32 = DATA + 0x1000;
const HANDLER: u32 = CODE + 0x8000;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

fn fixture(jit: bool) -> Machine<JitCpu> {
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
            "/nonexistent/heap-exceptions",
            "heap.exe",
            "/nonexistent/heap-exceptions-data",
        ),
    )
    .unwrap();
    Win32::prepare_process_infrastructure(&mut machine).unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
    machine
}

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
fn api(machine: &mut Machine<JitCpu>, name: &str) -> u32 {
    machine.resolve_import("kernel32.dll", name).unwrap()
}
fn call(code: &mut Asm, entry: u32, args: &[Arg], output: u32) {
    // Preserve a before/after stack pair for ordinary and continued API calls.
    code.bytes.extend([0x89, 0x25]);
    code.bytes.extend((output + 4).to_le_bytes());
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(address) => {
                code.mov_eax_mem(*address).push_eax();
            }
        }
    }
    code.call(entry).mov_mem_eax(output);
    code.bytes.extend([0x89, 0x25]);
    code.bytes.extend((output + 8).to_le_bytes());
}
fn returned(machine: &Machine<JitCpu>, output: u32) -> u32 {
    assert_eq!(
        machine.cpu.read_u32(output + 4).unwrap(),
        machine.cpu.read_u32(output + 8).unwrap(),
        "continued heap calls retain the stdcall stack"
    );
    machine.cpu.read_u32(output).unwrap()
}
fn run(machine: &mut Machine<JitCpu>, mut code: Asm) -> RunResult {
    code.push_imm(0).call(api(machine, "ExitProcess"));
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("heap caller", CODE, &[], 65536).unwrap();
    machine.run_virtual(1_000_000)
}
fn require_success(machine: &mut Machine<JitCpu>, code: Asm) {
    let result = run(machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn observer(machine: &mut Machine<JitCpu>, heap: u32) {
    let alloc = public(machine, "ntdll.dll", "RtlAllocateHeap");
    let free = public(machine, "ntdll.dll", "RtlFreeHeap");
    let mut code = Asm::new(HANDLER);
    code.mov_eax_mem(OBSERVED)
        .add_eax_imm(1)
        .mov_mem_eax(OBSERVED)
        .mov_eax_esp(4);
    code.bytes.extend([0x8b, 0x08]); // ECX = EXCEPTION_POINTERS.ExceptionRecord.
    for (index, offset) in [0, 4, 16, 20].into_iter().enumerate() {
        code.bytes.extend([0x8b, 0x41, offset]);
        code.mov_mem_eax(OBSERVED + 4 + index as u32 * 4);
    }
    // The lock exists because the caller initialized it with HeapLock/Unlock.
    code.mov_eax_mem(heap + 0x28).mov_mem_eax(OBSERVED + 20);
    code.mov_eax_mem(heap + 0x2c).mov_mem_eax(OBSERVED + 24);
    code.push_imm(16)
        .push_imm(8)
        .push_imm(heap)
        .call(alloc)
        .mov_mem_eax(OBSERVED + 28);
    code.push_eax().push_imm(0).push_imm(heap).call(free);
    code.mov_mem_eax(OBSERVED + 32);
    code.bytes.extend([0xb8, 0xff, 0xff, 0xff, 0xff]); // Continue execution.
    code.ret_n(4);
    machine.cpu.write(HANDLER, &code.bytes).unwrap();
}
fn copy_observation(code: &mut Asm, target: u32) {
    for offset in (0..36).step_by(4) {
        code.mov_eax_mem(OBSERVED + offset)
            .mov_mem_eax(target + offset);
    }
}
fn check_observation(machine: &Machine<JitCpu>, target: u32, count: u32) {
    let read = |offset| machine.cpu.read_u32(target + offset).unwrap();
    assert_eq!(
        (read(0), read(4), read(8), read(12), read(16)),
        (count, 0xc0000017, 0, 1, u32::MAX),
        "native oracle: continuable STATUS_NO_MEMORY carries the requested size"
    );
    assert_eq!((read(20), read(24)), (0, 0), "temporary heap lock released");
    assert_ne!(read(28), 0, "heap usable inside the exception callback");
    assert_eq!(read(32), 1);
}
fn initialize(code: &mut Asm, machine: &mut Machine<JitCpu>, heap: u32) {
    call(code, api(machine, "HeapLock"), &[I(heap)], DATA + 0xe00);
    call(code, api(machine, "HeapUnlock"), &[I(heap)], DATA + 0xe10);
    observer(machine, heap);
    call(
        code,
        api(machine, "AddVectoredExceptionHandler"),
        &[I(1), I(HANDLER)],
        DATA + 0xe20,
    );
}

#[test]
fn heap_and_rtl_fronts_share_zeroing_oom_continuation_and_realloc_preservation() {
    for jit in [false, true] {
        for front in 0..3 {
            let mut machine = fixture(jit);
            let heap = machine.ext.get::<Heaps>().process_heap;
            let (alloc, realloc) = match front {
                0 => (
                    api(&mut machine, "HeapAlloc"),
                    api(&mut machine, "HeapReAlloc"),
                ),
                1 => (
                    public(&mut machine, "kernel32.dll", "HeapAlloc"),
                    public(&mut machine, "kernel32.dll", "HeapReAlloc"),
                ),
                _ => (
                    public(&mut machine, "ntdll.dll", "RtlAllocateHeap"),
                    public(&mut machine, "ntdll.dll", "RtlReAllocateHeap"),
                ),
            };
            assert_eq!(
                public(&mut machine, "kernel32.dll", "HeapAlloc"),
                public(&mut machine, "ntdll.dll", "RtlAllocateHeap")
            );
            let mut code = Asm::new(CODE);
            initialize(&mut code, &mut machine, heap);
            call(&mut code, alloc, &[I(heap), I(12), I(64)], DATA);
            call(&mut code, alloc, &[I(heap), I(0), I(16)], DATA + 0x20);
            code.mov_eax_mem(DATA + 0x20);
            for offset in [0, 4, 8, 12] {
                code.bytes.extend([0xc7, 0x40, offset]);
                code.bytes.extend(0x3c3c3c3cu32.to_le_bytes());
            }
            call(
                &mut code,
                realloc,
                &[I(heap), I(12), M(DATA + 0x20), I(64)],
                DATA + 0x40,
            );
            let mut exception_count = 0;
            for (index, flags) in [0, 4, 12].into_iter().enumerate() {
                let output = DATA + 0x100 + index as u32 * 0x100;
                call(&mut code, alloc, &[I(heap), I(flags), I(u32::MAX)], output);
                if flags != 0 {
                    exception_count += 1;
                    copy_observation(&mut code, output + 0x20);
                }
                call(
                    &mut code,
                    realloc,
                    &[I(heap), I(flags), M(DATA + 0x40), I(u32::MAX)],
                    output + 0x50,
                );
                if flags != 0 {
                    exception_count += 1;
                    copy_observation(&mut code, output + 0x70);
                }
            }
            call(
                &mut code,
                public(&mut machine, "ntdll.dll", "RtlSizeHeap"),
                &[I(heap), I(0), M(DATA + 0x40)],
                DATA + 0x80,
            );
            require_success(&mut machine, code);
            let allocated = returned(&machine, DATA);
            let grown = returned(&machine, DATA + 0x40);
            assert_ne!(allocated, 0);
            assert_ne!(grown, 0);
            let mut bytes = [0xff; 64];
            machine.cpu.read(allocated, &mut bytes).unwrap();
            assert_eq!(bytes, [0; 64]);
            machine.cpu.read(grown, &mut bytes).unwrap();
            assert_eq!(&bytes[..16], &[0x3c; 16]);
            assert_eq!(&bytes[16..], &[0; 48]);
            assert_eq!(returned(&machine, DATA + 0x80), 64);
            for (index, flags) in [0, 4, 12].into_iter().enumerate() {
                let output = DATA + 0x100 + index as u32 * 0x100;
                assert_eq!(returned(&machine, output), 0);
                assert_eq!(returned(&machine, output + 0x50), 0);
                if flags != 0 {
                    check_observation(&machine, output + 0x20, index as u32 * 2 - 1);
                    check_observation(&machine, output + 0x70, index as u32 * 2);
                }
            }
            assert_eq!(machine.cpu.read_u32(OBSERVED).unwrap(), exception_count);
            assert_eq!(
                (
                    machine.cpu.read_u32(heap + 0x28).unwrap(),
                    machine.cpu.read_u32(heap + 0x2c).unwrap()
                ),
                (0, 0)
            );
            assert!(
                machine
                    .ext
                    .get_or_default::<ApiDiagnostics>()
                    .acceptance_eligible(RunMode::Strict)
            );
        }
    }
}

#[test]
fn unsupported_heap_flag_bits_remain_unavailable_and_do_not_claim_oom() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let heap = machine.ext.get::<Heaps>().process_heap;
        let mut code = Asm::new(CODE);
        initialize(&mut code, &mut machine, heap);
        call(
            &mut code,
            api(&mut machine, "HeapAlloc"),
            &[I(heap), I(8), I(16)],
            DATA,
        );
        call(
            &mut code,
            public(&mut machine, "ntdll.dll", "RtlAllocateHeap"),
            &[I(heap), I(0x24), I(16)],
            DATA + 0x20,
        );
        call(
            &mut code,
            api(&mut machine, "HeapReAlloc"),
            &[I(heap), I(0x24), M(DATA), I(32)],
            DATA + 0x40,
        );
        require_success(&mut machine, code);
        assert_eq!(returned(&machine, DATA + 0x20), 0);
        assert_eq!(returned(&machine, DATA + 0x40), 0);
        assert_eq!(machine.cpu.read_u32(OBSERVED).unwrap(), 0);
        let allocated = returned(&machine, DATA);
        assert_eq!(
            machine.ext.get_mut::<Heaps>().size(heap, allocated),
            Some(16)
        );
        let diagnostics = machine.ext.get::<ApiDiagnostics>();
        for name in ["ntdll.dll!RtlAllocateHeap", "ntdll.dll!RtlReAllocateHeap"] {
            assert!(diagnostics.warnings.iter().any(|((api, reason), count)| {
                api == name && reason.contains("flags 0x24") && *count == 1
            }));
        }
    }
}

#[test]
fn an_unhandled_heap_oom_uses_the_windows_exception_owner() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let heap = machine.ext.get::<Heaps>().process_heap;
        let mut code = Asm::new(CODE);
        call(
            &mut code,
            public(&mut machine, "kernel32.dll", "HeapAlloc"),
            &[I(heap), I(12), I(u32::MAX)],
            DATA,
        );
        let result = run(&mut machine, code);
        match result {
            RunResult::Failed(reason) => assert!(reason.message.contains("c0000017"), "{reason}"),
            other => panic!("unhandled allocation exception must fail: {other:?}"),
        }
    }
}
