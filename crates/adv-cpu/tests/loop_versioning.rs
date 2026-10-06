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
    differential_with_oracle_slice(name, bytes, regs, setup, slice, 10_000_000);
}

/// A one-instruction oracle slice canonicalizes flags before a faulting
/// memory CMP: Unicorn otherwise loses preceding ADD's lazy carry/auxiliary flags.
fn differential_with_oracle_slice(
    name: &str,
    bytes: &[u8],
    regs: [u32; 8],
    setup: impl Fn(&mut Executor),
    slice: u64,
    oracle_slice: usize,
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
        let mut expected = RunOutcome::Budget;
        let mut oracle_left = 10_000_000;
        while expected == RunOutcome::Budget && oracle_left != 0 {
            let budget = oracle_slice.min(oracle_left);
            expected = uc.run_unicorn(STOP, budget).unwrap();
            oracle_left -= budget;
        }
        let context = format!("{name} {strategy:?}");
        assert_eq!(outcome, expected, "{context}");
        assert_eq!(jit.state.eip, uc.state.eip, "{context}");
        assert_eq!(jit.state.regs, uc.state.regs, "{context}");
        assert_eq!(jit.state.mmx, uc.state.mmx, "{context}");
        // Fault and scheduler contexts observe the prefix's flags even when
        // the following guest instructions would replace them.
        assert_eq!(
            jit.state.eflags & 0x8d5,
            uc.state.eflags & 0x8d5,
            "{context}"
        );
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
fn wrapping_indexed_writes_cannot_hide_aliases_with_forwarded_slots() {
    let bytes = counted(false, |a| {
        a.mov(eax, dword_ptr(ebp + 8)).unwrap();
        a.inc(eax).unwrap();
        a.mov(dword_ptr(ebp + 8), eax).unwrap();
        a.mov(dword_ptr(esi + ebx * 4), 100).unwrap();
        a.mov(edi, dword_ptr(ebp + 8)).unwrap();
    });
    // Both effective addresses are DST+8. One wraps the base addition; the
    // other wraps the scaled index. An I64 alias proof would put each above
    // 4 GiB and incorrectly forward the earlier store instead of reading 100.
    for (base, index) in [(0xffff_ffe8, 0xc008), (DST, 0x4000_0002)] {
        let mut r = regs(3, base, 0);
        r[3] = index;
        r[5] = DST;
        differential(
            "wrapping-slot-alias",
            &bytes,
            r,
            |m| m.write(DST + 8, &1u32.to_le_bytes()).unwrap(),
            1 << 30,
        );
    }
}

#[test]
fn rewriting_a_narrow_counter_keeps_memory_checks_on_every_iteration() {
    let bytes = counted(true, |a| {
        a.mov(cx, 4).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut uc = machine(&bytes, None);
        for m in [&mut jit, &mut uc] {
            m.state.regs = regs(4, SRC, DST + 4096 - 20);
            m.write(DST + 4096, &[0xa5; 32]).unwrap();
            m.protect(DST + 4096, 4096, READ).unwrap();
        }
        // The first latch's CX=3 proves only three following stores. The
        // body resets CX, so the sixth store must still check the next page.
        // Keep fuel small even on a broken compiler: the unchecked loop must
        // not run far enough to leave this fixture's allocated window.
        let expected = uc.run_unicorn(STOP, 1000).unwrap();
        assert!(matches!(
            expected,
            RunOutcome::WriteFault { address, .. } if address == DST + 4096
        ));
        assert_eq!(jit.run(STOP, 16).unwrap(), expected, "{strategy:?}");
        assert_eq!(jit.state.eip, uc.state.eip);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.eflags, uc.state.eflags);
        assert_eq!(jit.memory().read(DST + 4096, 32).unwrap(), [0xa5; 32]);
    }
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

/// A loop around `body` that `close` steps and tests.
fn closed(
    body: impl FnOnce(&mut CodeAssembler),
    close: impl FnOnce(&mut CodeAssembler, CodeLabel),
) -> Vec<u8> {
    code(|a| {
        let mut top = a.create_label();
        a.pxor(mm0, mm0).unwrap();
        a.mov(eax, 0x0060_00a0).unwrap();
        a.movd(mm5, eax).unwrap();
        a.punpckldq(mm5, mm5).unwrap();
        a.set_label(&mut top).unwrap();
        body(a);
        close(a, top);
        a.emms().unwrap();
        a.ret().unwrap();
    })
}

#[test]
fn stores_between_the_counter_step_and_the_latch_match_unicorn() {
    let all = |_: &mut Executor| {};
    // Batched bodies whose decrement precedes a store: of a lane value, of
    // the counter itself, and to a slot the next iteration reads.
    let lane = counted_store(|a| a.mov(dword_ptr(edi - 4), eax).unwrap());
    let counter = counted_store(|a| a.mov(dword_ptr(edi - 4), ecx).unwrap());
    let slot = counted_store(|a| a.mov(dword_ptr(ebp), eax).unwrap());
    for (name, bytes) in [("lane", &lane), ("counter", &counter), ("slot", &slot)] {
        for count in [1u32, 2, 3, 4, 5, 3000] {
            let mut r = regs(count, SRC, DST);
            r[5] = END - 0x100;
            differential(name, bytes, r, all, 1 << 30);
            differential(name, bytes, r, all, 37);
        }
    }
}

#[test]
fn interleaved_pointer_steps_and_a_store_after_the_decrement_match_unicorn() {
    // A delayed recurrence: update the address between loads, then decrement
    // a separate counter before storing a result that later iterations read.
    // The two-lane batch is legal, since its reads and writes stay apart.
    // The latch reads the first lane's flags, so a batch is never exact,
    // even when the code after the loop overwrites every flag.
    for (displacement, clobber) in [(4, false), (12, false), (4, true)] {
        let bytes = code(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(edx, dword_ptr(eax - 8)).unwrap();
            a.mov(ebx, dword_ptr(eax)).unwrap();
            a.add(eax, 4).unwrap();
            a.xor(edx, ebx).unwrap();
            a.mov(ebx, dword_ptr(eax - 28)).unwrap();
            a.shl(ebx, 1).unwrap();
            a.add(edx, ebx).unwrap();
            a.dec(esi).unwrap();
            a.mov(dword_ptr(eax + displacement), edx).unwrap();
            a.jne(top).unwrap();
            if clobber {
                a.xor(ecx, ecx).unwrap();
            }
            a.ret().unwrap();
        });
        for count in [1, 2, 3, 4, 5, 8, 9, 3000] {
            let mut r = regs(0, count, 0);
            r[0] = SRC + 0x100;
            differential("interleaved-recurrence", &bytes, r, |_| {}, 1 << 30);
            differential("interleaved-recurrence-sliced", &bytes, r, |_| {}, 37);
        }
        let mut r = regs(0, 3000, 0);
        r[0] = SRC + 0x100;
        differential(
            "interleaved-recurrence-read-fault",
            &bytes,
            r,
            |m| m.protect(SRC + 0x2000, 4096, 0).unwrap(),
            1 << 30,
        );
        differential(
            "interleaved-recurrence-store-fault",
            &bytes,
            r,
            |m| m.protect(SRC + 0x2000, 4096, READ).unwrap(),
            41,
        );
    }
}

#[test]
fn large_counter_steps_do_not_wrap_the_batch_funding_threshold() {
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        // An invariant byte store keeps the range proof valid for this very
        // large stride, and avoids slot forwarding or a compact copy batch.
        a.mov(byte_ptr(edi), cl).unwrap();
        a.inc(edx).unwrap();
        a.sub(ecx, i32::MIN).unwrap();
        a.cmp(ecx, 0).unwrap();
        a.jne(top).unwrap();
        a.ret().unwrap();
    });
    let initial = regs(0, SRC, DST);
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut uc = machine(&bytes, None);
        for m in [&mut jit, &mut uc] {
            m.state.regs = initial;
        }
        // Finish compilation before starting the interrupt that bounds a
        // broken batch: two steps wrap the counter back to its initial value.
        assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
        jit.state.regs = initial;
        jit.state.eip = CODE;
        jit.state.eflags = 0x202;
        let flag = Arc::new(AtomicBool::new(false));
        jit.set_host_interrupt_flag(flag.clone());
        let notify = flag.clone();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            notify.store(true, Ordering::Release);
        });
        let result = jit.run(STOP, 1 << 30).unwrap();
        producer.join().unwrap();
        assert_eq!(result, RunOutcome::ReachedStop, "{strategy:?}");
        assert_eq!(uc.run_unicorn(STOP, 100).unwrap(), result);
        assert_eq!(jit.state.eip, uc.state.eip);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.regs[2], 2);
        assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
        assert_eq!(jit.memory().read(DST, 1), uc.memory().read(DST, 1));
    }
}

