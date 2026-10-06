//! Guest function result reuse must be invisible: every program ends in the
//! same architectural state and memory as without it.
use adv_cpu::{
    CompileStrategy, EXEC, Executor, MemoOptions, MemoStats, READ, Region, RunOutcome, WRITE,
};
use iced_x86::code_asm::*;
use std::time::Duration;

const CODE: u32 = 0x10000;
const FUNC: u32 = 0x11000;
const SRC: u32 = 0x14000;
const DST: u32 = 0x15000;
const COUNTER: u32 = 0x15800;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f800;

fn code(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

/// Calls FUNC ten times; `before` emits the loop body ahead of each call.
fn driver(before: impl Fn(&mut CodeAssembler)) -> Vec<u8> {
    code(CODE, |a| {
        a.mov(dword_ptr(COUNTER), 10).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        before(a);
        a.call(u64::from(FUNC)).unwrap();
        a.dec(dword_ptr(COUNTER)).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    })
}

/// Sums 64 dwords from `esi` into `[DST]`, preserving esi.
fn sum(a: &mut CodeAssembler, source: impl Fn(&mut CodeAssembler)) {
    a.push(esi).unwrap();
    source(a);
    a.xor(eax, eax).unwrap();
    a.mov(ecx, 64).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.add(eax, dword_ptr(esi)).unwrap();
    a.add(esi, 4).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
}

fn run(main: &[u8], func: &[u8], memo: bool) -> (Executor, Option<MemoStats>) {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    let source: Vec<u8> = (0..1024u32).map(|i| (i * 7 + 3) as u8).collect();
    m.write(SRC, &source).unwrap();
    for (at, bytes) in [(CODE, main), (FUNC, func)] {
        m.write(at, bytes).unwrap();
        m.protect(at, 4096, READ | EXEC).unwrap();
        let mut region = Region::new(at, bytes.len());
        region.reachable = true;
        region.compile_strategy = CompileStrategy::Fast;
        m.add_region(region).unwrap();
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if memo {
        m.set_memo(Some(MemoOptions {
            sightings: 1,
            record_gap: Duration::ZERO,
            automatic: false,
        }));
        m.memo_hook(FUNC);
    }
    m.state.eip = CODE;
    m.state.eflags = 0x202;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    assert_eq!(m.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
    let stats = m.memo_stats();
    (m, stats)
}

/// Runs with and without reuse, requires identical results, returns the
/// reuse statistics.
fn differential(main: &[u8], func: &[u8]) -> MemoStats {
    let (plain, _) = run(main, func, false);
    let (memo, stats) = run(main, func, true);
    assert_eq!(memo.state.regs, plain.state.regs);
    assert_eq!(memo.state.eip, plain.state.eip);
    assert_eq!(memo.state.eflags, plain.state.eflags);
    assert_eq!(memo.state.xmm, plain.state.xmm);
    assert_eq!(memo.state.mmx, plain.state.mmx);
    assert_eq!(memo.state.x87, plain.state.x87);
    for (at, len) in [(DST, 0x1000), (SRC, 0x1000), (STACK - 0x800, 0x804)] {
        assert_eq!(
            memo.memory().read_data(at, len).unwrap(),
            plain.memory().read_data(at, len).unwrap(),
            "memory at {at:#x}"
        );
    }
    stats.unwrap()
}

fn pure_sum() -> Vec<u8> {
    code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    })
}

#[test]
fn repeated_pure_calls_reuse_one_recording() {
    let main = driver(|a| a.xor(eax, eax).unwrap());
    let stats = differential(&main, &pure_sum());
    assert_eq!(stats.recordings, 1);
    assert_eq!(stats.hits, 9);
}

#[test]
fn a_changed_input_byte_executes_again() {
    let main = driver(|a| {
        let mut skip = a.create_label();
        a.cmp(dword_ptr(COUNTER), 5).unwrap();
        a.jne(skip).unwrap();
        a.add(byte_ptr(SRC + 12), 7).unwrap();
        a.set_label(&mut skip).unwrap();
        a.xor(eax, eax).unwrap();
    });
    let stats = differential(&main, &pure_sum());
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.recordings, 2);
    assert_eq!(stats.hits, 8);
}

