mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const STACK: u32 = 0x12f00;
const SRC: u32 = 0x20000;
const DST: u32 = 0x30000;
const END: u32 = 0x40000;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

/// A counted loop around `body`, entered once through a prologue.
fn counted(narrow: bool, body: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    code(|a| {
        let mut top = a.create_label();
        a.xor(eax, eax).unwrap();
        a.set_label(&mut top).unwrap();
        body(a);
        if narrow {
            a.dec(cx).unwrap();
        } else {
            a.dec(ecx).unwrap();
        }
        a.jne(top).unwrap();
        a.emms().unwrap();
        a.ret().unwrap();
    })
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, (END - CODE) as usize).unwrap();
    m.protect(CODE, (END - CODE) as usize, READ | WRITE)
        .unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    let pattern: Vec<u8> = (0..0x10000u32).map(|n| (n * 7 + 3) as u8).collect();
    m.write(SRC, &pattern).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m.state.eflags = 0x202;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}

/// Runs the JIT in budget slices and unicorn in one go; both must agree on
/// the outcome, registers, MMX state and every data byte.
fn differential(
    name: &str,
    bytes: &[u8],
    regs: [u32; 8],
    setup: impl Fn(&mut Executor),
    slice: u64,
) {
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(bytes, Some(strategy));
        let mut uc = machine(bytes, None);
        for m in [&mut jit, &mut uc] {
            m.state.regs = regs;
            m.state.regs[4] = STACK;
            setup(m);
        }
        let mut outcome = RunOutcome::Budget;
        for _ in 0..100_000 {
            outcome = jit.run(STOP, slice).unwrap();
            assert!(jit.state.fuel <= slice, "{name}: fuel overran its slice");
            if outcome != RunOutcome::Budget {
                break;
            }
        }
        let expected = uc.run_unicorn(STOP, 10_000_000).unwrap();
        let context = format!("{name} {strategy:?}");
        assert_eq!(outcome, expected, "{context}");
        assert_eq!(jit.state.eip, uc.state.eip, "{context}");
        assert_eq!(jit.state.regs, uc.state.regs, "{context}");
        assert_eq!(jit.state.mmx, uc.state.mmx, "{context}");
        // These bodies rewrite every arithmetic flag before reading one, so a
        // fault leaves them unspecified.
        let faulted = matches!(
            outcome,
            RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
        );
        if !faulted {
            assert_eq!(
                jit.state.eflags & 0x8d5,
                uc.state.eflags & 0x8d5,
                "{context}"
            );
        }
        for page in (SRC..END).step_by(4096) {
            let (Ok(left), Ok(right)) =
                (jit.memory().read(page, 4096), uc.memory().read(page, 4096))
            else {
                continue;
            };
            assert!(left == right, "{context}: page {page:#x} differs");
        }
    }
}

fn copy_dwords() -> Vec<u8> {
    counted(false, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi + 8), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
    })
}

fn regs(count: u32, src: u32, dst: u32) -> [u32; 8] {
    [0, count, 0, 0, STACK, 0, src, dst]
}

#[test]
fn versioned_copy_matches_unicorn_inside_and_across_page_permissions() {
    let bytes = copy_dwords();
    let all = |_: &mut Executor| {};
    differential("ordinary", &bytes, regs(3000, SRC, DST), all, 1 << 30);
    differential("one", &bytes, regs(1, SRC, DST), all, 1 << 30);
    differential("sliced", &bytes, regs(3000, SRC, DST), all, 37);
    // The destination reaches a read-only page midway.
    let readonly = |m: &mut Executor| m.protect(DST + 0x2000, 4096, READ).unwrap();
    differential("readonly", &bytes, regs(3000, SRC, DST), readonly, 1 << 30);
    // Exactly the last iteration's store touches the read-only page.
    differential("last", &bytes, regs(2047, SRC, DST), readonly, 1 << 30);
    differential("fits", &bytes, regs(2046, SRC, DST), readonly, 1 << 30);
    // The source reaches an inaccessible page; the destination an executable one.
    let inaccessible = |m: &mut Executor| m.protect(SRC + 0x1000, 4096, 0).unwrap();
    differential(
        "inaccessible",
        &bytes,
        regs(3000, SRC, DST),
        inaccessible,
        1 << 30,
    );
    let executable = |m: &mut Executor| m.protect(DST + 0x1000, 4096, READ | WRITE | EXEC).unwrap();
    differential(
        "executable",
        &bytes,
        regs(3000, SRC, DST),
        executable,
        1 << 30,
    );
    // Pointers leaving the top of the window.
    differential("window", &bytes, regs(3000, END - 0x800, DST), all, 1 << 30);
}