#[test]
fn invariant_memory_bounds_and_reloaded_bounds_match_unicorn() {
    const BOUND: u32 = END - 0x104;
    for reloaded in [false, true] {
        for (first, bound, stride, inclusive) in [
            (0, 3000, 1, false),
            (-7i32, 2990, 2, true),
            (3000, 4, -1, false),
            (2999, 7, -2, true),
        ] {
            let bytes = closed(
                |a| {
                    a.mov(eax, dword_ptr(esi + ebx * 4)).unwrap();
                    a.mov(dword_ptr(edi + ebx * 4), eax).unwrap();
                    if reloaded {
                        // The full bound load replaces an earlier temporary
                        // partial value in this register on every iteration.
                        a.mov(dx, word_ptr(esi + ebx * 4)).unwrap();
                    }
                },
                |a, top| {
                    a.add(ebx, stride).unwrap();
                    if reloaded {
                        a.mov(edx, dword_ptr(ebp + ecx * 4 - 8)).unwrap();
                        a.cmp(ebx, edx).unwrap();
                    } else {
                        a.cmp(ebx, dword_ptr(ebp + ecx * 4 - 8)).unwrap();
                    }
                    match (stride > 0, inclusive) {
                        (true, false) => a.jl(top).unwrap(),
                        (true, true) => a.jle(top).unwrap(),
                        (false, false) => a.jg(top).unwrap(),
                        (false, true) => a.jge(top).unwrap(),
                    }
                },
            );
            let mut r = regs(2, SRC + 0x100, DST + 0x100);
            r[3] = first as u32;
            r[5] = BOUND;
            let setup = |m: &mut Executor| m.write(BOUND, &(bound as u32).to_le_bytes()).unwrap();
            let context = format!(
                "memory-bound first={first} bound={bound} stride={stride} inclusive={inclusive} reloaded={reloaded}"
            );
            differential(&context, &bytes, r, setup, 1 << 30);
            differential(&format!("{context}-sliced"), &bytes, r, setup, 29);
            differential_with_oracle_slice(
                &format!("{context}-unreadable"),
                &bytes,
                r,
                |m| {
                    setup(m);
                    m.protect(END - 4096, 4096, 0).unwrap();
                },
                1 << 30,
                1,
            );
        }
    }
}

