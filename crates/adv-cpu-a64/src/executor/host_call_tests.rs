//! Authored thunk programs compare direct host calls with the ordinary INT path.
use super::*;
use crate::native::Checks;
use crate::translate::Features;
use adv_cpu_core::backend::asm::Asm;
use adv_cpu_core::memory::{EXEC, READ, WRITE};

const CODE: u32 = 0x400000;
const THUNK: u32 = 0x7e000000;
const DATA: u32 = 0x410000;
const STACK: u32 = 0x420000;
const WORD: u32 = 0xad800099;

fn read_word(memory: &Memory, address: u32) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        memory.read(address, 4)?.try_into().unwrap(),
    ))
}

const THUNK_INDEX: u32 = 7;

struct Calls {
    enabled: bool,
    direct: u32,
    traps: u32,
    delay: bool,
    interrupt: Option<Arc<AtomicBool>>,
}
impl Calls {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            direct: 0,
            traps: 0,
            delay: false,
            interrupt: None,
        }
    }
    fn result(&self, state: &mut CpuState, memory: &mut Memory) {
        let word = read_word(memory, state.regs[4]).unwrap();
        state.regs[4] = state.regs[4].wrapping_add(4);
        let arg = read_word(memory, state.regs[4] + 4).unwrap();
        memory.write(arg, &word.to_le_bytes()).unwrap();
        state.regs[0] = word;
    }
}
impl ExecutionTrap for Calls {
    fn host_thunk(&self, pc: u32, word: u32) -> Option<u32> {
        (self.enabled && pc == THUNK + 5 && [WORD, WORD + 1].contains(&word)).then_some(THUNK_INDEX)
    }
    fn host_call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        thunk: u32,
    ) -> Result<bool, String> {
        assert_eq!((thunk, state.eip), (THUNK_INDEX, THUNK + 7));
        self.direct += 1;
        if self.delay {
            let end = Instant::now() + std::time::Duration::from_millis(20);
            while Instant::now() < end {
                std::hint::spin_loop();
            }
        }
        if let Some(flag) = &self.interrupt {
            flag.store(true, Ordering::Release);
        }
        self.result(state, memory);
        Ok(false)
    }
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String> {
        assert_eq!(vector, 0x2e);
        self.traps += 1;
        self.result(state, memory);
        Ok(false)
    }
}

fn fixture(config: (bool, bool, Checks)) -> (Executor, u32) {
    let mut executor = Executor::address_space().unwrap();
    for addr in [CODE, THUNK, DATA, STACK] {
        executor.map(addr, 0x4000, READ | WRITE).unwrap();
    }
    let mut thunk = Asm::new(THUNK);
    thunk.push_imm(WORD).trap().ret_n(4);
    executor.write(THUNK, &thunk.bytes).unwrap();
    executor.protect(THUNK, 0x4000, READ | EXEC).unwrap();
    executor.write(DATA, &THUNK.to_le_bytes()).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA + 32);
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.mov_mem_eax(DATA + 40);
    let stop = code.here();
    executor.write(CODE, &code.bytes).unwrap();
    executor.protect(CODE, 0x4000, READ | EXEC).unwrap();
    executor.state.eip = CODE;
    executor.state.regs = [
        0x11111111,
        0x22222222,
        0x33333333,
        0x44444444,
        STACK + 0x3f00,
        STACK + 0x3f80,
        0x66666666,
        0x77777777,
    ];
    executor.state.eflags = 0x202;
    executor.state.xmm = [[0xa5; 16]; 8];
    let native = executor.native.as_mut().unwrap();
    native.features = Features { flagm: config.0 };
    native.first_visits = config.1;
    native.checks = config.2;
    (executor, stop)
}

fn bytes(executor: &Executor) -> Vec<u8> {
    [
        executor.memory.read(DATA, 0x4000).unwrap(),
        executor.memory.read(STACK, 0x4000).unwrap(),
    ]
    .concat()
}