#[test]
fn versioned_loops_with_negative_steps_narrow_counters_and_skips_match_unicorn() {
    let backwards = counted(false, |a| {
        a.movd(mm0, dword_ptr(esi - 4)).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.sub(esi, 4).unwrap();
        a.lea(edi, dword_ptr(edi - 4)).unwrap();
    });
    let readonly = |m: &mut Executor| m.protect(DST + 0x1000, 4096, READ).unwrap();
    differential(
        "backwards",
        &backwards,
        regs(2000, SRC + 0x2000, DST + 0x3000),
        |_| {},
        1 << 30,
    );
    differential(
        "backwards-fault",
        &backwards,
        regs(3000, SRC + 0x3000, DST + 0x3000),
        readonly,
        1 << 30,
    );
    differential(
        "backwards-window",
        &backwards,
        regs(3000, SRC + 0x100, DST + 0x3000),
        |_| {},
        1 << 30,
    );
    // CX counts; ECX's high half must not widen or shrink the proven range.
    let narrow = counted(true, |a| {
        a.mov(al, byte_ptr(esi + ebx * 2)).unwrap();
        a.add(byte_ptr(edi), al).unwrap();
        a.inc(edi).unwrap();
        a.inc(ebx).unwrap();
    });
    let mut r = regs(0x7fff_0900, SRC, DST);
    r[3] = 16;
    differential("narrow", &narrow, r, |_| {}, 1 << 30);
    differential(
        "narrow-fault",
        &narrow,
        r,
        |m: &mut Executor| m.protect(DST, 4096, READ).unwrap(),
        1 << 30,
    );
    r[1] = 0x0001_0000 | 0x7000;
    differential("narrow-wide", &narrow, r, |_| {}, 1 << 30);
    // A blend-shaped body: a skipped store, MMX state carried across the latch.
    let skip = counted(false, |a| {
        let mut skip = a.create_label();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.and(eax, 0x0100_0000).unwrap();
        a.je(skip).unwrap();
        a.movd(mm1, dword_ptr(esi)).unwrap();
        a.paddusb(mm2, mm1).unwrap();
        a.movd(dword_ptr(edi), mm2).unwrap();
        a.mov(byte_ptr(edi + 3), 0xff).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(edi, 4).unwrap();
        a.add(esi, 4).unwrap();
    });
    differential("skip", &skip, regs(3000, SRC, DST), |_| {}, 1 << 30);
    differential("skip-sliced", &skip, regs(3000, SRC, DST), |_| {}, 101);
    differential(
        "skip-fault",
        &skip,
        regs(3000, SRC, DST),
        |m: &mut Executor| m.protect(DST + 0x2000, 4096, READ).unwrap(),
        1 << 30,
    );
}

#[test]
fn versioned_invariant_loop_interrupts_and_resumes_exactly() {
    let bytes = counted(false, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(dword_ptr(edi), eax).unwrap();
        a.inc(ebx).unwrap();
    });
    let mut m = machine(&bytes, Some(CompileStrategy::Speed));
    m.state.regs = regs(100_000_000, SRC, DST);
    m.write(SRC, &1u32.to_le_bytes()).unwrap();
    m.write(DST, &0u32.to_le_bytes()).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    m.set_host_interrupt_flag(flag.clone());
    let notify = flag.clone();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        notify.store(true, Ordering::Release);
    });
    let elapsed = Instant::now();
    let outcome = m.run(STOP, 1 << 40).unwrap();
    producer.join().unwrap();
    assert_eq!(outcome, RunOutcome::Interrupted);
    assert!(elapsed.elapsed() < Duration::from_secs(2));
    let completed = m.state.regs[3];
    assert!(completed > 0 && completed < 100_000_000);
    assert_eq!(m.state.regs[1], 100_000_000 - completed);
    let sum = u32::from_le_bytes(m.memory().read(DST, 4).unwrap().try_into().unwrap());
    assert_eq!(sum, completed);
    assert_eq!(m.stats.interpreter_instructions, 0);
    flag.store(false, Ordering::Release);
    m.state.regs[1] = 10;
    assert_eq!(m.run(STOP, 1 << 20).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], completed + 10);
    let sum = u32::from_le_bytes(m.memory().read(DST, 4).unwrap().try_into().unwrap());
    assert_eq!(sum, completed + 10);
}