#[test]
fn writes_aliasing_a_memory_bound_keep_the_comparison_current() {
    // The destination eventually overwrites the bound through another base.
    // The new value ends the loop at 130 rather than the original 3000.
    for reloaded in [false, true] {
        let bytes = code(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi + ebx * 4)).unwrap();
            a.mov(dword_ptr(edi + ebx * 4), eax).unwrap();
            a.inc(ebx).unwrap();
            if reloaded {
                a.mov(edx, dword_ptr(ebp)).unwrap();
                a.cmp(ebx, edx).unwrap();
            } else {
                a.cmp(ebx, dword_ptr(ebp)).unwrap();
            }
            a.jb(top).unwrap();
            a.ret().unwrap();
        });
        let mut r = regs(0, SRC, DST);
        r[5] = DST + 128 * 4;
        let setup = |m: &mut Executor| {
            m.write(DST + 128 * 4, &3000u32.to_le_bytes()).unwrap();
            m.write(SRC + 128 * 4, &130u32.to_le_bytes()).unwrap();
        };
        differential("bound-alias", &bytes, r, setup, 1 << 30);
        differential("bound-alias-sliced", &bytes, r, setup, 17);
    }
    // A wrapping scaled address aliases the bound's invariant slot.
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(dword_ptr(esi + edx * 4), 7).unwrap();
        a.inc(ebx).unwrap();
        a.cmp(ebx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
    });
    for (base, index) in [(0xffff_ffe8, 0xc008), (DST, 0x4000_0002)] {
        let mut r = regs(0, base, 0);
        r[2] = index;
        r[5] = DST + 8;
        differential(
            "bound-wrapping-alias",
            &bytes,
            r,
            |m| m.write(DST + 8, &1000u32.to_le_bytes()).unwrap(),
            1 << 30,
        );
    }
    // The doubleword mutation is a forwarded slot. Its write is absent from
    // Plan::writes, but must still take part in the bound alias check.
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(dword_ptr(ebp), 1).unwrap();
        a.add(ebx, 2).unwrap();
        a.cmp(ebx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
    });
    let mut r = regs(0, SRC, DST);
    r[5] = DST;
    differential(
        "bound-forwarded-slot-alias",
        &bytes,
        r,
        |m| m.write(DST, &3u32.to_le_bytes()).unwrap(),
        13,
    );
    // A mixed-width overwrite prevents slot forwarding; both writes still
    // need to participate in the invariant-bound alias check.
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(dword_ptr(ebp), 15).unwrap();
        a.mov(byte_ptr(ebp), 7).unwrap();
        a.inc(ebx).unwrap();
        a.cmp(ebx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
    });
    let mut r = regs(0, SRC, DST);
    r[5] = DST;
    differential("bound-slot-alias", &bytes, r, |_| {}, 13);
}

#[test]
fn memory_bound_permissions_changed_between_slices_preserve_the_fault_prefix() {
    const BOUND: u32 = END - 0x104;
    for reloaded in [false, true] {
        let bytes = code(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi + ebx * 4)).unwrap();
            a.mov(dword_ptr(edi + ebx * 4), eax).unwrap();
            a.inc(ebx).unwrap();
            if reloaded {
                a.mov(edx, dword_ptr(ebp)).unwrap();
                a.cmp(ebx, edx).unwrap();
            } else {
                a.cmp(ebx, dword_ptr(ebp)).unwrap();
            }
            a.jb(top).unwrap();
            a.ret().unwrap();
        });
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut uc = machine(&bytes, None);
            for m in [&mut jit, &mut uc] {
                m.state.regs = regs(0, SRC, DST);
                m.state.regs[5] = BOUND;
                m.write(BOUND, &3000u32.to_le_bytes()).unwrap();
            }
            assert_eq!(jit.run(STOP, 37).unwrap(), RunOutcome::Budget);
            let completed = jit.state.regs[3];
            assert!(completed > 0 && completed < 3000);
            let cost = if reloaded { 6 } else { 5 };
            assert_eq!(
                uc.run_unicorn(STOP, cost * completed as usize).unwrap(),
                RunOutcome::Budget
            );
            assert_eq!(jit.state.eip, uc.state.eip);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
            for m in [&mut jit, &mut uc] {
                m.protect(END - 4096, 4096, 0).unwrap();
            }
            let mut expected = RunOutcome::Budget;
            for _ in 0..100 {
                expected = uc.run_unicorn(STOP, 1).unwrap();
                if expected != RunOutcome::Budget {
                    break;
                }
            }
            assert!(matches!(expected, RunOutcome::ReadFault { address, .. } if address == BOUND));
            assert_eq!(jit.run(STOP, 100).unwrap(), expected);
            assert_eq!(jit.state.eip, uc.state.eip);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
            assert_eq!(jit.memory().read(DST, 4096), uc.memory().read(DST, 4096));
        }
    }
}