// spec: A64-CTL-7, A64-CTL-12, A64-REG-4, A64-VER-1
#[test]
fn direct_and_trapped_calls_match_at_instruction_budget_cuts() {
    for flagm in [false, true] {
        for first in [false, true] {
            for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
                let (mut light, stop) = fixture((flagm, first, checks));
                let (mut ordinary, _) = fixture((flagm, first, checks));
                let initial = light.state.clone();
                let data = light.memory.read(DATA, 0x4000).unwrap().to_vec();
                let stack = light.memory.read(STACK, 0x4000).unwrap().to_vec();
                let mut direct = Calls::new(true);
                let mut traps = Calls::new(false);
                for budget in [1, 2, 3, 4, 5, 6, 7, 30] {
                    for executor in [&mut light, &mut ordinary] {
                        executor.state = initial.clone();
                        executor.state.eflags |= [0, 2, 4, 6, 7, 10, 11]
                            .into_iter()
                            .enumerate()
                            .fold(0, |flags, (i, bit)| {
                                flags | ((budget as u32 >> i) & 1) << bit
                            });
                        executor.write(DATA, &data).unwrap();
                        executor.write(STACK, &stack).unwrap();
                    }
                    let a = light
                        .run_with_traps(stop, budget, &mut direct, false)
                        .unwrap();
                    let b = ordinary
                        .run_with_traps(stop, budget, &mut traps, false)
                        .unwrap();
                    assert_eq!(a, b);
                    assert_eq!(
                        light.state.clone().capture_architecture(),
                        ordinary.state.clone().capture_architecture()
                    );
                    assert_eq!(bytes(&light), bytes(&ordinary));
                }
                assert!(direct.direct > 0);
            }
        }
    }
}

// spec: A64-CTL-10, A64-CTL-11, A64-CTL-12
#[test]
fn patches_to_thunk_bytes_and_import_pointer_take_effect() {
    let (mut executor, stop) = fixture((false, false, Checks::Adaptive));
    let initial = executor.state.clone();
    let mut calls = Calls::new(true);
    executor
        .run_with_traps(stop, 30, &mut calls, false)
        .unwrap();
    assert_eq!(calls.direct, 1);
    executor
        .write(THUNK + 1, &(WORD + 1).to_le_bytes())
        .unwrap();
    executor.state = initial.clone();
    executor
        .run_with_traps(stop, 30, &mut calls, false)
        .unwrap();
    assert_eq!(executor.state.regs[0], WORD + 1);
    assert!(executor.stats.invalidations > 0);
    // Change the import target to a plain guest function.
    let mut code = Asm::new(CODE + 0x100);
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&42u32.to_le_bytes());
    code.ret_n(4);
    executor.write(CODE + 0x100, &code.bytes).unwrap();
    executor.write(DATA, &(CODE + 0x100).to_le_bytes()).unwrap();
    executor.state = initial;
    executor
        .run_with_traps(stop, 30, &mut calls, false)
        .unwrap();
    assert_eq!(executor.state.regs[0], 42);
    assert_eq!(calls.direct, 2);
}

// spec: A64-CTL-8, A64-CTL-12
#[test]
fn deadline_and_interrupt_leave_before_the_instruction_after_a_call() {
    for delayed in [false, true] {
        let (mut executor, stop) = fixture((false, false, Checks::Adaptive));
        let initial = executor.state.clone();
        let mut calls = Calls::new(true);
        executor
            .run_with_traps(stop, 30, &mut calls, false)
            .unwrap();
        executor.state = initial;
        executor.write(DATA + 40, &0u32.to_le_bytes()).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        executor.set_host_interrupt_flag(flag.clone());
        if delayed {
            calls.delay = true;
            executor.set_deadline(Some(Instant::now() + std::time::Duration::from_millis(5)));
        } else {
            calls.interrupt = Some(flag);
        }
        let result = executor
            .run_with_traps(stop, 30, &mut calls, false)
            .unwrap();
        assert_eq!(
            result,
            if delayed {
                RunOutcome::Budget
            } else {
                RunOutcome::Interrupted
            }
        );
        assert_eq!(executor.state.eip, THUNK + 7);
        assert_eq!(read_word(&executor.memory, DATA + 40).unwrap(), 0);
        assert_eq!(calls.direct, 2);
    }
}

