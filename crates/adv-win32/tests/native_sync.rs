//! Authored synchronization calls validate native hooks over the shared object owner.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, Object, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_win32::{Win32, Win32Config};

#[path = "native_sync/owned.rs"]
mod owned;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const ARRAY: u32 = DATA + 0x300;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};
fn fixture(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add(
        "test",
        "hold_sync_waiter",
        adv_abi::Conv::Stdcall { argc: 1 },
        |env| owned::hold_until_waiter(env, env.arg(0)),
    );
    registry.add(
        "test",
        "sync_aliases",
        adv_abi::Conv::Stdcall { argc: 0 },
        |env| {
            adv_machine::Outcome::Return(
                env.sys.objects.alias_count(adv_machine::Namespace::DEFAULT) as u32,
            )
        },
    );
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/native-sync",
            "sync.exe",
            "/nonexistent/native-sync-data",
        ),
    )
    .unwrap();
    Win32::prepare_process_infrastructure(&mut machine).unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
    machine
}
fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, api: &str, args: &[Arg], output: u32) {
    let (module, name) = api.split_once('!').unwrap_or(("kernel32.dll", api));
    let entry = machine.resolve_import(module, name).unwrap();
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(pointer) => {
                code.mov_eax_mem(*pointer).push_eax();
            }
        }
    }
    code.call(entry);
    if output != 0 {
        code.mov_mem_eax(output);
    }
}
fn run(machine: &mut Machine<JitCpu>, mut code: Asm) {
    call(machine, &mut code, "ExitProcess", &[I(0)], 0);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("synchronization caller", CODE, &[], 0x10000)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
fn word(machine: &Machine<JitCpu>, offset: u32) -> u32 {
    machine.cpu.read_u32(DATA + offset).unwrap()
}
fn event(machine: &mut Machine<JitCpu>, signaled: bool) -> u32 {
    machine.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled,
    })
}
fn array(machine: &mut Machine<JitCpu>, handles: &[u32]) {
    for (i, &handle) in handles.iter().enumerate() {
        machine.cpu.write_u32(ARRAY + i as u32 * 4, handle).unwrap();
    }
}
#[derive(Clone, Copy)]
enum Change {
    None,
    Word(u8, u32),
    Handle(u32),
    Deny,
}
fn hook(
    machine: &mut Machine<JitCpu>,
    name: &str,
    slot: u32,
    argc: u8,
    change: Change,
    timeout: bool,
) {
    let binding = machine.resolve_import("ntdll.dll", name).unwrap();
    let original = machine
        .sys
        .thunks
        .pristine_entry((binding - THUNK_BASE) / THUNK_STRIDE);
    let at = CODE + 0x4000 + slot * 0x400;
    let observed = DATA + 0x800 + slot * 0x40;
    machine.cpu.write(at + 0x200, &original).unwrap();
    let mut code = Asm::new(at);
    code.mov_eax_mem(observed)
        .add_eax_imm(1)
        .mov_mem_eax(observed);
    for i in 0..argc {
        code.mov_eax_esp(4 + i * 4)
            .mov_mem_eax(observed + 4 + u32::from(i) * 4);
    }
    if matches!(
        name,
        "NtCreateEvent"
            | "NtOpenEvent"
            | "NtCreateMutant"
            | "NtOpenMutant"
            | "NtCreateSemaphore"
            | "NtOpenSemaphore"
    ) {
        code.mov_eax_esp(12)
            .add_eax_imm(12)
            .mov_eax_ptr_eax()
            .mov_mem_eax(observed + 24);
    }
    if timeout {
        code.mov_eax_esp(20)
            .mov_eax_ptr_eax()
            .mov_mem_eax(observed + 24);
        code.mov_eax_esp(20)
            .add_eax_imm(4)
            .mov_eax_ptr_eax()
            .mov_mem_eax(observed + 28);
    }
    match change {
        Change::None => {}
        Change::Word(offset, value) => {
            code.bytes.extend_from_slice(&[0xc7, 0x44, 0x24, offset]);
            code.bytes.extend_from_slice(&value.to_le_bytes());
        }
        Change::Handle(pointer) => {
            code.mov_eax_mem(pointer);
            code.bytes.extend_from_slice(&[0x89, 0x44, 0x24, 4]);
        }
        Change::Deny => {
            code.bytes.extend_from_slice(&[0xb8, 0x22, 0, 0, 0xc0]);
            code.ret_n(u16::from(argc) * 4);
        }
    }
    if !matches!(change, Change::Deny) {
        code.jmp(at + 0x200);
    }
    machine.cpu.write(at, &code.bytes).unwrap();
    let modules = adv_os::modules(&machine.ext);
    let image = modules.get(modules.by_name("ntdll.dll").unwrap());
    let entry = match image.exports_by_name.get(name).unwrap() {
        adv_pe::ExportTarget::Rva(rva) => image.base + rva,
        _ => panic!("native code entry"),
    };
    let mut patch = Asm::new(entry);
    patch.jmp(at);
    machine.cpu.write(entry, &patch.bytes).unwrap();
}