#[test]
fn memory_bounds_crossing_page_permissions_match_unicorn() {
    const BOUND: u32 = END - 4098;
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(byte_ptr(edi), cl).unwrap();
        a.inc(ecx).unwrap();
        a.cmp(ecx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
    });
    let mut r = regs(0, SRC, DST);
    r[5] = BOUND;
    differential(
        "bound-two-pages",
        &bytes,
        r,
        |m| m.write(BOUND, &3000u32.to_le_bytes()).unwrap(),
        37,
    );
    // Unicorn allows this straddling read despite the second page's
    // protection. Use its exact pre-CMP prefix as the fault-state oracle.
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut uc = machine(&bytes, None);
        for m in [&mut jit, &mut uc] {
            m.state.regs = r;
            m.write(BOUND, &3000u32.to_le_bytes()).unwrap();
            m.write(DST, &[0xa5]).unwrap();
            m.protect(END - 4096, 4096, 0).unwrap();
        }
        assert_eq!(uc.run_unicorn(STOP, 2).unwrap(), RunOutcome::Budget);
        assert_eq!(
            jit.run(STOP, 100).unwrap(),
            RunOutcome::ReadFault {
                pc: uc.state.eip,
                address: BOUND,
            }
        );
        assert_eq!(jit.state.eip, uc.state.eip);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
        assert_eq!(jit.memory().read(DST, 1).unwrap(), [0]);
        assert_eq!(jit.memory().read(DST, 1), uc.memory().read(DST, 1));
    }
}

#[test]
fn absolute_memory_bounds_in_the_high_address_half_match_unicorn() {
    const HIGH_CODE: u32 = 0x9001_0000;
    const HIGH_STOP: u32 = HIGH_CODE + 0x1000;
    const HIGH_STACK: u32 = HIGH_CODE + 0x2f00;
    const HIGH_DATA: u32 = HIGH_CODE + 0x20000;
    const HIGH_BOUND: u32 = HIGH_CODE + 0x30000;
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(byte_ptr(edi), cl).unwrap();
    a.inc(ecx).unwrap();
    a.cmp(ecx, dword_ptr(HIGH_BOUND as i32)).unwrap();
    a.jb(top).unwrap();
    a.ret().unwrap();
    let bytes = a.assemble(u64::from(HIGH_CODE)).unwrap();
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = Executor::new(HIGH_CODE, 0x40000).unwrap();
        let mut uc = Executor::new(HIGH_CODE, 0x40000).unwrap();
        for m in [&mut jit, &mut uc] {
            m.protect(HIGH_CODE, 0x40000, READ | WRITE).unwrap();
            m.write(HIGH_CODE, &bytes).unwrap();
            m.protect(HIGH_CODE, 4096, READ | EXEC).unwrap();
            m.protect(HIGH_STOP, 4096, READ | EXEC).unwrap();
            m.write(HIGH_STACK, &HIGH_STOP.to_le_bytes()).unwrap();
            m.write(HIGH_BOUND, &3000u32.to_le_bytes()).unwrap();
            m.state.regs = [0, 0, 0, 0, HIGH_STACK, 0, 0, HIGH_DATA];
            m.state.eip = HIGH_CODE;
            m.state.eflags = 0x202;
        }
        let mut region = Region::new(HIGH_CODE, bytes.len());
        region.compile_strategy = strategy;
        jit.add_region(region).unwrap();
        let expected = uc.run_unicorn(HIGH_STOP, 100_000).unwrap();
        assert_eq!(jit.run(HIGH_STOP, 100_000).unwrap(), expected);
        assert_eq!(expected, RunOutcome::ReachedStop);
        assert_eq!(jit.state.eip, uc.state.eip);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
        assert_eq!(
            jit.memory().read(HIGH_DATA, 1),
            uc.memory().read(HIGH_DATA, 1)
        );
    }
}

#[cfg(windows)]
#[test]
fn shared_memory_bounds_use_checked_reads_and_keep_exact_state() {
    use adv_cpu::backend::{Guest, Prot};
    const BOUND: u32 = END - 4096;
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(byte_ptr(edi), bl).unwrap();
        a.inc(ebx).unwrap();
        a.cmp(ebx, dword_ptr(ebp)).unwrap();
        a.jb(top).unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut cpu = adv_cpu::JitCpu::new(STOP, false).unwrap();
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(STOP, 4096, Prot::RX).unwrap();
        cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
        cpu.map(DST, 4096, Prot::RW).unwrap();
        let shared = cpu.map_shared_audio(BOUND, 4096).unwrap();
        cpu.write(CODE, &bytes).unwrap();
        cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
        cpu.write(BOUND, &3000u32.to_le_bytes()).unwrap();
        let mut initial = regs(0, 0, DST);
        initial[5] = BOUND;
        let jit = cpu.executor_mut();
        jit.state.regs = initial;
        jit.state.eip = CODE;
        jit.state.eflags = 0x202;
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        jit.add_region(region).unwrap();
        let mut uc = machine(&bytes, None);
        uc.state.regs = initial;
        uc.write(BOUND, &3000u32.to_le_bytes()).unwrap();
        assert_eq!(jit.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            uc.run_unicorn(STOP, 100_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(jit.state.eip, uc.state.eip);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
        assert_eq!(jit.memory().read(DST, 1), uc.memory().read(DST, 1));
        let mut bound = [0; 4];
        shared.read_alias.copy_into(0, &mut bound);
        assert_eq!(bound, 3000u32.to_le_bytes());
    }
}

/// A rotate-and-mix body stepping its pointers, then `dec ecx`, `store`, `jne`.
fn counted_store(store: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(edx, dword_ptr(esi + 4)).unwrap();
        a.mov(ebx, eax).unwrap();
        a.ror(ebx, 7).unwrap();
        a.xor(eax, ebx).unwrap();
        a.add(eax, edx).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        store(a);
        a.jne(top).unwrap();
        a.ret().unwrap();
    })
}