#[test]
fn a_destination_read_before_its_write_is_an_input() {
    let main = driver(|a| a.xor(eax, eax).unwrap());
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.add(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.hits, 0);
}

#[test]
fn stack_arguments_select_among_entries_of_one_state() {
    let main = code(CODE, |a| {
        a.mov(dword_ptr(COUNTER), 10).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(COUNTER)).unwrap();
        a.and(eax, 1).unwrap();
        a.shl(eax, 8).unwrap();
        a.add(eax, SRC).unwrap();
        a.push(eax).unwrap();
        a.xor(eax, eax).unwrap();
        a.call(u64::from(FUNC)).unwrap();
        a.add(esp, 4).unwrap();
        a.dec(dword_ptr(COUNTER)).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, dword_ptr(esp + 8)).unwrap());
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.recordings, 2);
    assert_eq!(stats.hits, 8);
}

#[test]
fn code_that_reads_the_clock_is_never_reused() {
    let main = driver(|a| a.xor(eax, eax).unwrap());
    let func = code(FUNC, |a| {
        a.rdtsc().unwrap();
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.xor(edx, edx).unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.aborts, 1);
}

#[test]
fn mmx_results_and_pending_state_are_restored() {
    let main = driver(|a| {
        a.xor(eax, eax).unwrap();
        a.pxor(mm1, mm1).unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.movq(mm0, qword_ptr(SRC)).unwrap();
        a.paddb(mm0, qword_ptr(SRC + 8)).unwrap();
        a.movd(mm1, eax).unwrap();
        a.paddd(mm0, mm1).unwrap();
        a.movq(qword_ptr(DST), mm0).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    // The first call still sees empty x87 tags; later calls see MMX in use.
    assert_eq!(stats.recordings, 2);
    assert_eq!(stats.hits, 8);
}

#[test]
fn rewritten_code_is_not_answered_from_old_recordings() {
    let main = driver(|a| a.xor(eax, eax).unwrap());
    let (mut m, stats) = run(&main, &pure_sum(), true);
    assert_eq!(stats.unwrap().hits, 9);
    let changed = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC + 4).unwrap());
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    m.protect(FUNC, 4096, READ | WRITE).unwrap();
    m.write(FUNC, &changed).unwrap();
    m.protect(FUNC, 4096, READ | EXEC).unwrap();
    let mut region = Region::new(FUNC, changed.len());
    region.reachable = true;
    m.add_region(region).unwrap();
    m.memo_hook(FUNC);
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    assert_eq!(m.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
    let (plain, _) = run(&main, &changed, false);
    assert_eq!(m.state.regs, plain.state.regs);
    assert_eq!(
        m.memory().read_data(DST, 4).unwrap(),
        plain.memory().read_data(DST, 4).unwrap()
    );
}

#[test]
fn x87_memory_operands_are_recorded_by_the_interpreter() {
    let main = driver(|a| a.xor(eax, eax).unwrap());
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.fld(dword_ptr(SRC + 16)).unwrap();
        a.fadd(dword_ptr(SRC + 20)).unwrap();
        a.fstp(qword_ptr(DST + 8)).unwrap();
        a.fild(dword_ptr(SRC + 24)).unwrap();
        a.fistp(dword_ptr(DST + 16)).unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.aborts, 0);
    assert_eq!(stats.recordings, 1);
    assert_eq!(stats.hits, 9);
}

#[test]
fn x87_inputs_select_a_new_recording() {
    let main = driver(|a| {
        let mut skip = a.create_label();
        a.cmp(dword_ptr(COUNTER), 4).unwrap();
        a.jne(skip).unwrap();
        a.add(byte_ptr(SRC + 517), 1).unwrap();
        a.set_label(&mut skip).unwrap();
        a.xor(eax, eax).unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.fld(dword_ptr(SRC + 516)).unwrap();
        a.fstp(dword_ptr(DST + 8)).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.recordings, 2);
    assert_eq!(stats.hits, 8);
}

/// A REP copy fully overwrites its destination, which is no input.
#[test]
fn rep_strings_record_each_element() {
    let main = driver(|a| {
        a.add(byte_ptr(DST + 0x100), 1).unwrap();
        a.xor(eax, eax).unwrap();
        a.xor(ecx, ecx).unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.push(edi).unwrap();
        a.mov(edi, DST + 0x100).unwrap();
        a.mov(ecx, 16).unwrap();
        a.rep().movsd().unwrap();
        a.mov(ecx, 0).unwrap();
        a.rep().movsd().unwrap();
        a.mov(ecx, 8).unwrap();
        a.rep().stosd().unwrap();
        a.mov(esi, SRC).unwrap();
        a.mov(edi, SRC + 4).unwrap();
        a.mov(ecx, 4).unwrap();
        a.repe().cmpsd().unwrap();
        a.pop(edi).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.aborts, 0);
    assert_eq!(stats.recordings, 1);
    assert_eq!(stats.hits, 9);
}

/// Callers leave different stale contents in empty x87 registers.
fn stale_x87(a: &mut CodeAssembler) {
    a.xor(eax, eax).unwrap();
    a.fild(dword_ptr(COUNTER)).unwrap();
    a.fild(dword_ptr(COUNTER)).unwrap();
    a.fmulp(st1, st0).unwrap();
    a.fstp(dword_ptr(DST + 0x200)).unwrap();
}

#[test]
fn stale_x87_registers_are_kept_unless_observed() {
    let main = code(CODE, |a| {
        a.mov(dword_ptr(COUNTER), 10).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        stale_x87(a);
        a.call(u64::from(FUNC)).unwrap();
        // The physical registers below TOP still hold the caller's values.
        a.movq(qword_ptr(DST + 0x300), mm7).unwrap();
        a.movq(qword_ptr(DST + 0x308), mm6).unwrap();
        a.emms().unwrap();
        a.mov(eax, dword_ptr(COUNTER)).unwrap();
        a.movq(qword_ptr(eax * 8 + (DST + 0x400)), mm7).unwrap();
        a.emms().unwrap();
        a.dec(dword_ptr(COUNTER)).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.fld(dword_ptr(SRC + 16)).unwrap();
        a.fstp(dword_ptr(DST + 8)).unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.recordings, 1);
    assert_eq!(stats.hits, 9);
}

#[test]
fn stale_x87_registers_read_through_mmx_are_inputs() {
    let main = driver(stale_x87);
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.movq(qword_ptr(DST + 8), mm6).unwrap();
        a.emms().unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.hits, 0);
}

#[test]
fn observed_x87_exception_flags_are_inputs() {
    let main = driver(|a| {
        let mut skip = a.create_label();
        a.test(dword_ptr(COUNTER), 1).unwrap();
        a.jz(skip).unwrap();
        a.fnclex().unwrap();
        a.set_label(&mut skip).unwrap();
        a.xor(eax, eax).unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.fstsw(word_ptr(DST + 8)).unwrap();
        a.fld(dword_ptr(SRC + 16)).unwrap();
        a.fdiv(dword_ptr(SRC + 20)).unwrap();
        a.fstp(dword_ptr(DST + 12)).unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    // The first status is clear; later ones alternate in the inexact flag.
    assert_eq!(stats.recordings, 3);
    assert_eq!(stats.hits, 7);
}

#[test]
fn unobserved_x87_exception_flags_accumulate() {
    let main = driver(|a| {
        let mut skip = a.create_label();
        a.cmp(dword_ptr(COUNTER), 3).unwrap();
        a.jne(skip).unwrap();
        a.fnclex().unwrap();
        a.fld1().unwrap();
        a.fldz().unwrap();
        a.fdivp(st1, st0).unwrap();
        a.fstp(st0).unwrap();
        a.set_label(&mut skip).unwrap();
        a.xor(eax, eax).unwrap();
    });
    let func = code(FUNC, |a| {
        sum(a, |a| a.mov(esi, SRC).unwrap());
        a.fld(dword_ptr(SRC + 16)).unwrap();
        a.fdiv(dword_ptr(SRC + 20)).unwrap();
        a.fstp(dword_ptr(DST + 12)).unwrap();
        a.mov(dword_ptr(DST), eax).unwrap();
        a.pop(esi).unwrap();
        a.ret().unwrap();
    });
    let stats = differential(&main, &func);
    assert_eq!(stats.recordings, 1);
    assert_eq!(stats.hits, 9);
}