#[test]
// spec: PERS-BRIDGE-4
fn event_frontends_execute_live_create_open_set_reset_hooks_and_native_parameter_changes() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        machine
            .cpu
            .write(DATA + 0x400, b"authored-live-event\0")
            .unwrap();
        let wide: Vec<_> = "Local\\authored-live-event\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        machine.cpu.write(DATA + 0x500, &wide).unwrap();
        hook(
            &mut machine,
            "NtCreateEvent",
            0,
            5,
            Change::Word(20, 1),
            false,
        );
        hook(&mut machine, "NtOpenEvent", 1, 3, Change::None, false);
        hook(
            &mut machine,
            "NtSetEvent",
            2,
            2,
            Change::Handle(DATA + 4),
            false,
        );
        hook(
            &mut machine,
            "NtResetEvent",
            3,
            2,
            Change::Handle(DATA + 4),
            false,
        );
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateEventA",
            &[I(0), I(0), I(0), I(DATA + 0x400)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "CreateEventW",
            &[I(0), I(0), I(0), I(0)],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(0)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(0)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 4), I(0)],
            DATA + 16,
        );
        call(
            &mut machine,
            &mut code,
            "OpenEventW",
            &[I(0x1f0003), I(1), I(DATA + 0x500)],
            DATA + 20,
        );
        call(&mut machine, &mut code, "SetEvent", &[M(DATA)], DATA + 24);
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(0)],
            DATA + 28,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 4), I(0)],
            DATA + 32,
        );
        call(&mut machine, &mut code, "SetEvent", &[M(DATA)], DATA + 36);
        call(&mut machine, &mut code, "ResetEvent", &[M(DATA)], DATA + 40);
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 4), I(0)],
            DATA + 44,
        );
        run(&mut machine, code);
        assert_ne!(word(&machine, 0), 0);
        assert_ne!(word(&machine, 4), 0);
        assert_eq!(
            (word(&machine, 8), word(&machine, 12), word(&machine, 16)),
            (0, 0x102, 0)
        );
        assert!(
            machine
                .sys
                .objects
                .same_object(word(&machine, 0), word(&machine, 20))
        );
        assert_eq!(
            machine.sys.objects.handle_flags(word(&machine, 20)),
            Some(1)
        );
        assert_eq!(
            (word(&machine, 24), word(&machine, 28), word(&machine, 32)),
            (1, 0x102, 0)
        );
        assert_eq!(
            (word(&machine, 36), word(&machine, 40), word(&machine, 44)),
            (1, 1, 0x102)
        );
        assert_eq!(
            (
                word(&machine, 0x800),
                word(&machine, 0x840),
                word(&machine, 0x880),
                word(&machine, 0x8c0)
            ),
            (2, 1, 2, 1)
        );
        assert_eq!(
            (
                word(&machine, 0x808),
                word(&machine, 0x810),
                word(&machine, 0x814)
            ),
            (0x1f0003, 1, 0)
        );
        assert_eq!((word(&machine, 0x818), word(&machine, 0x858)), (0x80, 2));
        assert_eq!((word(&machine, 0x888), word(&machine, 0x8c8)), (0, 0));
    }
}