#[test]
fn compare_latched_and_signed_counters_match_unicorn() {
    let all = |_: &mut Executor| {};
    let copy = |a: &mut CodeAssembler| {
        a.mov(eax, dword_ptr(esi + ebx * 4)).unwrap();
        a.mov(dword_ptr(edi + ebx * 4), eax).unwrap();
    };
    // Up to a register bound, exclusive and inclusive, and to a constant.
    let below = closed(copy, |a, top| {
        a.inc(ebx).unwrap();
        a.cmp(ebx, edx).unwrap();
        a.jb(top).unwrap();
    });
    let through = closed(copy, |a, top| {
        a.add(ebx, 1).unwrap();
        a.cmp(ebx, edx).unwrap();
        a.jle(top).unwrap();
    });
    let constant = closed(copy, |a, top| {
        a.inc(ebx).unwrap();
        a.cmp(ebx, 2500).unwrap();
        a.jne(top).unwrap();
    });
    for (name, bytes) in [
        ("below", &below),
        ("through", &through),
        ("constant", &constant),
    ] {
        for (first, bound) in [
            (0, 1),
            (0, 2),
            (0, 3),
            (5, 3000),
            (2499, 2500),
            (-3i32 as u32, 7),
        ] {
            if name == "below" && first > bound || name == "constant" && first >= 2500 {
                continue;
            }
            let mut r = regs(0, SRC + 12, DST + 12);
            (r[3], r[2]) = (first, bound);
            differential(name, bytes, r, all, 1 << 30);
            differential(name, bytes, r, all, 37);
        }
    }
    // Down to an inclusive register bound, the destination read-only midway.
    let down = closed(copy, |a, top| {
        a.dec(ebx).unwrap();
        a.cmp(ebx, ebp).unwrap();
        a.jge(top).unwrap();
    });
    let mut r = regs(0, SRC, DST);
    (r[3], r[5]) = (3000, 7);
    differential("down", &down, r, all, 1 << 30);
    differential("down-sliced", &down, r, all, 41);
    let readonly = |m: &mut Executor| m.protect(DST + 0x1000, 4096, READ).unwrap();
    differential("down-fault", &down, r, readonly, 1 << 30);
    // Pointer bounds: an end pointer met exactly, and one stepped past,
    // which runs on until the source ends.
    let pointers = closed(
        |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        },
        |a, top| {
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.cmp(esi, edx).unwrap();
            a.jne(top).unwrap();
        },
    );
    for (name, end) in [
        ("end", SRC + 4 * 3001),
        ("end-one", SRC + 4),
        ("end-missed", SRC + 4 * 3001 + 2),
    ] {
        let mut r = regs(0, SRC, DST + 0x8000);
        r[2] = end;
        differential(name, &pointers, r, all, 1 << 30);
        differential(name, &pointers, r, all, 43);
    }
    // A negative index stepping up to zero, and a positive one down past it:
    // an MMX scale with the counter as the address index.
    let negative = closed(copy, |a, top| {
        a.add(ebx, 1).unwrap();
        a.jne(top).unwrap();
    });
    for count in [1u32, 2, 3, 3000] {
        let r = regs(0, SRC + 0x4000, DST + 0x4000);
        let mut r = r;
        r[3] = count.wrapping_neg();
        differential("negative", &negative, r, all, 1 << 30);
        differential("negative-sliced", &negative, r, all, 31);
    }
    let scale = closed(
        |a| {
            a.movd(mm1, dword_ptr(esi + ecx)).unwrap();
            a.punpcklbw(mm1, mm0).unwrap();
            a.pmullw(mm1, mm5).unwrap();
            a.psrlw(mm1, 8).unwrap();
            a.packuswb(mm1, mm0).unwrap();
            a.movd(dword_ptr(edi + ecx), mm1).unwrap();
        },
        |a, top| {
            a.sub(ecx, 4).unwrap();
            a.jns(top).unwrap();
        },
    );
    for count in [1u32, 2, 3, 4, 3000] {
        let r = regs(4 * (count - 1), SRC, DST);
        differential("scale", &scale, r, all, 1 << 30);
        differential("scale-sliced", &scale, r, all, 29);
    }
    let r = regs(4 * 2999, SRC, DST);
    differential("scale-fault", &scale, r, readonly, 1 << 30);
    // A signed bound with a side exit, counting from below zero, and a
    // decrement whose flags the latch tests after a store and through TEST.
    let search = code(|a| {
        let mut top = a.create_label();
        let mut found = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi + ebx * 4 + 0x400)).unwrap();
        a.cmp(eax, 0x5555_5555).unwrap();
        a.je(found).unwrap();
        a.mov(dword_ptr(edi + ebx * 4 + 0x400), eax).unwrap();
        a.inc(ebx).unwrap();
        a.cmp(ebx, edx).unwrap();
        a.jl(top).unwrap();
        a.set_label(&mut found).unwrap();
        a.ret().unwrap();
    });
    let marker = |m: &mut Executor| {
        m.write(SRC + 0x400 + 4 * 1000, &0x5555_5555u32.to_le_bytes())
            .unwrap()
    };
    for bound in [-200i32, 40, 3000] {
        let mut r = regs(0, SRC, DST);
        (r[3], r[2]) = (-250i32 as u32, bound as u32);
        differential("search", &search, r, marker, 1 << 30);
        differential("search-sliced", &search, r, marker, 23);
    }
    let late = closed(
        |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(esi, 4).unwrap();
        },
        |a, top| {
            a.dec(ecx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.lea(edi, dword_ptr(edi + 4)).unwrap();
            a.test(ecx, ecx).unwrap();
            a.jg(top).unwrap();
        },
    );
    for count in [1u32, 2, 3000] {
        let r = regs(count, SRC, DST);
        differential("late", &late, r, all, 1 << 30);
        differential("late-sliced", &late, r, all, 19);
    }
}

