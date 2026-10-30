//! Machine dispatch keeps trace and diagnostic contracts around direct host calls.
use crate::leaf::trap;
use crate::*;
use crate::{Conv, Dispatch, Machine, Trace, layout::PARK_ADDR};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Exit, Prot, asm::Asm},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

struct Binding {
    conv: Conv,
    wide: bool,
}
impl crate::Resolver for Binding {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
        Some(crate::HostFn {
            conv: self.conv,
            handler: trap,
            data: 0,
            trace: false,
            guest: None,
            leaf: Some(if self.wide {
                |e| {
                    let value = e.arg(1);
                    e.write_u32(e.arg(0), value).unwrap();
                    0xfedcba98_12345678
                }
            } else {
                |e| {
                    let value = e.arg(1);
                    e.write_u32(e.arg(0), value).unwrap();
                    e.return32(value)
                }
            }),
        })
    }
}

fn fixture(conv: Conv, wide: bool) -> Machine<JitCpu> {
    fixture_on(conv, wide, true)
}
fn fixture_on(conv: Conv, wide: bool, native: bool) -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, native).unwrap(),
        Binding { conv, wide },
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("leaf test", CODE, &[], 65536).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    let target = machine.resolve_import("fixture.dll", "leaf").unwrap();
    machine.cpu.write_u32(DATA + 16, target).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(17).push_imm(DATA);
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&(DATA + 16).to_le_bytes());
    if matches!(conv, Conv::Cdecl { .. }) {
        code.add_esp(8);
    }
    code.jmp(PARK_ADDR);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
}

fn run(machine: &mut Machine<JitCpu>) {
    machine.cpu.set_reg(Reg::Eip, CODE);
    machine.cpu.set_reg(Reg::Esp, DATA + 0x3f00);
    machine.cpu.set_reg(Reg::Edx, 0x87654321);
    machine.cpu.set_reg(Reg::Eflags, 0xed7);
    let mut dispatch = Dispatch {
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        parked: None,
        yield_on_frame: false,
        defer_work: false,
    };
    assert!(matches!(machine.cpu.run(&mut dispatch), Exit::Parked));
    assert!(dispatch.parked.is_none());
}

// spec: A64-CTL-12, A64-REG-4
#[test]
fn direct_calls_match_the_interpreter_in_registers_stack_memory_and_flags() {
    for conv in [Conv::Stdcall { argc: 2 }, Conv::Cdecl { argc: 2 }] {
        for wide in [false, true] {
            let mut light = fixture(conv, wide);
            let mut traced = fixture_on(conv, wide, false);
            for _ in 0..3 {
                run(&mut light);
                run(&mut traced);
            }
            assert_eq!(
                light.cpu.capture_architecture().unwrap(),
                traced.cpu.capture_architecture().unwrap()
            );
            let mut a = vec![0; 0x4000];
            let mut b = a.clone();
            light.cpu.read(DATA, &mut a).unwrap();
            traced.cpu.read(DATA, &mut b).unwrap();
            assert_eq!(a, b);
            assert_eq!(light.sys.host_calls, 3);
            assert_eq!(traced.sys.host_calls, 3);
            assert_eq!(light.cpu.reg(Reg::Esp), DATA + 0x3f00);
        }
    }
}

// spec: A64-CTL-12
#[test]
fn trace_enabled_after_translation_records_direct_calls() {
    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl std::io::Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    for _ in 0..3 {
        run(&mut machine);
    }
    let output = Shared::default();
    machine.sys.trace = Trace::to_writer(Box::new(output.clone()));
    machine.sys.trace.all = true;
    run(&mut machine);
    machine.sys.trace = Trace::default();
    assert_eq!(machine.sys.host_calls, 4);
    assert!(String::from_utf8_lossy(&output.0.borrow()).contains("leaf"));
}