// spec: A64F-FP-4, A64-CTL-12, A64-VER-1
#[test]
fn host_call_observes_dirty_mmx_and_x87_aliases_in_an_inlined_call() {
    use iced_x86::code_asm::*;
    struct Observer(Calls);
    impl ExecutionTrap for Observer {
        fn host_thunk(&self, pc: u32, word: u32) -> Option<u32> {
            self.0.host_thunk(pc, word)
        }
        fn host_call(
            &mut self,
            state: &mut CpuState,
            memory: &mut Memory,
            thunk: u32,
        ) -> Result<bool, String> {
            assert_eq!(state.mmx[3], 0x11111111);
            assert_eq!(
                state.x87.regs[3],
                adv_cpu_core::X87Register::from_parts(0x11111111, 0xffff)
            );
            // This is the canonical state consumed by context readers as well.
            let mut context = state.clone();
            let context = context.capture_architecture();
            assert_eq!(context.x87.regs[3], state.x87.regs[3]);
            self.0.host_call(state, memory, thunk)
        }
        fn call(
            &mut self,
            state: &mut CpuState,
            memory: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            self.0.call(state, memory, vector)
        }
    }
    for flagm in [false, true] {
        for first in [false, true] {
            for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
                let (mut native, _) = fixture((flagm, first, checks));
                let (mut oracle, _) = fixture((flagm, first, checks));
                let mut code = CodeAssembler::new(32).unwrap();
                code.movd(mm3, eax).unwrap();
                code.push(DATA + 32).unwrap();
                code.call(u64::from(THUNK)).unwrap();
                code.movq(qword_ptr((DATA + 64) as i32), mm3).unwrap();
                let code = code.assemble(u64::from(CODE)).unwrap();
                let stop = CODE + code.len() as u32;
                for executor in [&mut native, &mut oracle] {
                    executor.protect(CODE, 0x4000, READ | WRITE).unwrap();
                    executor.write(CODE, &code).unwrap();
                    executor.protect(CODE, 0x4000, READ | EXEC).unwrap();
                }
                oracle.interpreter_only = true;
                let mut observer = Observer(Calls::new(true));
                let mut ordinary = Calls::new(false);
                assert_eq!(
                    native
                        .run_with_traps(stop, 100, &mut observer, false)
                        .unwrap(),
                    oracle
                        .run_with_traps(stop, 100, &mut ordinary, false)
                        .unwrap()
                );
                assert_eq!(
                    native.state.clone().capture_architecture(),
                    oracle.state.clone().capture_architecture()
                );
                assert_eq!(bytes(&native), bytes(&oracle));
                if !first {
                    assert_eq!(observer.0.direct, 1);
                }
            }
        }
    }
}