/// The blend loop of a sprite renderer: skip transparent pixels, otherwise
/// mix source and destination by the source alpha and a global factor.
fn blend() -> Vec<u8> {
    code(|a| {
        let mut top = a.create_label();
        let mut skip = a.create_label();
        a.pxor(mm0, mm0).unwrap();
        a.movd(mm5, dword_ptr(ebp)).unwrap();
        a.movq(mm7, qword_ptr(ebp + 8)).unwrap();
        a.punpcklbw(mm5, mm0).unwrap();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.and(eax, 0xff00_0000u32 as i32).unwrap();
        a.je(skip).unwrap();
        a.movd(mm1, dword_ptr(esi)).unwrap();
        a.movd(mm2, dword_ptr(edi)).unwrap();
        a.punpcklbw(mm1, mm0).unwrap();
        a.punpcklbw(mm2, mm0).unwrap();
        a.pmullw(mm1, mm5).unwrap();
        a.psrlw(mm1, 8).unwrap();
        a.movq(mm6, mm7).unwrap();
        a.movq(mm3, mm1).unwrap();
        a.packuswb(mm3, mm0).unwrap();
        a.movd(ebx, mm3).unwrap();
        a.rol(ebx, 8).unwrap();
        a.mov(bh, bl).unwrap();
        a.mov(ax, bx).unwrap();
        a.rol(eax, 16).unwrap();
        a.mov(ax, bx).unwrap();
        a.movd(mm3, eax).unwrap();
        a.punpcklbw(mm3, mm0).unwrap();
        a.psubusw(mm6, mm3).unwrap();
        a.pmullw(mm1, mm3).unwrap();
        a.pmullw(mm2, mm6).unwrap();
        a.paddusw(mm2, mm1).unwrap();
        a.psrlw(mm2, 8).unwrap();
        a.packuswb(mm2, mm0).unwrap();
        a.movd(dword_ptr(edi), mm2).unwrap();
        a.mov(byte_ptr(edi + 3), 0xff).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(edi, 4).unwrap();
        a.add(esi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.emms().unwrap();
        a.ret().unwrap();
    })
}

const VARS: u32 = END - 0x100;

#[test]
fn batched_blends_match_unicorn_for_every_alpha_pattern_and_overlap() {
    let bytes = blend();
    // The destination starts as a second pattern; the factors sit at VARS.
    let base = |m: &mut Executor| {
        let pattern: Vec<u8> = (0..0x8000u32).map(|n| (n * 13 + 5) as u8).collect();
        m.write(DST, &pattern).unwrap();
        m.write(VARS, &[0x80, 0xa0, 0xc0, 0x00, 0, 0, 0, 0])
            .unwrap();
        m.write(VARS + 8, &0x00ff_00ff_00ff_00ffu64.to_le_bytes())
            .unwrap();
    };
    // Every 64th source pixel is transparent: most batches agree, some split.
    let mut r = regs(3000, SRC, DST);
    r[5] = VARS;
    for count in [1, 2, 3, 5, 64, 1001, 3000] {
        r[1] = count;
        differential("blend", &bytes, r, base, 1 << 30);
    }
    r[1] = 3000;
    differential("blend-sliced", &bytes, r, base, 37);
    differential("blend-sliced-odd", &bytes, r, base, 101);
    // Long transparent and opaque runs, alternating pixels, and runs whose
    // length leaves a lone pixel at either end of a batch.
    let runs = move |m: &mut Executor| {
        base(m);
        m.write(SRC + 0x400, &[0; 0x400]).unwrap();
        for n in 0..0x200u32 {
            m.write(SRC + 0x800 + n * 4 + 3, &[0x80]).unwrap();
        }
        for n in 0..0x100u32 {
            let alpha = if n % 2 == 0 { 0 } else { 0x40 };
            m.write(SRC + 0x1000 + n * 4 + 3, &[alpha]).unwrap();
        }
        for n in 0..0x100u32 {
            let alpha = if n % 3 == 0 { 0 } else { 0x40 };
            m.write(SRC + 0x1400 + n * 4 + 3, &[alpha]).unwrap();
        }
    };
    differential("blend-runs", &bytes, r, runs, 1 << 30);
    differential("blend-runs-sliced", &bytes, r, runs, 53);
    // In place, and overlapping by a pixel either way, or by part of one.
    for (name, dst) in [
        ("blend-inplace", SRC),
        ("blend-ahead", SRC + 4),
        ("blend-behind", SRC - 4),
        ("blend-straddle", SRC + 2),
        ("blend-far", SRC + 8),
    ] {
        let mut r = r;
        r[7] = dst;
        r[1] = 2000;
        differential(name, &bytes, r, runs, 1 << 30);
    }
    // The destination turns read-only midway.
    let readonly = move |m: &mut Executor| {
        runs(m);
        m.protect(DST + 0x2000, 4096, READ).unwrap();
    };
    differential("blend-readonly", &bytes, r, readonly, 1 << 30);
}

#[test]
fn batched_copies_and_integer_bodies_match_unicorn() {
    let all = |_: &mut Executor| {};
    // Quadword copies, a 16-bit counter included.
    for narrow in [false, true] {
        let quadwords = counted(narrow, |a| {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();
            a.add(esi, 8).unwrap();
            a.add(edi, 8).unwrap();
        });
        let mut r = regs(1500, SRC, DST);
        differential("movq", &quadwords, r, all, 1 << 30);
        differential("movq-sliced", &quadwords, r, all, 41);
        r[1] = 0x0003_0000 | 999;
        differential("movq-wide", &quadwords, r, all, 1 << 30);
    }
    let dwords = counted(true, |a| {
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
    });
    let mut r = regs(0x7fff_0000 | 3001, SRC, DST);
    differential("movd-narrow", &dwords, r, all, 1 << 30);
    // Overlapping copies smear or shift the source.
    r[7] = SRC + 4;
    differential("movd-smear", &dwords, r, all, 1 << 30);
    r[7] = SRC - 4;
    differential("movd-shift", &dwords, r, all, 1 << 30);
    // Byte averages through zero and sign extension and a byte store.
    let bytes = counted(false, |a| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.movsx(ebx, byte_ptr(esi + 1)).unwrap();
        a.add(eax, ebx).unwrap();
        a.sar(eax, 1).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.movzx(edx, ah).unwrap();
        a.add(esi, 2).unwrap();
        a.add(edi, 1).unwrap();
    });
    let r = regs(5001, SRC, DST);
    differential("bytes", &bytes, r, all, 1 << 30);
    differential("bytes-sliced", &bytes, r, all, 29);
    // Stepping by INC keeps the carry of the body's last addition, which a
    // batch does not compute: the scalar iteration after it does.
    let increments = counted(false, |a| {
        let mut skip = a.create_label();
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.add(al, 0x90).unwrap();
        a.cmp(eax, 0x40).unwrap();
        a.je(skip).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.set_label(&mut skip).unwrap();
        a.inc(esi).unwrap();
        a.inc(edi).unwrap();
    });
    for count in [1, 2, 3, 4, 7, 5001] {
        let r = regs(count, SRC, DST);
        differential("increments", &increments, r, all, 1 << 30);
    }
    let r = regs(5001, SRC, DST);
    differential("increments-sliced", &increments, r, all, 31);
    // The same loop leaving early at the first byte 0xb0, the 171st, with
    // the exit at every position within a batch.
    let search = code(|a| {
        let mut top = a.create_label();
        let mut found = a.create_label();
        a.set_label(&mut top).unwrap();
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.add(al, 0x90).unwrap();
        a.cmp(eax, 0x40).unwrap();
        a.je(found).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.inc(esi).unwrap();
        a.inc(edi).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.set_label(&mut found).unwrap();
        a.ret().unwrap();
    });
    for start in 0..4 {
        for count in [100, 171 - start, 172 - start, 5001] {
            let r = regs(count, SRC + start, DST);
            differential("search", &search, r, all, 1 << 30);
            differential("search-sliced", &search, r, all, 23);
        }
    }
    // Integer arithmetic choosing between two results per element.
    let integers = counted(false, |a| {
        let mut odd = a.create_label();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(ebx, eax).unwrap();
        a.imul_3(ebx, ebx, 3).unwrap();
        a.lea(edx, dword_ptr(eax + ebx * 2 + 5)).unwrap();
        a.sar(edx, 3).unwrap();
        a.xor(edx, eax).unwrap();
        a.ror(edx, 7).unwrap();
        a.test(dl, 1).unwrap();
        a.jne(odd).unwrap();
        a.neg(edx).unwrap();
        a.not(ebx).unwrap();
        a.set_label(&mut odd).unwrap();
        a.or(edx, ebp).unwrap();
        a.mov(dword_ptr(edi), edx).unwrap();
        a.mov(word_ptr(edi + 4), bx).unwrap();
        a.lea(edx, dword_ptr(esi + 3)).unwrap();
        a.mov(dword_ptr(edi + 8), edx).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 12).unwrap();
    });
    let mut r = regs(2001, SRC, DST);
    r[5] = 0x1000_0001;
    differential("integers", &integers, r, all, 1 << 30);
    differential("integers-sliced", &integers, r, all, 47);
}