// spec: A64-CTL-10, A64-CTL-12
#[test]
fn published_thunk_copy_calls_directly_and_remains_patchable() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    let target = machine.cpu.read_u32(DATA + 16).unwrap();
    let index = (target - crate::layout::THUNK_BASE) / crate::layout::THUNK_STRIDE;
    let bytes = machine.sys.thunks.pristine_entry(index);
    let mut image = vec![0; 0x4000];
    image[..bytes.len()].copy_from_slice(&bytes);
    machine
        .cpu
        .map_private_copy_pages(CODE + 0x30000, &image, Prot::RX)
        .unwrap();
    machine
        .sys
        .thunks
        .register_thunk_pages(CODE + 0x30000, 0x4000);
    machine.cpu.write_u32(DATA + 16, CODE + 0x30000).unwrap();
    // Normal guest entries and callbacks can retain a host continuation.
    machine.sys.contexts[0].frames.push(crate::sched::Frame {
        id: 1,
        stack: DATA + 0x3ff0,
        return_floor: DATA + 0x3e00,
        unwindable: false,
        then: Box::new(|_, _| Outcome::Return(0)),
        callee_saved: [0; 4],
        origin: crate::sched::Origin {
            esp: DATA + 0x3ff0,
            ret_eip: PARK_ADDR,
            thunk: index,
            trace_args: None,
            interrupted: false,
        },
    });
    for _ in 0..3 {
        run(&mut machine);
    }
    assert_eq!(machine.sys.host_calls, 3);
    // An ordinary replacement returns directly and cleans the same arguments.
    assert_eq!(machine.sys.contexts[0].frames.len(), 1);
    machine
        .cpu
        .write(CODE + 0x30000, &[0xb8, 42, 0, 0, 0, 0xc2, 8, 0])
        .unwrap();
    run(&mut machine);
    assert_eq!(machine.cpu.reg(Reg::Eax), 42);
    assert_eq!(machine.sys.host_calls, 3);
}

// spec: A64-CTL-12
#[test]
fn direct_calls_reach_host_services_and_frame_budget() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    for _ in 0..3 {
        run(&mut machine);
    }
    machine.sys.stats.frame_budget = Some(crate::frame_budget::FrameBudget::new(
        machine.sys.stats.total,
        None,
    ));
    run(&mut machine);
    let mut csv = Vec::new();
    machine
        .sys
        .stats
        .write_host_csv(&machine.cpu, &machine.sys.thunks, &mut csv)
        .unwrap();
    assert!(
        String::from_utf8(csv)
            .unwrap()
            .contains("\"fixture.dll\",\"leaf\",1,")
    );
    // Close the API row through the usual frame accounting boundary.
    let budget = machine.sys.stats.frame_budget.as_mut().unwrap();
    budget.mark_frame(1);
    budget.finish(machine.sys.stats.total, None);
    assert!(budget.json().contains("\"api\":\"fixture.dll!leaf\""));
}

// spec: A64-CTL-12
#[test]
fn ordinary_leaf_handler_uses_owned_invocation_arguments() {
    let mut machine = fixture(Conv::Stdcall { argc: 2 }, false);
    let target = machine.resolve_import("fixture.dll", "leaf").unwrap();
    let thunk = (target - crate::layout::THUNK_BASE) / crate::layout::THUNK_STRIDE;
    machine.sys.invocation_arguments = Some(crate::invocation::Arguments {
        context: machine.sys.current,
        thunk,
        values: vec![DATA, 91].into(),
    });
    let result = trap(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    });
    assert!(matches!(result, Outcome::Return(91)));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 91);
}

/// A function returning its argument plus one, at `FUNCTION`.
const FUNCTION: u32 = CODE + 0x1000;
const PLUS_ONE: [u8; 8] = [0x8b, 0x44, 0x24, 0x04, 0x83, 0xc0, 0x01, 0xc3];

struct Doubling;
impl crate::Resolver for Doubling {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
        Some(crate::HostFn {
            conv: Conv::Cdecl { argc: 1 },
            handler: |env| Outcome::Return(env.arg(0) * 2),
            data: 0,
            trace: false,
            guest: None,
            leaf: None,
        })
    }
}

