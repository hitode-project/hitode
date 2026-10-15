//! Authored thunk programs compare light calls with the ordinary INT path.
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

struct Calls {
    enabled: bool,
    leaves: u32,
    traps: u32,
    delay: bool,
    interrupt: Option<Arc<AtomicBool>>,
}
impl Calls {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            leaves: 0,
            traps: 0,
            delay: false,
            interrupt: None,
        }
    }
    fn result(&self, state: &mut CpuState, memory: &mut Memory, word: u32) -> u64 {
        state.regs[4] = state.regs[4].wrapping_add(4);
        let arg = read_word(memory, state.regs[4] + 4).unwrap();
        memory.write(arg, &word.to_le_bytes()).unwrap();
        u64::from(state.regs[2]) << 32 | u64::from(word)
    }
}
impl ExecutionTrap for Calls {
    fn leaf_call(&self, pc: u32, word: u32) -> bool {
        self.enabled && pc == THUNK + 5 && [WORD, WORD + 1].contains(&word)
    }
    fn leaf(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        pc: u32,
        word: u32,
    ) -> Option<u64> {
        if !self.leaf_call(pc, word) || read_word(memory, state.regs[4]).ok() != Some(word) {
            return None;
        }
        self.leaves += 1;
        if self.delay {
            let end = Instant::now() + std::time::Duration::from_millis(20);
            while Instant::now() < end {
                std::hint::spin_loop();
            }
        }
        if let Some(flag) = &self.interrupt {
            flag.store(true, Ordering::Release);
        }
        Some(self.result(state, memory, word))
    }
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String> {
        assert_eq!(vector, 0x2e);
        self.traps += 1;
        let word = read_word(memory, state.regs[4]).unwrap();
        let value = self.result(state, memory, word);
        state.regs[0] = value as u32;
        state.regs[2] = (value >> 32) as u32;
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
fn leaf_and_trap_match_at_instruction_budget_cuts() {
    for flagm in [false, true] {
        for first in [false, true] {
            for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
                let (mut light, stop) = fixture((flagm, first, checks));
                let (mut ordinary, _) = fixture((flagm, first, checks));
                let initial = light.state.clone();
                let data = light.memory.read(DATA, 0x4000).unwrap().to_vec();
                let stack = light.memory.read(STACK, 0x4000).unwrap().to_vec();
                let mut leaves = Calls::new(true);
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
                        .run_with_traps(stop, budget, &mut leaves, false)
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
                assert!(leaves.leaves > 0);
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
    assert_eq!(calls.leaves, 1);
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
    assert_eq!(calls.leaves, 2);
}

// spec: A64-CTL-8, A64-CTL-12
#[test]
fn deadline_and_interrupt_leave_before_the_instruction_after_leaf() {
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
        assert_eq!(calls.leaves, 2);
    }
}

// spec: A64-CTL-12
#[test]
fn cached_leaf_can_return_to_ordinary_dispatch() {
    let (mut executor, stop) = fixture((false, false, Checks::Adaptive));
    let initial = executor.state.clone();
    let mut calls = Calls::new(true);
    executor
        .run_with_traps(stop, 30, &mut calls, false)
        .unwrap();
    calls.enabled = false;
    executor.state = initial;
    executor
        .run_with_traps(stop, 30, &mut calls, false)
        .unwrap();
    assert_eq!((calls.leaves, calls.traps), (1, 1));
    assert_eq!(executor.state.regs[0], WORD);
}