#[test]
fn batched_loop_interrupts_and_resumes_exactly() {
    let bytes = counted(false, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, ebx).unwrap();
        // A doubleword store here would be a forwarded slot instead.
        a.mov(byte_ptr(edi), al).unwrap();
        a.add(edx, 1).unwrap();
    });
    let mut m = machine(&bytes, Some(CompileStrategy::Speed));
    m.state.regs = regs(100_000_000, SRC, DST);
    m.state.regs[3] = 5;
    m.write(SRC, &1u32.to_le_bytes()).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    m.set_host_interrupt_flag(flag.clone());
    let notify = flag.clone();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        notify.store(true, Ordering::Release);
    });
    let outcome = m.run(STOP, 1 << 40).unwrap();
    producer.join().unwrap();
    assert_eq!(outcome, RunOutcome::Interrupted);
    let completed = m.state.regs[2];
    assert!(completed > 0 && completed < 100_000_000);
    assert_eq!(m.state.regs[1], 100_000_000 - completed);
    assert_eq!(m.state.regs[0], 6);
    assert_eq!(m.memory().read(DST, 1).unwrap(), [6]);
    flag.store(false, Ordering::Release);
    m.state.regs[1] = 11;
    assert_eq!(m.run(STOP, 1 << 20).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[2], completed + 11);
}