// spec: A64-CTL-14
#[test]
fn redirected_functions_run_the_target_while_their_bytes_are_unchanged() {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        Doubling,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("redirect test", CODE, &[], 65536).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RWX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(21).call(FUNCTION).add_esp(4).jmp(PARK_ADDR);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
    let thunk = machine.resolve_import("fixture.dll", "double").unwrap();
    machine
        .sys
        .redirects
        .insert(FUNCTION, (thunk, PLUS_ONE.into()));
    let results = |machine: &mut Machine<JitCpu>| {
        (0..3)
            .map(|_| {
                run(machine);
                machine.cpu.reg(Reg::Eax)
            })
            .collect::<Vec<_>>()
    };
    // Interpreted first visits reach the target too.
    assert_eq!(results(&mut machine), [42, 42, 42]);
    let mut changed = PLUS_ONE;
    changed[6] = 2;
    machine.cpu.write(FUNCTION, &changed).unwrap();
    assert_eq!(results(&mut machine), [23, 23, 23]);
    machine.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
    machine.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
    assert_eq!(results(&mut machine), [22, 22, 22]);
}

const SLOT: u32 = DATA + 0x4000;
/// A copy of `PLUS_ONE` that nothing redirects.
const OTHER: u32 = FUNCTION + 0x100;

/// Three times the dword its argument points at, or the argument itself
/// when it is below `DATA`.
fn triple<M: adv_cpu::body::Mem>(
    mem: &mut M,
    [at, ..]: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    let value = if at < DATA { at } else { mem.u32(at)? };
    Ok(adv_cpu::body::Ret::from(value * 3))
}
adv_cpu::body!(TRIPLE = triple, inputs: &[adv_cpu::body::Input::Stack(0)], outputs: adv_cpu::body::EAX, pop: 0);

/// `push argument; call [SLOT]; add esp, 4`, then the park, with `FUNCTION`
/// in the slot, redirected to the doubling thunk and bound to `TRIPLE`.
fn bound_fixture(argument: u32) -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        Doubling,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("direct body test", CODE, &[], 65536).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RWX).unwrap();
    machine.cpu.map(DATA, 0x8000, Prot::RW).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(argument);
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&SLOT.to_le_bytes());
    code.add_esp(4).jmp(PARK_ADDR);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
    machine.cpu.write(OTHER, &PLUS_ONE).unwrap();
    machine.cpu.write_u32(SLOT, FUNCTION).unwrap();
    let thunk = machine.resolve_import("fixture.dll", "double").unwrap();
    machine
        .sys
        .redirects
        .insert(FUNCTION, (thunk, PLUS_ONE.into()));
    machine.sys.direct_bodies.insert(FUNCTION, TRIPLE.direct);
    machine
}

fn results(machine: &mut Machine<JitCpu>) -> Vec<u32> {
    (0..3)
        .map(|_| {
            run(machine);
            machine.cpu.reg(Reg::Eax)
        })
        .collect()
}

// spec: A64-CALL-1, A64-CALL-2, A64-CALL-5, A64-CTL-11
#[test]
fn import_calls_reach_direct_bodies_until_the_slot_or_entry_changes() {
    let mut machine = bound_fixture(21);
    // Interpreted visits take the thunk, and so does the run whose
    // translation first watches the slot's page; translated calls then
    // call the body.
    let first = results(&mut machine);
    assert!(
        first.iter().all(|&value| value == 42 || value == 63),
        "{first:?}"
    );
    assert_eq!(first[2], 63);
    assert_eq!(
        machine.cpu.reg(Reg::Esp),
        DATA + 0x3f00,
        "the stack balances"
    );
    // A write to the import slot reaches translated calls (A64-CALL-1).
    machine.cpu.write_u32(SLOT, OTHER).unwrap();
    assert_eq!(results(&mut machine), [22, 22, 22]);
    // So does a patch of the entry (A64-CTL-11).
    machine.cpu.write_u32(SLOT, FUNCTION).unwrap();
    let mut changed = PLUS_ONE;
    changed[6] = 2;
    machine.cpu.write(FUNCTION, &changed).unwrap();
    assert_eq!(results(&mut machine), [23, 23, 23]);
}

