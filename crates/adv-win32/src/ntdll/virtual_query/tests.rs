use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg},
};
use adv_machine::{
    Clock, Machine,
    layout::{PAGE, PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;

const DATA: u32 = 0x410000;
const OUT: u32 = DATA;
const RETURNED: u32 = DATA + 128;
const SENTINEL: u32 = 0x24681357;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.sys.set_process_identity(pid).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/nt-vm-query-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine
        .spawn("query caller", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    machine.cpu.map(DATA + PAGE, PAGE, Prot::READ).unwrap();
    machine
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn invoke<B: Backend>(
    machine: &mut Machine<B>,
    process: u32,
    address: u32,
    class: i32,
    output: u32,
    length: u32,
    returned: u32,
) -> Outcome {
    query(
        &mut env(machine),
        Handle(process),
        GuestPtr::new(address),
        class,
        GuestPtr::new(output),
        length,
        GuestPtr::new(returned),
    )
}
fn status(outcome: Outcome, expected: u32) {
    assert!(matches!(outcome, Outcome::Return(value) if value == expected));
}
fn reset<B: Backend>(machine: &mut Machine<B>) {
    for offset in (0..256).step_by(4) {
        machine.cpu.write_u32(DATA + offset, SENTINEL).unwrap();
    }
    crate::util::set_last_error(&mut env(machine), 0x13572468);
}
fn words<B: Backend>(machine: &Machine<B>) -> [u32; 7] {
    std::array::from_fn(|index| machine.cpu.read_u32(OUT + index as u32 * 4).unwrap())
}
fn unchanged<B: Backend>(machine: &mut Machine<B>) {
    assert_eq!(words(machine), [SENTINEL; 7]);
    assert_eq!(machine.cpu.read_u32(RETURNED).unwrap(), SENTINEL);
    assert_eq!(crate::util::last_error(&env(machine)), 0x13572468);
}
fn allocation<B: Backend>(
    machine: &mut Machine<B>,
    address: u32,
    length: u32,
    flags: u32,
    protect: u32,
) -> u32 {
    crate::kernel32::allocate_memory(&mut env(machine), address, length, flags, protect).unwrap()
}
fn local<B: Backend>(make: impl Fn() -> B) {
    let mut machine = machine(make(), 0x1000);
    let base = allocation(&mut machine, 0, PAGE * 4, 0x2000, 4);
    allocation(&mut machine, base + PAGE, PAGE * 2, 0x1000, 4);
    status(
        crate::kernel32::protect_memory(
            &mut env(&mut machine),
            base + PAGE * 2,
            PAGE,
            0x20,
            RETURNED,
        ),
        1,
    );
    for (offset, state, protection) in [
        (0, 0x2000, 0),
        (PAGE, 0x1000, 4),
        (PAGE * 2, 0x1000, 0x20),
        (PAGE * 3, 0x2000, 0),
    ] {
        reset(&mut machine);
        status(
            invoke(
                &mut machine,
                u32::MAX,
                base + offset + 31,
                0,
                OUT,
                32,
                RETURNED,
            ),
            0,
        );
        assert_eq!(
            words(&machine),
            [base + offset, base, 4, PAGE, state, protection, 0x20000]
        );
        assert_eq!(machine.cpu.read_u32(OUT + 28).unwrap(), SENTINEL);
        assert_eq!(machine.cpu.read_u32(RETURNED).unwrap(), BASIC_SIZE);
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x13572468);
    }
    // Exercise both registered six-argument adapters used by actual callers.
    for name in ["NtQueryVirtualMemory", "ZwQueryVirtualMemory"] {
        reset(&mut machine);
        let entry = machine.resolve_import("ntdll.dll", name).unwrap();
        machine.cpu.set_reg(Reg::Esp, DATA + 512);
        for (index, argument) in [PARK_ADDR, u32::MAX, base + PAGE + 17, 0, OUT, 28, 0]
            .into_iter()
            .enumerate()
        {
            machine
                .cpu
                .write_u32(DATA + 512 + index as u32 * 4, argument)
                .unwrap();
        }
        let index = (entry - THUNK_BASE) / THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        let mut call = env(&mut machine);
        call.thunk = index;
        status(handler(&mut call), 0);
        assert_eq!(
            words(&machine),
            [base + PAGE, base, 4, PAGE, 0x1000, 4, 0x20000]
        );
        assert_eq!(machine.cpu.read_u32(RETURNED).unwrap(), SENTINEL);
    }
    // The first pending process status does not destroy the live address space.
    assert!(
        machine
            .sys
            .process_identity
            .completion
            .begin_termination(37)
    );
    status(invoke(&mut machine, u32::MAX, base, 0, OUT, 28, 0), 0);
}
#[test]
fn basic_queries_report_real_region_runs_and_the_registered_adapter() {
    for jit in [false, true] {
        local(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    local(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn errors<B: Backend>(make: impl Fn() -> B) {
    let mut machine = machine(make(), 0x1000);
    let base = allocation(&mut machine, 0, PAGE, 0x3000, 4);
    let denied = machine.sys.objects.insert(Object::Process {
        identity: machine.sys.process_identity.clone(),
        access: 8,
    });
    let limited = machine.sys.objects.insert(Object::Process {
        identity: machine.sys.process_identity.clone(),
        access: 0x1000,
    });
    let thread = machine.sys.objects.insert(Object::Thread { ctx: 0 });
    for (handle, address, class, output, length, returned, expected) in [
        (u32::MAX, base, -1, OUT, 28, RETURNED, 0xc0000003),
        (u32::MAX, base, 0, OUT, 27, RETURNED, 0xc0000004),
        (u32::MAX, base, 0, 0, 0, RETURNED, 0xc0000004),
        (u32::MAX, u32::MAX, 0, OUT, 28, RETURNED, 0xc000000d),
        (u32::MAX, base, 0, 0, 28, RETURNED, 0xc0000005),
        (u32::MAX, base, 0, OUT + 1, 28, RETURNED, 0xc0000080),
        (u32::MAX, base, 0, OUT, 28, RETURNED + 1, 0xc0000080),
        (u32::MAX, base, 0, OUT, 28, 1, 0xc0000080),
        (u32::MAX, base, 0, OUT, 28, DATA + PAGE, 0xc0000005),
        (
            u32::MAX,
            base,
            0,
            DATA + PAGE - 16,
            28,
            RETURNED,
            0xc0000005,
        ),
        (u32::MAX, base, 0, OUT, PAGE * 2, RETURNED, 0xc0000005),
        (0, base, 0, OUT, 28, RETURNED, 0xc0000008),
        (0x12345678, base, 0, OUT, 28, RETURNED, 0xc0000008),
        (thread, base, 0, OUT, 28, RETURNED, 0xc0000024),
        (u32::MAX - 1, base, 0, OUT, 28, RETURNED, 0xc0000024),
        (denied, base, 0, OUT, 28, RETURNED, 0xc0000022),
        (limited, base, 0, OUT, 28, RETURNED, 0xc0000022),
    ] {
        reset(&mut machine);
        status(
            invoke(
                &mut machine,
                handle,
                address,
                class,
                output,
                length,
                returned,
            ),
            expected,
        );
        unchanged(&mut machine);
    }
    reset(&mut machine);
    status(
        invoke(&mut machine, u32::MAX, base, 0, OUT, 28, OUT + 12),
        0,
    );
    assert_eq!(machine.cpu.read_u32(OUT + 12).unwrap(), BASIC_SIZE);
}
#[test]
fn native_x86_outputs_require_writable_aligned_buffers_without_wow64_translation() {
    for jit in [false, true] {
        errors(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    errors(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn remote<B: Backend>(make: impl Fn() -> B) {
    for race in [0, 1, 2] {
        let mut parent = machine(make(), 0x1000);
        let mut child = machine(make(), 0x2000);
        child.sys.suspend_context(0);
        let base = allocation(&mut child, 0x50000000, PAGE * 2, 0x3000, 0x20);
        let before = crate::kernel32::query_memory(&mut env(&mut child), base + 37).unwrap();
        let allowed = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 0x400,
        });
        let services = processes::Services::default();
        parent.ext.insert(services.clone());
        reset(&mut parent);
        let Outcome::HostRequest { then } =
            invoke(&mut parent, allowed, base + 37, 0, OUT, 28, RETURNED)
        else {
            panic!("query must use the real remote VM owner");
        };
        let processes::Request::Visit { pid, context, task } = services.pop().unwrap() else {
            panic!("expected owner visit");
        };
        assert_eq!((pid, context), (0x2000, None));
        if race == 1 {
            child.sys.terminate_process_stopped(37);
        }
        task(Ok(&mut env(&mut child)));
        if race == 2 {
            parent.cpu.protect(DATA, PAGE, Prot::READ).unwrap();
        }
        status(
            then(&mut env(&mut parent)),
            match race {
                0 => 0,
                1 => 0xc000010a,
                _ => 0xc0000005,
            },
        );
        if race == 0 {
            assert_eq!(words(&parent), before);
            assert_eq!(parent.cpu.read_u32(RETURNED).unwrap(), BASIC_SIZE);
        } else {
            unchanged(&mut parent);
        }
        assert_eq!(
            crate::kernel32::query_memory(&mut env(&mut child), base + 37).unwrap(),
            before
        );
        assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
        if race == 1 {
            status(
                invoke(&mut parent, allowed, base, 0, OUT, 28, RETURNED),
                0xc000010a,
            );
            assert!(!services.pending());
        }
    }
}
#[test]
fn remote_queries_visit_live_owner_and_revalidate_caller_outputs() {
    for jit in [false, true] {
        remote(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    remote(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