#[test]
fn compact_copies_cover_four_lane_aliases_tails_and_observable_budget_state() {
    for packed in [false, true] {
        let bytes = counted(true, |a| {
            if packed {
                a.movd(mm0, dword_ptr(esi)).unwrap();
                a.movd(dword_ptr(edi), mm0).unwrap();
            } else {
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            }
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
        });
        for count in [3, 4, 5, 8, 9, 129] {
            // +/-8 and +/-12 distinguish a four-lane alias proof from V=2.
            for destination in [
                DST,
                SRC,
                SRC + 8,
                SRC + 12,
                SRC - 8,
                SRC - 12,
                SRC + 15,
                SRC - 15,
                SRC + 16,
                SRC - 16,
            ] {
                differential(
                    "compact-copy",
                    &bytes,
                    regs(0x2345_0000 | count, SRC, destination),
                    |_| {},
                    37,
                );
            }
        }
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for fuel in [6, 7, 8, 9, 10, 11, 13, 17, 33, 34, 35] {
                let mut jit = machine(&bytes, Some(strategy));
                let mut uc = machine(&bytes, None);
                for m in [&mut jit, &mut uc] {
                    m.state.regs = regs(0x2345_0000 | 200, SRC, DST);
                }
                assert_eq!(jit.run(STOP, fuel).unwrap(), RunOutcome::Budget);
                let done = (jit.state.regs[6] - SRC) / 4;
                assert!(done > 0 && done < 200);
                // One prologue instruction, six per complete iteration.
                assert_eq!(
                    uc.run_unicorn(STOP, 1 + 6 * done as usize).unwrap(),
                    RunOutcome::Budget
                );
                assert_eq!(jit.state.eip, uc.state.eip);
                assert_eq!(jit.state.regs, uc.state.regs, "packed={packed} fuel={fuel}");
                assert_eq!(jit.state.mmx, uc.state.mmx);
                assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
                assert_eq!(
                    jit.memory().read(DST, 804).unwrap(),
                    uc.memory().read(DST, 804).unwrap()
                );
                // Fault before the next MOV/MOVD overwrites its result: the
                // last completed element and flags must remain observable.
                for m in [&mut jit, &mut uc] {
                    m.protect(SRC, 4096, 0).unwrap();
                }
                let expected = uc.run_unicorn(STOP, 100).unwrap();
                assert_eq!(jit.run(STOP, 100).unwrap(), expected);
                assert_eq!(jit.state.regs, uc.state.regs);
                assert_eq!(jit.state.mmx, uc.state.mmx);
                assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
            }
        }
    }
}

#[test]
fn compact_copy_interrupt_publishes_progress_and_resumes() {
    let bytes = counted(false, |a| {
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
    });
    // A sufficiently long in-place copy allows an asynchronous interrupt;
    // one zero-filled allocation, without a second large oracle allocation.
    const COUNT: u32 = 16_000_000;
    let mut m = adv_cpu::Executor::new(CODE, 0x4200000).unwrap();
    m.protect(CODE, 0x4200000, READ | WRITE).unwrap();
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.add_region(Region::new(CODE, bytes.len())).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.state.regs = regs(COUNT, SRC, SRC);
    m.state.eip = CODE;
    // Finish compilation before starting the interrupt producer.
    assert_eq!(m.run(STOP, 64).unwrap(), RunOutcome::Budget);
    m.state.regs = regs(COUNT, SRC, SRC);
    m.state.eip = CODE;
    let flag = Arc::new(AtomicBool::new(false));
    m.set_host_interrupt_flag(flag.clone());
    let notify = flag.clone();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        notify.store(true, Ordering::Release);
    });
    let budget = 1 << 40;
    let outcome = m.run(STOP, budget).unwrap();
    producer.join().unwrap();
    assert_eq!(outcome, RunOutcome::Interrupted);
    let done = COUNT - m.state.regs[1];
    assert!(
        done >= 5 && done < COUNT,
        "must interrupt after a packed batch"
    );
    assert_eq!(m.state.regs[6], SRC + done * 4);
    assert_eq!(m.state.regs[7], SRC + done * 4);
    assert_eq!(m.state.mmx[0], 0);
    assert_eq!(budget - m.state.fuel, u64::from(done) + 1);
    assert_eq!(m.stats.interpreter_instructions, 0);
    flag.store(false, Ordering::Release);
    m.state.regs[1] = 11;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[6], SRC + (done + 11) * 4);
    assert_eq!(m.state.regs[7], m.state.regs[6]);
}