// spec: A64-CALL-1, A64-CALL-2, A64-CALL-5
#[test]
fn forwarding_chains_call_the_body_without_a_guest_return_store() {
    let mut machine = bound_fixture(21);
    let first = CODE + 0x300;
    let second = CODE + 0x340;
    let inner = SLOT + 4;
    // A balanced hot-patch frame followed by two import forwarding jumps.
    let mut bytes = vec![0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d, 0xe9];
    bytes.extend_from_slice(&second.wrapping_sub(first + 11).to_le_bytes());
    machine.cpu.write(first, &bytes).unwrap();
    let mut bytes = vec![0xff, 0x25];
    bytes.extend_from_slice(&inner.to_le_bytes());
    machine.cpu.write(second, &bytes).unwrap();
    machine.cpu.write_u32(SLOT, first).unwrap();
    machine.cpu.write_u32(inner, FUNCTION).unwrap();
    assert_eq!(results(&mut machine)[2], 63);
    let below_arguments = DATA + 0x3ef8;
    machine.cpu.write_u32(below_arguments, 0x1357_2468).unwrap();
    run(&mut machine);
    assert_eq!(machine.cpu.reg(Reg::Eax), 63);
    assert_eq!(machine.cpu.read_u32(below_arguments).unwrap(), 0x1357_2468);
    machine.cpu.write_u32(inner, OTHER).unwrap();
    assert_eq!(results(&mut machine), [22; 3]);
    machine.cpu.write_u32(inner, FUNCTION).unwrap();
    assert_eq!(results(&mut machine)[2], 63);
    // A changed forwarding entry must run, including its register effects.
    let mut changed = vec![0xbb]; // mov ebx, 99
    changed.extend_from_slice(&99u32.to_le_bytes());
    changed.push(0xe9);
    changed.extend_from_slice(&second.wrapping_sub(first + 10).to_le_bytes());
    machine.cpu.write(first, &changed).unwrap();
    assert_eq!(results(&mut machine)[2], 63);
    assert_eq!(machine.cpu.reg(Reg::Ebx), 99);
}

// spec: A64-CALL-4
#[test]
fn a_body_the_window_stops_runs_on_the_full_path() {
    let mut machine = bound_fixture(DATA + 0x100);
    machine.cpu.write_u32(DATA + 0x100, 5).unwrap();
    assert_eq!(results(&mut machine)[2], 15);
    // An unmapped page stops the window form before it writes anything;
    // the full path, here the doubling thunk, decides.
    let unmapped = DATA + 0x10000;
    let mut machine = bound_fixture(unmapped);
    assert_eq!(results(&mut machine), [unmapped * 2; 3]);
}

struct Raising;
impl crate::Resolver for Raising {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<crate::HostFn> {
        Some(crate::HostFn {
            conv: Conv::Cdecl { argc: 1 },
            handler: |env| {
                Outcome::Raise(adv_cpu::backend::Fault {
                    kind: adv_cpu::backend::FaultKind::Read(env.arg(0)),
                    pc: FUNCTION,
                })
            },
            data: 0,
            trace: false,
            guest: None,
            leaf: None,
        })
    }
}

// spec: A64-CALL-4
#[test]
fn a_raised_fault_is_the_entrys_first_instruction_faulting() {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        Raising,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("raise test", CODE, &[], 65536).unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RWX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    let thunk = machine.resolve_import("fixture.dll", "raise").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x1234)
        .call(FUNCTION)
        .add_esp(4)
        .jmp(PARK_ADDR);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
    machine
        .sys
        .redirects
        .insert(FUNCTION, (thunk, PLUS_ONE.into()));
    machine.cpu.set_reg(Reg::Eip, CODE);
    machine.cpu.set_reg(Reg::Esp, DATA + 0x3f00);
    let mut dispatch = Dispatch {
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        parked: None,
        yield_on_frame: false,
        defer_work: false,
    };
    assert!(matches!(machine.cpu.run(&mut dispatch), Exit::Parked));
    // Without a guest exception handler the fault stops the machine, with
    // the state at the entry: its return address on the stack.
    let Some(Parked::Fail(message)) = dispatch.parked else {
        panic!("the fault did not stop the machine");
    };
    assert!(
        message.contains("Read(4660)") && message.contains(&format!("pc: {FUNCTION}")),
        "{message}"
    );
    assert_eq!(machine.cpu.reg(Reg::Esp), DATA + 0x3f00 - 8);
    assert_eq!(machine.cpu.read_u32(DATA + 0x3f00 - 8).unwrap(), CODE + 10);
}