#[test]
fn native_event_hook_failures_are_returned_without_allocating_aliases_or_changing_signals() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let handle = event(&mut machine, true);
        let baseline = machine
            .sys
            .objects
            .kernel_objects()
            .filter(|object| matches!(object, Object::Event { .. }))
            .count();
        machine.cpu.write(DATA + 0x400, b"denied-event\0").unwrap();
        for (slot, name, argc) in [
            (0, "NtCreateEvent", 5),
            (1, "NtOpenEvent", 3),
            (2, "NtSetEvent", 2),
            (3, "NtResetEvent", 2),
        ] {
            hook(&mut machine, name, slot, argc, Change::Deny, false);
        }
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateEventA",
            &[I(0), I(0), I(0), I(0)],
            DATA,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 4);
        call(
            &mut machine,
            &mut code,
            "OpenEventA",
            &[I(0x1f0003), I(0), I(DATA + 0x400)],
            DATA + 8,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 12);
        call(&mut machine, &mut code, "SetEvent", &[I(handle)], DATA + 16);
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 20);
        call(
            &mut machine,
            &mut code,
            "ResetEvent",
            &[I(handle)],
            DATA + 24,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 28);
        run(&mut machine, code);
        assert_eq!(
            (0..8).map(|i| word(&machine, i * 4)).collect::<Vec<_>>(),
            [0, 5, 0, 5, 0, 5, 0, 5]
        );
        assert!(matches!(
            machine.sys.objects.get(handle),
            Some(Object::Event { signaled: true, .. })
        ));
        assert_eq!(
            machine
                .sys
                .objects
                .kernel_objects()
                .filter(|object| matches!(object, Object::Event { .. }))
                .count(),
            baseline
        );
    }
}

#[test]
fn wait_all_acquires_atomically_wait_any_returns_its_index_and_native_validation_does_not_consume()
{
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let first = event(&mut machine, true);
        let second = event(&mut machine, false);
        array(&mut machine, &[first, second]);
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjects",
            &[I(2), I(ARRAY), I(1), I(0)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[I(first), I(0)],
            DATA + 4,
        );
        call(&mut machine, &mut code, "SetEvent", &[I(first)], 0);
        call(&mut machine, &mut code, "SetEvent", &[I(second)], 0);
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjects",
            &[I(2), I(ARRAY), I(1), I(0)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[I(first), I(0)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[I(second), I(0)],
            DATA + 16,
        );
        call(&mut machine, &mut code, "SetEvent", &[I(second)], 0);
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjects",
            &[I(2), I(ARRAY), I(0), I(0)],
            DATA + 20,
        );
        call(&mut machine, &mut code, "SetEvent", &[I(first)], 0);
        call(
            &mut machine,
            &mut code,
            "DuplicateHandle",
            &[
                I(u32::MAX),
                I(first),
                I(u32::MAX),
                I(DATA + 0x100),
                I(0),
                I(0),
                I(2),
            ],
            DATA + 24,
        );
        code.mov_eax_mem(DATA + 0x100).mov_mem_eax(ARRAY + 4);
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjects",
            &[I(2), I(ARRAY), I(1), I(0)],
            DATA + 28,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 32);
        code.mov_mem_imm(DATA + 0x380, 0);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtWaitForMultipleObjects",
            &[I(2), I(ARRAY), I(0), I(0), I(DATA + 0x380)],
            DATA + 36,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtWaitForMultipleObjects",
            &[I(0), I(ARRAY), I(1), I(0), I(0)],
            DATA + 40,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtWaitForMultipleObjects",
            &[I(2), I(DATA + 0x2000), I(1), I(0), I(0)],
            DATA + 44,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtWaitForMultipleObjects",
            &[I(2), I(ARRAY), I(2), I(0), I(0)],
            DATA + 48,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[I(first), I(0)],
            DATA + 52,
        );
        run(&mut machine, code);
        assert_eq!(
            (0..6).map(|i| word(&machine, i * 4)).collect::<Vec<_>>(),
            [0x102, 0, 0, 0x102, 0x102, 1]
        );
        assert_eq!(
            (word(&machine, 24), word(&machine, 28), word(&machine, 32)),
            (1, u32::MAX, 87)
        );
        assert_eq!(
            (
                word(&machine, 36),
                word(&machine, 40),
                word(&machine, 44),
                word(&machine, 48),
                word(&machine, 52)
            ),
            (0xc000000d, 0xc000000d, 0xc0000005, 0xc000000d, 0)
        );
    }
}