#[test]
fn forwarded_stack_slots_match_unicorn_and_respect_aliasing_writes() {
    // Locals at [ebp+8] and [ebp+12], read and written every iteration.
    let locals = counted(false, |a| {
        a.mov(eax, dword_ptr(ebp + 8)).unwrap();
        a.add(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(ebp + 8), eax).unwrap();
        a.add(dword_ptr(ebp + 12), eax).unwrap();
        a.mov(ebx, dword_ptr(ebp + 12)).unwrap();
        a.mov(dword_ptr(edi), ebx).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
    });
    let frame = |m: &mut Executor| m.write(DST + 0x3000, &[0x11; 32]).unwrap();
    let mut r = regs(1000, SRC, DST);
    r[5] = DST + 0x3000;
    differential("slots", &locals, r, frame, 1 << 30);
    differential("slots-sliced", &locals, r, frame, 23);
    // The destination sweeps across the frame: forwarding must not hide it.
    r[1] = 0xc80;
    differential("slots-aliased", &locals, r, frame, 1 << 30);
    differential("slots-aliased-sliced", &locals, r, frame, 41);
    // The frame turns read-only: its stores fault.
    let readonly = |m: &mut Executor| {
        frame(m);
        m.protect(DST + 0x3000, 4096, READ).unwrap();
    };
    r[1] = 1000;
    differential("slots-readonly", &locals, r, readonly, 1 << 30);
    // A narrow store into a slot and an indexed store keep both unforwarded.
    let mixed = counted(false, |a| {
        a.mov(eax, dword_ptr(ebp + 8)).unwrap();
        a.add(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(ebp + 8), eax).unwrap();
        a.mov(byte_ptr(ebp + 9), cl).unwrap();
        a.and(eax, 0xfc).unwrap();
        a.mov(dword_ptr(ebp + eax), ecx).unwrap();
        a.add(dword_ptr(ebp + 12), eax).unwrap();
        a.add(esi, 4).unwrap();
    });
    differential("slots-mixed", &mixed, r, frame, 1 << 30);
}

#[test]
fn windowed_gathers_and_scatters_match_unicorn() {
    // Offsets loaded from the source address a table at EBX: each access
    // depends on data, so the clone checks it against a validated window.
    let gather = counted(false, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.and(eax, 0x3ffc).unwrap();
        a.mov(edx, dword_ptr(ebx + eax)).unwrap();
        a.mov(dword_ptr(edi), edx).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
    });
    let mut r = regs(3000, SRC, DST);
    r[3] = SRC + 0x8000;
    let all = |_: &mut Executor| {};
    differential("gather", &gather, r, all, 1 << 30);
    differential("gather-sliced", &gather, r, all, 29);
    let hole = |m: &mut Executor| m.protect(SRC + 0x9000, 4096, 0).unwrap();
    differential("gather-fault", &gather, r, hole, 1 << 30);
    // Unaligned table reads straddle pages.
    r[3] = SRC + 0x8002;
    differential("gather-straddle", &gather, r, all, 1 << 30);
    differential("gather-straddle-fault", &gather, r, hole, 1 << 30);
    // The same addressing as a store, into read-only and executable pages.
    let scatter = counted(false, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.and(eax, 0x3ffc).unwrap();
        a.mov(dword_ptr(ebx + eax), ecx).unwrap();
        a.add(esi, 4).unwrap();
    });
    r[3] = DST;
    differential("scatter", &scatter, r, all, 1 << 30);
    differential("scatter-sliced", &scatter, r, all, 31);
    let readonly = |m: &mut Executor| m.protect(DST + 0x2000, 4096, READ).unwrap();
    differential("scatter-readonly", &scatter, r, readonly, 1 << 30);
    let executable = |m: &mut Executor| m.protect(DST + 0x1000, 4096, READ | WRITE | EXEC).unwrap();
    differential("scatter-executable", &scatter, r, executable, 1 << 30);
}

#[test]
fn loops_leaving_through_a_side_exit_resume_after_it() {
    // Copies until a marker word, then finishes outside the loop.
    let bytes = code(|a| {
        let mut top = a.create_label();
        let mut found = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.cmp(eax, 0x5555_5555).unwrap();
        a.je(found).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.set_label(&mut found).unwrap();
        a.lea(ebx, dword_ptr(ecx + 7)).unwrap();
        a.ret().unwrap();
    });
    for at in [0u32, 1, 100, 2999] {
        let marker = move |m: &mut Executor| {
            m.write(SRC + at * 4, &0x5555_5555u32.to_le_bytes())
                .unwrap()
        };
        differential("side", &bytes, regs(3000, SRC, DST), marker, 1 << 30);
        differential("side-sliced", &bytes, regs(3000, SRC, DST), marker, 43);
    }
}