// spec: EXEC-CALL-1
#[test]
fn ordinary_host_call_without_diagnostics_reads_no_stack_or_clock() {
    struct Empty;
    impl Resolver for Empty {
        fn provides(&self, _: &str) -> bool {
            true
        }
        fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
            Some(HostFn {
                conv: Conv::Stdcall { argc: 0 },
                handler: |_| Outcome::Return(7),
                data: 0,
                trace: false,
                guest: None,
                leaf: None,
            })
        }
    }
    struct Unreadable {
        regs: [u32; 16],
        reads: std::cell::Cell<usize>,
    }
    impl Guest for Unreadable {
        fn reg(&self, reg: Reg) -> u32 {
            self.regs[reg as usize]
        }
        fn set_reg(&mut self, reg: Reg, value: u32) {
            self.regs[reg as usize] = value;
        }
        fn read(&self, addr: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
            self.reads.set(self.reads.get() + 1);
            Err(MemFault {
                addr,
                len: bytes.len() as u32,
            })
        }
        fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
            Err(MemFault {
                addr,
                len: bytes.len() as u32,
            })
        }
        fn map(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
        fn protect(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
        fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        Empty,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.spawn("quiet call", CODE, &[], 65536).unwrap();
    let target = machine.resolve_import("fixture", "empty").unwrap();
    let index = (target - layout::THUNK_BASE) / layout::THUNK_STRIDE;
    let mut cpu = Unreadable {
        regs: [0; 16],
        reads: Default::default(),
    };
    let lookups = thunk::LOOKUPS.get();
    let clocks = stats::CLOCK_READS.get();
    let mut dispatch = Dispatch {
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        parked: None,
        yield_on_frame: false,
        defer_work: false,
    };
    assert!(dispatch.host_call(&mut cpu, index).is_ok());
    assert_eq!(thunk::LOOKUPS.get() - lookups, 1);
    assert_eq!(stats::CLOCK_READS.get(), clocks);
    assert_eq!(cpu.reads.get(), 0);
    assert_eq!(machine.sys.recent.iter().count(), 0);
    machine.sys.stats.services_enabled = true;
    let mut dispatch = Dispatch {
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        parked: None,
        yield_on_frame: false,
        defer_work: false,
    };
    assert!(dispatch.host_call(&mut cpu, index).is_ok());
    assert_eq!(cpu.reads.get(), 1);
    assert_eq!(machine.sys.recent.iter().count(), 1);
    assert_eq!(stats::CLOCK_READS.get(), clocks + 1);
}

fn yielding_triple<M: adv_cpu::body::Mem>(
    mem: &mut M,
    args: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    let mut ret = triple(mem, args)?;
    ret.yield_now = true;
    Ok(ret)
}
adv_cpu::body!(YIELDING_TRIPLE = yielding_triple, inputs: &[adv_cpu::body::Input::Stack(0)], outputs: adv_cpu::body::EAX, pop: 0);

// spec: A64-CALL-2, A64-CALL-5, NAT-BODY-18
#[test]
fn a_completed_body_yields_at_its_continuation_without_repeating_work() {
    for entry in [false, true] {
        let mut machine = bound_fixture(21);
        machine
            .sys
            .direct_bodies
            .insert(FUNCTION, YIELDING_TRIPLE.direct);
        let next = if entry {
            let mut code = Asm::new(CODE);
            code.push_imm(21);
            code.bytes.push(0xbf);
            code.bytes.extend_from_slice(&FUNCTION.to_le_bytes());
            code.bytes.extend_from_slice(&[0xff, 0xd7]);
            let next = CODE + code.bytes.len() as u32;
            code.add_esp(4).jmp(PARK_ADDR);
            machine.cpu.write(CODE, &code.bytes).unwrap();
            next
        } else {
            CODE + 11
        };
        let mut yielded = false;
        for _ in 0..4 {
            machine.cpu.set_reg(Reg::Eip, CODE);
            machine.cpu.set_reg(Reg::Esp, DATA + 0x3f00);
            let mut dispatch = Dispatch {
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                parked: None,
                yield_on_frame: false,
                defer_work: false,
            };
            assert!(matches!(machine.cpu.run(&mut dispatch), Exit::Parked));
            let Some(reason) = dispatch.parked.take() else {
                continue;
            };
            assert!(matches!(reason, Parked::ServiceYield { resume_eip } if resume_eip == next));
            assert_eq!(machine.cpu.reg(Reg::Eax), 63);
            assert_eq!(machine.cpu.reg(Reg::Esp), DATA + 0x3efc);
            assert!(machine.park(reason).is_none());
            assert_eq!(machine.cpu.reg(Reg::Eip), next);
            let mut dispatch = Dispatch {
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                parked: None,
                yield_on_frame: false,
                defer_work: false,
            };
            assert!(matches!(machine.cpu.run(&mut dispatch), Exit::Parked));
            assert!(dispatch.parked.is_none());
            assert_eq!(machine.cpu.reg(Reg::Eax), 63);
            assert_eq!(machine.cpu.reg(Reg::Esp), DATA + 0x3f00);
            yielded = true;
        }
        assert!(yielded);
    }
}

fn vector_triple<M: adv_cpu::body::Mem>(
    mem: &mut M,
    args: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let saved: u64;
        std::arch::asm!("mrs {saved}, fpcr", "msr fpcr, {control}", saved = out(reg) saved, control = in(reg) 0x400000u64, options(nostack));
        std::arch::asm!(
            "movi v9.16b, #0", "movi v16.16b, #0", "movi v17.16b, #0",
            "movi v18.16b, #0", "movi v19.16b, #0", "movi v20.16b, #0",
            "movi v21.16b, #0", "movi v22.16b, #0", "movi v23.16b, #0",
            out("v9") _, out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _, options(nostack)
        );
        std::arch::asm!("msr fpcr, {saved}", saved = in(reg) saved, options(nostack));
    }
    triple(mem, args)
}
adv_cpu::body!(VECTOR_TRIPLE = vector_triple, inputs: &[adv_cpu::body::Input::Stack(0)], outputs: adv_cpu::body::EAX, pop: 0);