#[test]
// spec: PERS-BRIDGE-4
fn multiple_wait_hooks_observe_native_kind_relative_timeout_and_change_actual_acquisition_or_error()
{
    for jit in [false, true] {
        for denied in [false, true] {
            let mut machine = fixture(jit);
            let first = event(&mut machine, false);
            let second = event(&mut machine, true);
            array(&mut machine, &[first, second]);
            hook(
                &mut machine,
                "NtWaitForMultipleObjects",
                0,
                5,
                if denied {
                    Change::Deny
                } else {
                    Change::Word(12, 1)
                },
                true,
            );
            let mut code = Asm::new(CODE);
            call(&mut machine, &mut code, "SetLastError", &[I(0x12345678)], 0);
            call(
                &mut machine,
                &mut code,
                "WaitForMultipleObjects",
                &[I(2), I(ARRAY), I(1), I(25)],
                DATA,
            );
            call(&mut machine, &mut code, "GetLastError", &[], DATA + 4);
            run(&mut machine, code);
            assert_eq!(
                (word(&machine, 0), word(&machine, 4)),
                if denied {
                    (u32::MAX, 5)
                } else {
                    (1, 0x12345678)
                }
            );
            assert_eq!(
                (
                    word(&machine, 0x800),
                    word(&machine, 0x804),
                    word(&machine, 0x808),
                    word(&machine, 0x80c),
                    word(&machine, 0x810)
                ),
                (1, 2, ARRAY, 0, 0)
            );
            assert_eq!(
                (word(&machine, 0x818), word(&machine, 0x81c)),
                ((-250000i32) as u32, u32::MAX)
            );
            assert!(
                matches!(machine.sys.objects.get(second), Some(Object::Event { signaled, .. }) if *signaled == denied)
            );
        }
    }
}

#[test]
fn alertable_multiple_waits_deliver_fifo_apcs_on_the_blocked_target_and_nonalertable_waits_do_not()
{
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let first = event(&mut machine, false);
        let second = event(&mut machine, false);
        array(&mut machine, &[first, second]);
        hook(
            &mut machine,
            "NtWaitForMultipleObjects",
            0,
            5,
            Change::None,
            false,
        );
        let callback = CODE + 0x2000;
        let mut routine = Asm::new(callback);
        call(
            &mut machine,
            &mut routine,
            "GetCurrentThreadId",
            &[],
            DATA + 0x204,
        );
        routine.mov_eax_mem(DATA + 0x200);
        routine
            .bytes
            .extend_from_slice(&[0x6b, 0xc0, 10, 0x03, 0x44, 0x24, 4]);
        routine.mov_mem_eax(DATA + 0x200).ret_n(4);
        machine.cpu.write(callback, &routine.bytes).unwrap();
        let worker = CODE + 0x3000;
        let mut routine = Asm::new(worker);
        call(
            &mut machine,
            &mut routine,
            "WaitForMultipleObjectsEx",
            &[I(2), I(ARRAY), I(0), I(u32::MAX), I(1)],
            DATA + 0x208,
        );
        routine.bytes.extend_from_slice(&[0xb8, 37, 0, 0, 0]);
        routine.ret_n(4);
        machine.cpu.write(worker, &routine.bytes).unwrap();
        let mut code = Asm::new(CODE);
        for parameter in [1, 2] {
            call(
                &mut machine,
                &mut code,
                "QueueUserAPC",
                &[I(callback), I(u32::MAX - 1), I(parameter)],
                0,
            );
        }
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjectsEx",
            &[I(2), I(ARRAY), I(0), I(0), I(0)],
            DATA,
        );
        code.mov_eax_mem(DATA + 0x200).mov_mem_eax(DATA + 4);
        call(
            &mut machine,
            &mut code,
            "WaitForMultipleObjectsEx",
            &[I(2), I(ARRAY), I(0), I(0), I(1)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(worker), I(0), I(0), I(DATA + 16)],
            DATA + 12,
        );
        call(&mut machine, &mut code, "Sleep", &[I(0)], 0);
        for parameter in [3, 4] {
            call(
                &mut machine,
                &mut code,
                "QueueUserAPC",
                &[I(callback), M(DATA + 12), I(parameter)],
                0,
            );
        }
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 12), I(u32::MAX)],
            DATA + 20,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA + 12), I(DATA + 24)],
            DATA + 28,
        );
        run(&mut machine, code);
        assert_eq!(
            (word(&machine, 0), word(&machine, 4), word(&machine, 8)),
            (0x102, 0, 0xc0)
        );
        assert_eq!(
            (word(&machine, 20), word(&machine, 24), word(&machine, 28)),
            (0, 37, 1)
        );
        assert_eq!(
            (
                word(&machine, 0x200),
                word(&machine, 0x204),
                word(&machine, 0x208)
            ),
            (1234, word(&machine, 16), 0xc0)
        );
        assert_eq!(
            (
                word(&machine, 0x800),
                word(&machine, 0x810),
                word(&machine, 0x814)
            ),
            (3, 1, 0)
        );
    }
}