#[test]
#[ignore = "10M instruction microbenchmark"]
fn syscall_microbenchmark() {
    struct Return;
    impl ExecutionTrap for Return {
        fn call(
            &mut self,
            _state: &mut CpuState,
            _memory: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            Ok(false)
        }
    }
    let (mut cpu, _) = fixture((false, false, Checks::Adaptive));
    let mut a = iced_x86::code_asm::CodeAssembler::new(32).unwrap();
    use iced_x86::code_asm::*;
    a.mov(ecx, 10_000_000u32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.int(0x2e).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    let bytes = a.assemble(u64::from(CODE)).unwrap();
    let stop = CODE + bytes.len() as u32;
    cpu.write(CODE, &bytes).unwrap();
    assert_eq!(
        cpu.run_with_traps(stop, 100, &mut Return, false).unwrap(),
        RunOutcome::Budget
    );
    cpu.state.eip = CODE;
    let helpers = cpu.native.as_ref().unwrap().diagnostics.helpers;
    let now = Instant::now();
    assert_eq!(
        cpu.run_with_traps(stop, 40_000_000, &mut Return, false)
            .unwrap(),
        RunOutcome::ReachedStop
    );
    println!(
        "W11 int ns/instruction={:.3} helpers={}",
        now.elapsed().as_nanos() as f64 / 10_000_000.0,
        cpu.native.as_ref().unwrap().diagnostics.helpers - helpers
    );
}

// spec: A64-TR-4, A64-CTL-12, A64-VER-1
#[test]
fn common_raw_syscalls_and_timestamp_reads_use_native_calls() {
    struct Clock {
        now: u64,
        calls: u32,
    }
    impl ExecutionTrap for Clock {
        fn guest_time_us(&mut self) -> Option<u64> {
            self.now += 1;
            Some(self.now)
        }
        fn call(
            &mut self,
            state: &mut CpuState,
            _: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            self.calls += 1;
            state.regs[3] = state.regs[0];
            Ok(false)
        }
    }
    for first in [false, true] {
        for checks in [Checks::Adaptive, Checks::All] {
            for budget in [1, 2, 3, 4, 100] {
                let make = |interpreted| {
                    let (mut cpu, _) = fixture((false, first, checks));
                    // RDTSC; INT 2e; RDTSC; ADC EDI,0; NOP.
                    cpu.write(
                        CODE,
                        &[0x0f, 0x31, 0xcd, 0x2e, 0x0f, 0x31, 0x83, 0xd7, 0, 0x90],
                    )
                    .unwrap();
                    cpu.interpreter_only = interpreted;
                    cpu
                };
                let (mut a, mut b) = (make(false), make(true));
                let (mut ca, mut cb) = (
                    Clock {
                        now: 4294966,
                        calls: 0,
                    },
                    Clock {
                        now: 4294966,
                        calls: 0,
                    },
                );
                let stop = CODE + 9;
                assert_eq!(
                    a.run_with_traps(stop, budget, &mut ca, false).unwrap(),
                    b.run_with_traps(stop, budget, &mut cb, false).unwrap()
                );
                assert_eq!(
                    a.state.clone().capture_architecture(),
                    b.state.clone().capture_architecture()
                );
                assert_eq!(
                    a.run_with_traps(stop, 100, &mut ca, false).unwrap(),
                    b.run_with_traps(stop, 100, &mut cb, false).unwrap()
                );
                assert_eq!(
                    a.state.clone().capture_architecture(),
                    b.state.clone().capture_architecture()
                );
                assert_eq!(ca.calls, cb.calls);
                if !first {
                    assert_eq!(a.native.as_ref().unwrap().diagnostics.helpers, 0);
                }
            }
        }
    }
}

// spec: A64-CTL-12, A64-VER-1
#[test]
fn common_prefixed_syscalls_continue_at_the_decoded_next_instruction() {
    struct Returns(u32);
    impl ExecutionTrap for Returns {
        fn call(
            &mut self,
            state: &mut CpuState,
            _: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            assert_eq!(state.eip, self.0);
            state.regs[0] = 0x1234;
            Ok(false)
        }
    }
    for prefix in [0x66u8, 0xf2, 0xf3, 0x64, 0x65] {
        for first in [false, true] {
            for budget in [1, 2, 100] {
                let make = |interpreted| {
                    let (mut cpu, _) = fixture((false, first, Checks::All));
                    cpu.write(CODE, &[prefix, 0xcd, 0x2e, 0x83, 0xc0, 1, 0x90])
                        .unwrap();
                    cpu.interpreter_only = interpreted;
                    cpu
                };
                let (mut a, mut b) = (make(false), make(true));
                let outcome = a
                    .run_with_traps(CODE + 6, budget, &mut Returns(CODE + 3), false)
                    .unwrap();
                assert_eq!(
                    outcome,
                    b.run_with_traps(CODE + 6, budget, &mut Returns(CODE + 3), false)
                        .unwrap()
                );
                assert_eq!(
                    a.state.clone().capture_architecture(),
                    b.state.clone().capture_architecture()
                );
                assert_eq!(
                    a.run_with_traps(CODE + 6, 100, &mut Returns(CODE + 3), false)
                        .unwrap(),
                    b.run_with_traps(CODE + 6, 100, &mut Returns(CODE + 3), false)
                        .unwrap()
                );
                assert_eq!(
                    a.state.clone().capture_architecture(),
                    b.state.clone().capture_architecture()
                );
                if !first {
                    assert_eq!(a.native.as_ref().unwrap().diagnostics.helpers, 0);
                }
            }
        }
    }
}

#[test]
#[ignore = "before/after counter and syscall helper census"]
fn common_counter_conversion_counts() {
    struct Counter;
    impl ExecutionTrap for Counter {
        fn guest_time_us(&mut self) -> Option<u64> {
            Some(1)
        }
        fn call(&mut self, _: &mut CpuState, _: &mut Memory, vector: u32) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            Ok(false)
        }
    }
    for (name, code) in [("rdtsc", [0x0f, 0x31]), ("int_2e", [0xcd, 0x2e])] {
        let (mut cpu, _) = fixture((false, false, Checks::Adaptive));
        cpu.write(CODE, &code).unwrap();
        cpu.count_tier0(true);
        assert_eq!(
            cpu.run_with_traps(CODE + 2, 100, &mut Counter, false)
                .unwrap(),
            RunOutcome::ReachedStop
        );
        let counts = cpu.retired_forms().unwrap();
        println!(
            "CENSUS {name} {} {} {} ReachedStop",
            counts[1], counts[2], counts[3]
        );
    }
}