struct VectorBody;
impl Resolver for VectorBody {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
        Some(HostFn {
            conv: Conv::Cdecl { argc: 1 },
            handler: |env| Outcome::Return(env.arg(0) * 3),
            data: 0,
            trace: false,
            guest: None,
            leaf: None,
        })
    }
}

// spec: A64-CALL-3
#[test]
fn direct_body_keeps_dirty_mmx_and_xmm_values() {
    let mut results = Vec::new();
    for jit in [false, true] {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            VectorBody,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("vector body", CODE, &[], 65536).unwrap();
        machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x8000, Prot::RW).unwrap();
        machine.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
        machine.cpu.write_u32(SLOT, FUNCTION).unwrap();
        let thunk = machine.resolve_import("fixture", "vector").unwrap();
        machine
            .sys
            .redirects
            .insert(FUNCTION, (thunk, PLUS_ONE.into()));
        machine
            .sys
            .direct_bodies
            .insert(FUNCTION, VECTOR_TRIPLE.direct);
        #[cfg(target_arch = "aarch64")]
        let before_fpcr: u64;
        #[cfg(target_arch = "aarch64")]
        unsafe {
            std::arch::asm!("mrs {saved}, fpcr", saved = out(reg) before_fpcr, options(nostack));
        }

        let mut code = vec![
            0x66, 0x0f, 0x76, 0xc0, // pcmpeqd xmm0, xmm0
            0x0f, 0x76, 0xc9, // pcmpeqd mm1, mm1
            0x68,
        ];
        code.extend_from_slice(&21u32.to_le_bytes());
        code.extend_from_slice(&[0xff, 0x15]);
        code.extend_from_slice(&SLOT.to_le_bytes());
        code.extend_from_slice(&[0x83, 0xc4, 4, 0x66, 0x0f, 0x7f, 0x05]);
        code.extend_from_slice(&(DATA + 0x100).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x7f, 0x0d]);
        code.extend_from_slice(&(DATA + 0x110).to_le_bytes());
        code.push(0xe9);
        code.extend_from_slice(
            &PARK_ADDR
                .wrapping_sub(CODE + code.len() as u32 + 4)
                .to_le_bytes(),
        );
        machine.cpu.write(CODE, &code).unwrap();
        for _ in 0..4 {
            run(&mut machine);
        }
        assert_eq!(machine.cpu.reg(Reg::Eax), 63);
        let mut output = [0; 24];
        machine.cpu.read(DATA + 0x100, &mut output).unwrap();
        assert_eq!(output, [0xff; 24]);
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let after_fpcr: u64;
            std::arch::asm!("mrs {saved}, fpcr", saved = out(reg) after_fpcr, options(nostack));
            assert_eq!(after_fpcr, before_fpcr);
        }
        results.push((
            output,
            machine.cpu.reg(Reg::Edx),
            machine.cpu.reg(Reg::Eflags) & 0x400,
        ));
    }
    assert_eq!(results[0], results[1]);
}

