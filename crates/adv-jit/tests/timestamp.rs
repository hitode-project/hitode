use adv_jit::{
    ApiCall, ApiMemory, ApiOutcome, EXEC, HostServices, Machine, READ, Region, RunOutcome,
    TSC_FREQ, WRITE,
};
use iced_x86::code_asm::*;
use pal_machine::Clock;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x16000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x1f800;

fn code(reads: u32) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    for index in 0..reads {
        assembler.rdtsc().unwrap();
        assembler.mov(dword_ptr(DATA + index * 8), eax).unwrap();
        assembler.mov(dword_ptr(DATA + index * 8 + 4), edx).unwrap();
    }
    assembler.ret().unwrap();
    assembler.assemble(u64::from(CODE)).unwrap()
}
fn machine(native: bool, reads: u32) -> Machine {
    let bytes = code(reads);
    let mut machine = Machine::new(CODE, 0x10000).unwrap();
    machine.protect(CODE, 0x10000, READ | WRITE).unwrap();
    machine.write(CODE, &bytes).unwrap();
    machine.protect(CODE, 4096, READ | EXEC).unwrap();
    machine.state.eip = CODE;
    machine.state.regs = [
        0xaabbccdd, 0x11223344, 0xeeff0011, 0x55667788, STACK, 0xabcdef12, 0x12345678, 0x87654321,
    ];
    machine.state.eflags = 0xcd7;
    machine.write(STACK, &STOP.to_le_bytes()).unwrap();
    if native {
        machine.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    machine
}
fn ticks(machine: &Machine, index: u32) -> u64 {
    u64::from_le_bytes(
        machine
            .memory()
            .read(DATA + index * 8, 8)
            .unwrap()
            .try_into()
            .unwrap(),
    )
}
struct FixedClock {
    now: u64,
    reads: u32,
}
impl HostServices for FixedClock {
    fn guest_time_us(&mut self) -> Option<u64> {
        self.reads += 1;
        Some(self.now)
    }
    fn call(&mut self, _: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        panic!("no API in fixture")
    }
}
struct OwnerClock(Clock);
impl HostServices for OwnerClock {
    fn guest_time_us(&mut self) -> Option<u64> {
        Some(self.0.guest_now_us())
    }
    fn call(&mut self, _: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        panic!("no API in fixture")
    }
}

#[test]
fn consecutive_native_and_interpreted_reads_preserve_flags_and_other_registers() {
    for native in [false, true] {
        let mut machine = machine(native, 3);
        let initial = machine.state.clone();
        let mut clock = FixedClock {
            now: 4_294_968,
            reads: 0,
        };
        assert_eq!(
            machine.run_with_services(STOP, 100, &mut clock).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(
            clock.reads, 3,
            "native calls cannot be hoisted or eliminated"
        );
        assert_eq!(
            [ticks(&machine, 0), ticks(&machine, 1), ticks(&machine, 2)],
            [4_294_968_000, 4_294_968_001, 4_294_968_002]
        );
        assert_eq!(machine.state.regs[0], 706);
        assert_eq!(machine.state.regs[2], 1);
        for index in [1, 3, 5, 6, 7] {
            assert_eq!(machine.state.regs[index], initial.regs[index]);
        }
        assert_eq!(machine.state.regs[4], STACK + 4);
        assert_eq!(machine.state.eflags, initial.eflags);
        assert_eq!(machine.state.xmm, initial.xmm);
        assert_eq!(machine.state.x87, initial.x87);
        if native {
            assert!(machine.stats.native_entries > 0);
            assert_eq!(machine.stats.interpreter_instructions, 0);
        }
    }
}

#[test]
fn restored_contexts_and_executor_changes_share_one_monotonic_counter() {
    let mut machine = machine(false, 1);
    let saved = machine.save_context().unwrap();
    let mut clock = FixedClock {
        now: 1_000_000,
        reads: 0,
    };
    machine.run_with_services(STOP, 100, &mut clock).unwrap();
    assert_eq!(ticks(&machine, 0), TSC_FREQ);
    machine.restore_context(&saved).unwrap();
    machine
        .add_region(Region::new(CODE, code(1).len()))
        .unwrap();
    machine.run_with_services(STOP, 100, &mut clock).unwrap();
    assert_eq!(ticks(&machine, 0), TSC_FREQ + 1);
    machine.restore_context(&saved).unwrap();
    clock.now += 10;
    machine.run_with_services(STOP, 100, &mut clock).unwrap();
    assert_eq!(ticks(&machine, 0), TSC_FREQ + 10_000);
    assert_eq!(clock.reads, 3);
}

#[test]
fn virtual_clock_progress_jump_and_qpc_ratio_use_the_execution_owner() {
    for native in [false, true] {
        let mut machine = machine(native, 3);
        let saved = machine.save_context().unwrap();
        let mut clock = OwnerClock(Clock::virtual_time());
        let before = clock.0.now_us();
        machine.run_with_services(STOP, 100, &mut clock).unwrap();
        assert_eq!(clock.0.now_us(), before + 3);
        assert_eq!(
            ticks(&machine, 2),
            clock.0.qpc() * (TSC_FREQ / pal_machine::clock::QPC_FREQ)
        );
        let deadline = clock.0.now_us() + 10_000;
        clock.0.advance_to(deadline);
        machine.restore_context(&saved).unwrap();
        machine.run_with_services(STOP, 100, &mut clock).unwrap();
        assert_eq!(ticks(&machine, 0), (deadline + 1) * 1000);
    }
}

#[test]
fn paused_real_owner_excludes_host_pause_duration() {
    for native in [false, true] {
        let mut machine = machine(native, 3);
        let saved = machine.save_context().unwrap();
        let mut clock = OwnerClock(Clock::real());
        clock.0.pause();
        let paused = clock.0.now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        machine.run_with_services(STOP, 100, &mut clock).unwrap();
        assert_eq!(clock.0.now_us(), paused);
        assert_eq!(
            [ticks(&machine, 0), ticks(&machine, 1), ticks(&machine, 2)],
            [paused * 1000, paused * 1000 + 1, paused * 1000 + 2]
        );
        clock.0.resume();
        let resumed = clock.0.qpc() * (TSC_FREQ / pal_machine::clock::QPC_FREQ);
        machine.restore_context(&saved).unwrap();
        machine.run_with_services(STOP, 100, &mut clock).unwrap();
        assert!(ticks(&machine, 0) >= resumed);
        assert!(
            ticks(&machine, 2) <= clock.0.qpc() * (TSC_FREQ / pal_machine::clock::QPC_FREQ) + 999
        );
    }
}

#[test]
fn single_step_completes_rdtsc_then_reports_debug_trap() {
    let mut machine = machine(true, 1);
    machine.state.eflags |= 1 << 8;
    let original = machine.state.clone();
    let mut clock = FixedClock {
        now: 1_000_000,
        reads: 0,
    };
    assert_eq!(
        machine.run_with_services(STOP, 1, &mut clock).unwrap(),
        RunOutcome::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(machine.state.eip, CODE + 2);
    assert_eq!(machine.state.regs[0], TSC_FREQ as u32);
    assert_eq!(machine.state.regs[2], 0);
    assert_eq!(machine.state.eflags, original.eflags);
    assert_eq!(machine.state.fuel, 0);
    assert_eq!(clock.reads, 1);
}

#[test]
fn standalone_default_clock_reads_progress_without_host_cpu_frequency() {
    for native in [false, true] {
        let mut machine = machine(native, 3);
        assert_eq!(machine.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert!(ticks(&machine, 0) < ticks(&machine, 1));
        assert!(ticks(&machine, 1) < ticks(&machine, 2));
    }
}

#[cfg(feature = "pal")]
#[test]
fn pal_rdtsc_and_win32_qpc_share_the_same_clock_on_both_executors() {
    use adv_jit::pal::JitCpu;
    use pal_cpu::{Guest, Prot};
    use pal_machine::{Machine as PalMachine, Registry, RunResult};
    for native in [false, true] {
        let mut registry = Registry::default();
        pal_win32::register(&mut registry);
        let cpu = JitCpu::new(pal_machine::layout::PARK_ADDR, native).unwrap();
        let mut machine = PalMachine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let qpc = machine
            .resolve_import("kernel32.dll", "QueryPerformanceCounter")
            .unwrap();
        let exit = machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        assembler.rdtsc().unwrap();
        assembler.mov(dword_ptr(DATA), eax).unwrap();
        assembler.mov(dword_ptr(DATA + 4), edx).unwrap();
        assembler.push(DATA + 8).unwrap();
        assembler.call(u64::from(qpc)).unwrap();
        assembler.rdtsc().unwrap();
        assembler.mov(dword_ptr(DATA + 16), eax).unwrap();
        assembler.mov(dword_ptr(DATA + 20), edx).unwrap();
        assembler.push(0).unwrap();
        assembler.call(u64::from(exit)).unwrap();
        let bytes = assembler.assemble(u64::from(CODE)).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.write(CODE, &bytes).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.machine_mut().set_async_compilation(false);
        if native {
            machine
                .cpu
                .machine_mut()
                .add_region(Region::new(CODE, bytes.len()))
                .unwrap();
        }
        machine
            .spawn("authored clock fixture", CODE, &[], 0x10000)
            .unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        let mut data = [0; 24];
        machine.cpu.read(DATA, &mut data).unwrap();
        let values: Vec<_> = data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| u64::from_le_bytes(*p))
            .collect();
        assert_eq!(values[0] + 1000, values[1] * 100);
        assert_eq!(values[2], values[1] * 100 + 1000);
    }
}
