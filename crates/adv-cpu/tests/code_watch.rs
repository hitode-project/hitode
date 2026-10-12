//! Executable data stays writable until sampled code needs an SMC barrier.
// Tests x64 engine mechanisms; the a64 engine has its own.
#![cfg(not(target_arch = "aarch64"))]
use adv_cpu::{CompileStrategy, CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const PATCH: u32 = CODE + 0x100;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x3f000;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    build(&mut assembler);
    assembler.assemble(u64::from(at)).unwrap()
}

fn machine() -> Executor {
    let mut executor = Executor::new(CODE, 0x30000).unwrap();
    executor.protect(CODE, 0x30000, READ | WRITE).unwrap();
    executor.protect(CODE, 4096, READ | EXEC).unwrap();
    executor.protect(STOP, 4096, READ | EXEC).unwrap();
    executor.protect(DATA, 8192, READ | WRITE | EXEC).unwrap();
    executor.enable_auto_jit(false);
    executor
}

fn start(executor: &mut Executor, pc: u32) {
    executor.state = CpuState {
        eip: pc,
        ..CpuState::default()
    };
    executor.state.regs[4] = STACK;
    executor.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn compile(executor: &mut Executor, at: u32, code: &[u8]) {
    executor.write(at, code).unwrap();
    let mut region = Region::new(at, code.len());
    region.compile_strategy = CompileStrategy::Fast;
    executor.add_region(region).unwrap();
}

fn patcher(target: u32) -> Vec<u8> {
    assemble(PATCH, |assembler| {
        assembler.mov(dword_ptr(edi), ecx).unwrap();
        assembler.jmp(u64::from(target)).unwrap();
    })
}

#[test]
fn rwx_data_writes_stay_native_then_first_execution_and_code_patches_remain_coherent() {
    const WORDS: u32 = 256;
    const MASK: u32 = 0xa5c3_7e19;
    const TARGET: u32 = DATA + 0x800;
    let writer = assemble(CODE, |assembler| {
        let mut again = assembler.create_label();
        assembler.set_label(&mut again).unwrap();
        assembler.xor(dword_ptr(edi), eax).unwrap();
        assembler.add(edi, 4).unwrap();
        assembler.dec(ecx).unwrap();
        assembler.jnz(again).unwrap();
        assembler.ret().unwrap();
    });
    let target = assemble(TARGET, |assembler| {
        assembler.mov(eax, 17).unwrap();
        assembler.ret().unwrap();
    });
    let mut executor = machine();
    executor.write(TARGET, &target).unwrap();
    executor
        .write(
            DATA,
            &(0..WORDS).flat_map(u32::to_le_bytes).collect::<Vec<_>>(),
        )
        .unwrap();
    compile(&mut executor, CODE, &writer);
    compile(&mut executor, PATCH, &patcher(TARGET));
    start(&mut executor, CODE);
    executor.state.regs[0] = MASK;
    executor.state.regs[1] = WORDS;
    executor.state.regs[7] = DATA;
    assert_eq!(executor.run(STOP, 4096).unwrap(), RunOutcome::ReachedStop);
    let expected: Vec<_> = (0..WORDS)
        .flat_map(|word| (word ^ MASK).to_le_bytes())
        .collect();
    assert_eq!(
        executor.memory().read(DATA, expected.len()).unwrap(),
        expected
    );
    assert!(executor.stats.native_entries > 0);
    assert_eq!(executor.stats.interpreter_instructions, 0);

    // The data page still has guest EXEC: its first fetch succeeds without a
    // protection change. A later native copy must stop when that code changes.
    start(&mut executor, PATCH);
    executor.state.regs[1] = 17;
    executor.state.regs[7] = TARGET + 1;
    assert_eq!(executor.run(STOP, 64).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[0], 17);
    let mut region = Region::new(TARGET, target.len());
    region.compile_strategy = CompileStrategy::Fast;
    executor.add_region(region).unwrap();
    let invalidations = executor.stats.invalidations;
    start(&mut executor, PATCH);
    executor.state.regs[1] = 42;
    executor.state.regs[7] = TARGET + 1;
    assert_eq!(executor.run(STOP, 64).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[0], 42);
    assert_eq!(
        executor.memory().read(TARGET + 1, 4).unwrap(),
        42u32.to_le_bytes()
    );
    assert!(executor.stats.invalidations > invalidations);
}

#[test]
fn cross_page_instruction_tail_stays_watched_across_a_budget_return() {
    const TARGET: u32 = DATA + 4094;
    let target = assemble(TARGET, |assembler| {
        assembler.mov(eax, 17).unwrap();
        assembler.ret().unwrap();
    });
    let mut executor = machine();
    executor.write(TARGET, &target).unwrap();
    compile(&mut executor, PATCH, &patcher(TARGET));
    // Only the opcode starts inside the region. Its immediate lives on both
    // pages, so the completed instruction tail must retain both dependencies.
    let mut region = Region::new(TARGET, 1);
    region.complete_tail = true;
    region.compile_strategy = CompileStrategy::Fast;
    executor.add_region(region).unwrap();
    start(&mut executor, TARGET);
    assert_eq!(executor.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(executor.state.regs[0], 17);
    assert_eq!(executor.state.eip, TARGET + 5);
    assert_eq!(executor.run(STOP, 16).unwrap(), RunOutcome::ReachedStop);
    let invalidations = executor.stats.invalidations;

    start(&mut executor, PATCH);
    executor.state.regs[1] = 29;
    executor.state.regs[7] = TARGET + 1;
    assert_eq!(executor.run(STOP, 64).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(executor.state.regs[0], 29);
    assert_eq!(
        executor.memory().read(TARGET + 1, 4).unwrap(),
        29u32.to_le_bytes()
    );
    assert!(executor.stats.invalidations > invalidations);
}