fn memory_body<M: adv_cpu::body::Mem>(
    mem: &mut M,
    [a, b, n, kind, ..]: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    if kind == 2 {
        for i in 0..n {
            let x = unsafe { mem.input(a + i, 1)?.read() };
            let y = unsafe { mem.input(b + i, 1)?.read() };
            if x != y {
                return Ok((x.cmp(&y) as i32 as u32).into());
            }
        }
        return Ok(0u32.into());
    }
    if n != 0 {
        let from = if kind == 0 {
            mem.input(b, n)?
        } else {
            std::ptr::null()
        };
        let to = mem.output(a, n)?;
        unsafe {
            if kind == 0 {
                std::ptr::copy(from, to, n as usize);
            } else {
                std::ptr::write_bytes(to, b as u8, n as usize);
            }
        }
        mem.commit()?;
    }
    Ok(if kind == 0 {
        u64::from(a) << 32 | u64::from(a)
    } else {
        u64::from(a)
    }
    .into())
}
fn memory_move<M: adv_cpu::body::Mem>(
    mem: &mut M,
    mut args: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    args[3] = 0;
    memory_body(mem, args)
}
fn memory_set<M: adv_cpu::body::Mem>(
    mem: &mut M,
    mut args: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    args[3] = 1;
    memory_body(mem, args)
}
fn memory_compare<M: adv_cpu::body::Mem>(
    mem: &mut M,
    mut args: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    args[3] = 2;
    memory_body(mem, args)
}
const MEMORY_ARGS: &[adv_cpu::body::Input] = &[
    adv_cpu::body::Input::Stack(0),
    adv_cpu::body::Input::Stack(1),
    adv_cpu::body::Input::Stack(2),
];
adv_cpu::body!(MEMORY_MOVE = memory_move, inputs: MEMORY_ARGS, outputs: adv_cpu::body::EAX | adv_cpu::body::EDX, pop: 0, inline: Some(adv_cpu::body::Inline::Move));
adv_cpu::body!(MEMORY_SET = memory_set, inputs: MEMORY_ARGS, outputs: adv_cpu::body::EAX, pop: 0, inline: Some(adv_cpu::body::Inline::Set));
adv_cpu::body!(MEMORY_COMPARE = memory_compare, inputs: MEMORY_ARGS, outputs: adv_cpu::body::EAX, pop: 0, inline: Some(adv_cpu::body::Inline::Compare));