// spec: A64-TR-4
#[test]
fn common_standalone_rdtsc_uses_the_owner_counter_without_a_trap_handler() {
    for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
        for first in [false, true] {
            let (mut cpu, _) = fixture((false, first, checks));
            cpu.state.regs[3] = DATA;
            let flags = cpu.state.eflags;
            // RDTSC; MOV [EBX],EAX; MOV [EBX+4],EDX; RDTSC; NOP.
            cpu.write(
                CODE,
                &[0x0f, 0x31, 0x89, 0x03, 0x89, 0x53, 4, 0x0f, 0x31, 0x90],
            )
            .unwrap();
            assert_eq!(cpu.run(CODE + 9, 100).unwrap(), RunOutcome::ReachedStop);
            let first_ticks =
                u64::from_le_bytes(cpu.memory.read(DATA, 8).unwrap().try_into().unwrap());
            let second_ticks = u64::from(cpu.state.regs[0]) | u64::from(cpu.state.regs[2]) << 32;
            assert!(second_ticks > first_ticks);
            assert_eq!(cpu.state.eflags, flags);
            if !first {
                assert_eq!(cpu.native.as_ref().unwrap().diagnostics.helpers, 0);
                assert_eq!(cpu.stats.interpreter_entries, 0);
            }
        }
    }
}

// spec: A64-CTL-12, A64-VER-1
#[test]
fn common_unhandled_syscalls_refund_the_int_and_preserve_its_fault_pc() {
    for prefix in [&[][..], &[0x66], &[0x65]] {
        let (mut cpu, _) = fixture((false, false, Checks::All));
        let mut code = prefix.to_vec();
        code.extend_from_slice(&[0xcd, 0x2e, 0x90]);
        cpu.write(CODE, &code).unwrap();
        assert_eq!(
            cpu.run(CODE + code.len() as u32 - 1, 100).unwrap(),
            RunOutcome::Trap {
                pc: CODE,
                vector: 0x2e
            }
        );
        assert_eq!(cpu.state.eip, CODE + code.len() as u32 - 1);
        assert_eq!(cpu.native.as_ref().unwrap().diagnostics.helpers, 0);
    }
}