fn full_memory(env: &mut Env<'_>, kind: u32) -> Outcome {
    let args = [env.arg(0), env.arg(1), env.arg(2), kind, 0, 0, 0];
    let mut scratch = adv_cpu::body::Scratch::default();
    let mut memory = adv_cpu::body::GuestMem::new(env.cpu, &mut scratch);
    match memory_body(&mut memory, args) {
        Ok(ret) if kind == 0 => Outcome::Return64(ret.value),
        Ok(ret) => Outcome::Return(ret.value as u32),
        Err(adv_cpu::body::Stop::Fault { address, write }) => {
            Outcome::Raise(adv_cpu::backend::Fault {
                kind: if write {
                    adv_cpu::backend::FaultKind::Write(address)
                } else {
                    adv_cpu::backend::FaultKind::Read(address)
                },
                pc: FUNCTION,
            })
        }
        Err(stop) => panic!("unexpected full-path stop: {stop:?}"),
    }
}
struct MemoryBody(u32);
impl Resolver for MemoryBody {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
        Some(HostFn {
            conv: Conv::Cdecl { argc: 3 },
            handler: match self.0 {
                0 => |e| full_memory(e, 0),
                1 => |e| full_memory(e, 1),
                _ => |e| full_memory(e, 2),
            },
            data: 0,
            trace: false,
            guest: None,
            leaf: None,
        })
    }
}

// spec: A64-CALL-2, A64-CALL-3, A64-CALL-4, A64-CALL-8
#[test]
fn memory_body_call_sites_match_the_interpreter_and_full_handlers() {
    for (kind, body) in [MEMORY_MOVE, MEMORY_SET, MEMORY_COMPARE]
        .into_iter()
        .enumerate()
    {
        for len in [0, 4, 8, 16, 63, 64, 65] {
            for blocked in [false, true] {
                let execute = |jit| {
                    let mut m = Machine::new(
                        JitCpu::new(PARK_ADDR, jit).unwrap(),
                        MemoryBody(kind as u32),
                        Clock::virtual_time(),
                    )
                    .unwrap();
                    m.spawn("memory body", CODE, &[], 65536).unwrap();
                    m.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
                    m.cpu.map(DATA, 0x8000, Prot::RW).unwrap();
                    let a = DATA + 0x101;
                    let b = if kind == 1 { 0xa5 } else { DATA + 0x100 };
                    let thunk = m.resolve_import("fixture", "memory").unwrap();
                    m.cpu.write(FUNCTION, &PLUS_ONE).unwrap();
                    m.sys.redirects.insert(FUNCTION, (thunk, PLUS_ONE.into()));
                    m.sys.direct_bodies.insert(FUNCTION, body.direct);
                    m.cpu.write_u32(SLOT, FUNCTION).unwrap();
                    let mut code = Asm::new(CODE);
                    code.push_imm(len).push_imm(b).push_imm(a);
                    code.bytes.extend_from_slice(&[0xff, 0x15]);
                    code.bytes.extend_from_slice(&SLOT.to_le_bytes());
                    code.add_esp(12).jmp(PARK_ADDR);
                    m.cpu.write(CODE, &code.bytes).unwrap();
                    let seed: Vec<u8> = (0..128).map(|n| (n * 71) as u8).collect();
                    for _ in 0..4 {
                        m.cpu.write(DATA + 0x100, &seed).unwrap();
                        if blocked {
                            m.cpu.protect(DATA, 0x4000, Prot::RWX).unwrap();
                        }
                        run(&mut m);
                    }
                    let mut bytes = [0; 128];
                    m.cpu.read(DATA + 0x100, &mut bytes).unwrap();
                    (
                        m.cpu.reg(Reg::Eax),
                        m.cpu.reg(Reg::Edx),
                        m.cpu.reg(Reg::Esp),
                        m.cpu.reg(Reg::Eflags) & 0x400,
                        bytes,
                    )
                };
                assert_eq!(
                    execute(true),
                    execute(false),
                    "kind={kind} len={len} blocked={blocked}"
                );
            }
        }
    }
}
