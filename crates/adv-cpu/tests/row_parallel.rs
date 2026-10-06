mod support;

use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE, X87Register};
use iced_x86::code_asm::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const STACK: u32 = 0x12f00;
const FRAME: u32 = 0x13000;
const SRC: u32 = 0x20000;
const DST: u32 = 0x40000;
const END: u32 = 0x60000;

#[derive(Clone, Copy, Debug)]
enum Body {
    Copy,
    Mix,
    Simd,
    Transparent,
}

impl Body {
    fn stride(self) -> u32 {
        if matches!(self, Self::Simd) { 16 } else { 4 }
    }
}

/// EBP names a read-only frame; EBX counts rows. The glue restores ECX and
/// corrects both pointers after the inner loop has advanced across one row.
fn nested(body: Body) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut outer = a.create_label();
    let mut inner = a.create_label();
    let mut skip = a.create_label();
    a.set_label(&mut outer).unwrap();
    a.mov(ecx, dword_ptr(ebp)).unwrap();
    a.set_label(&mut inner).unwrap();
    match body {
        Body::Copy => {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        }
        Body::Mix => {
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(mm1, dword_ptr(edi)).unwrap();
            a.paddusb(mm0, mm1).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
        }
        Body::Simd => {
            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.movdqu(xmm1, xmmword_ptr(edi)).unwrap();
            a.paddusb(xmm0, xmm1).unwrap();
            a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        }
        Body::Transparent => {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.je(skip).unwrap();
            a.movd(mm0, eax).unwrap();
            a.movd(mm1, dword_ptr(edi)).unwrap();
            a.paddusb(mm0, mm1).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            // These values remain visible when following rows skip every
            // pixel, so the merge must choose the most recent actual writer.
            a.movd(mm2, eax).unwrap();
            a.mov(edx, eax).unwrap();
            a.set_label(&mut skip).unwrap();
        }
    }
    a.add(esi, body.stride() as i32).unwrap();
    a.add(edi, body.stride() as i32).unwrap();
    a.dec(ecx).unwrap();
    a.jne(inner).unwrap();
    a.add(esi, dword_ptr(ebp + 4)).unwrap();
    a.add(edi, dword_ptr(ebp + 8)).unwrap();
    a.dec(ebx).unwrap();
    a.jne(outer).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

#[derive(Clone, Copy, Debug)]
struct Rectangle {
    width: u32,
    height: u32,
    source: u32,
    destination: u32,
    source_pitch: i32,
    destination_pitch: i32,
}

fn offset(origin: u32, row: u32, pitch: i32) -> u32 {
    origin.wrapping_add(row.wrapping_mul(pitch as u32))
}

fn data_end(rectangle: Rectangle, body: Body) -> u32 {
    let last = rectangle.height - 1;
    let source = rectangle
        .source
        .max(offset(rectangle.source, last, rectangle.source_pitch));
    let destination = rectangle.destination.max(offset(
        rectangle.destination,
        last,
        rectangle.destination_pitch,
    ));
    let end = source.max(destination) + rectangle.width * body.stride();
    END.max((end + 4095) & !4095)
}

fn machine(
    bytes: &[u8],
    rectangle: Rectangle,
    body: Body,
    compilation: Option<(CompileStrategy, usize)>,
) -> Executor {
    let end = data_end(rectangle, body);
    let mut m = Executor::new(CODE, (end - CODE) as usize).unwrap();
    m.protect(CODE, (end - CODE) as usize, READ | WRITE)
        .unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let pattern: Vec<u8> = (0..end - SRC)
        .map(|n| (n.wrapping_mul(7).wrapping_add(3)) as u8)
        .collect();
    m.write(SRC, &pattern).unwrap();
    let correction = rectangle.width.wrapping_mul(body.stride());
    let mut frame = Vec::new();
    frame.extend_from_slice(&rectangle.width.to_le_bytes());
    frame.extend_from_slice(
        &(rectangle.source_pitch as u32)
            .wrapping_sub(correction)
            .to_le_bytes(),
    );
    frame.extend_from_slice(
        &(rectangle.destination_pitch as u32)
            .wrapping_sub(correction)
            .to_le_bytes(),
    );
    m.write(FRAME, &frame).unwrap();
    m.protect(FRAME, 4096, READ).unwrap();
    if matches!(body, Body::Transparent) {
        for row in 0..rectangle.height {
            let mut pixels = Vec::new();
            for column in 0..rectangle.width {
                // The final two rows are completely transparent. Earlier
                // rows also split vector lanes and skip occasional pixels.
                let active = row + 2 < rectangle.height && (row + column) % 3 != 0;
                let pixel = if active {
                    0x0102_0300 | ((row * rectangle.width + column + 1) & 0xff)
                } else {
                    0
                };
                pixels.extend_from_slice(&pixel.to_le_bytes());
            }
            m.write(
                offset(rectangle.source, row, rectangle.source_pitch),
                &pixels,
            )
            .unwrap();
        }
    }
    m.state.eip = CODE;
    m.state.regs = [
        0xa1b2_c3d4,
        0,
        0xdead_beef,
        rectangle.height,
        STACK,
        FRAME,
        rectangle.source,
        rectangle.destination,
    ];
    m.state.eflags = 0x203;
    m.state.xmm = [[0xa5; 16]; 8];
    m.state.x87.regs = std::array::from_fn(|index| {
        X87Register::from_parts(0x8000_0000_1111_0000 + index as u64, 0x4000 + index as u16)
    });
    m.state.x87.set_top(3);
    if let Some((strategy, workers)) = compilation {
        m.set_row_workers(workers).unwrap();
        m.set_row_threshold(0);
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m
}

fn compare(jit: &Executor, oracle: &Executor, body: Body, context: &str) {
    let mut actual = jit.state.clone();
    let mut expected = oracle.state.clone();
    actual.refresh_mmx();
    expected.refresh_mmx();
    assert_eq!(actual.eip, expected.eip, "{context}: PC");
    assert_eq!(actual.regs, expected.regs, "{context}: registers");
    assert_eq!(
        actual.eflags & 0x8d5,
        expected.eflags & 0x8d5,
        "{context}: flags"
    );
    assert_eq!(actual.mmx, expected.mmx, "{context}: MMX");
    let touched = match body {
        Body::Copy | Body::Simd => 0,
        Body::Mix => 0b011,
        Body::Transparent => 0b111,
    };
    for index in 0..8 {
        // Unicorn retains the prior x87 exponent for MMX destinations. Check
        // its payload while requiring the architectural MMX exponent here.
        let raw = if touched & (1 << index) != 0 {
            X87Register::from_parts(expected.x87.regs[index].significand(), 0xffff)
        } else {
            expected.x87.regs[index]
        };
        assert_eq!(
            actual.x87.regs[index], raw,
            "{context}: x87 physical slot {index}"
        );
    }
    assert_eq!(
        actual.x87.control, expected.x87.control,
        "{context}: x87 control"
    );
    assert_eq!(
        actual.x87.status, expected.x87.status,
        "{context}: x87 status"
    );
    // Unicorn reconstructs numeric tags of modified MMX slots; architectural
    // MMX entry instead marks every physical slot valid, retaining raw80 bits.
    let expected_tag = if matches!(body, Body::Copy | Body::Simd) {
        0xffff
    } else {
        0
    };
    assert_eq!(actual.x87.tag, expected_tag, "{context}: x87 tag");
    assert_eq!(actual.xmm, expected.xmm, "{context}: XMM");
    assert_eq!(actual.mxcsr, expected.mxcsr, "{context}: MXCSR");
    compare_bytes(jit, oracle, SRC, END, context);
    assert_eq!(
        jit.memory().read(FRAME, 4096).unwrap(),
        oracle.memory().read(FRAME, 4096).unwrap(),
        "{context}: invariant frame"
    );
}

fn compare_native(jit: &Executor, serial: &Executor, context: &str) {
    let mut actual = jit.state.clone();
    let mut expected = serial.state.clone();
    actual.refresh_mmx();
    expected.refresh_mmx();
    // Remaining host work allowance is the only nonarchitectural difference.
    actual.fuel = expected.fuel;
    assert_eq!(actual, expected, "{context}: complete native CPU state");
}

fn compare_bytes(jit: &Executor, oracle: &Executor, start: u32, end: u32, context: &str) {
    let actual = jit.memory().read(start, (end - start) as usize).unwrap();
    let expected = oracle.memory().read(start, (end - start) as usize).unwrap();
    if let Some(index) = actual.iter().zip(expected).position(|(a, b)| a != b) {
        panic!(
            "{context}: byte {:#x} differs: {:02x} != {:02x}",
            start + index as u32,
            actual[index],
            expected[index]
        );
    }
}

fn finish(m: &mut Executor, slice: u64, context: &str) -> RunOutcome {
    for _ in 0..100_000 {
        let outcome = m.run(STOP, slice).unwrap();
        assert!(m.state.fuel <= slice, "{context}: fuel exceeded the slice");
        if outcome != RunOutcome::Budget {
            return outcome;
        }
    }
    panic!("{context}: execution did not finish");
}

#[test]
fn nested_copy_and_mmx_rows_match_unicorn_with_positive_and_negative_pitch() {
    for body in [Body::Copy, Body::Mix] {
        let bytes = nested(body);
        for pitch in [288, -320] {
            let rectangle = Rectangle {
                width: 64,
                height: 11,
                source: if pitch < 0 { SRC + 0x4000 } else { SRC },
                destination: if pitch < 0 { DST + 0x5000 } else { DST },
                source_pitch: pitch,
                destination_pitch: pitch + 16,
            };
            let mut oracle = machine(&bytes, rectangle, body, None);
            assert_eq!(
                oracle.run_unicorn(STOP, 100_000).unwrap(),
                RunOutcome::ReachedStop
            );
            for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
                let mut serial = machine(&bytes, rectangle, body, Some((strategy, 0)));
                serial.set_row_threshold(u64::MAX);
                assert_eq!(
                    finish(&mut serial, 1 << 20, "native serial"),
                    RunOutcome::ReachedStop
                );
                for workers in 0..=3 {
                    let context = format!("{body:?} pitch={pitch} {strategy:?} workers={workers}");
                    let mut jit = machine(&bytes, rectangle, body, Some((strategy, workers)));
                    assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
                    compare(&jit, &oracle, body, &context);
                    compare_native(&jit, &serial, &context);
                    assert_eq!(
                        jit.state.regs[6],
                        offset(rectangle.source, rectangle.height, rectangle.source_pitch),
                        "{context}: final source glue"
                    );
                    assert_eq!(
                        jit.state.regs[7],
                        offset(
                            rectangle.destination,
                            rectangle.height,
                            rectangle.destination_pitch
                        ),
                        "{context}: final destination glue"
                    );
                    let stats = jit.row_parallel_stats();
                    assert_eq!(stats.workers, workers, "{context}");
                    assert!(
                        stats.batches > 0 && stats.rows > 1,
                        "{context}: row execution was not admitted"
                    );
                }
            }
        }
    }
}

#[test]
fn transparent_final_rows_preserve_the_last_register_and_mmx_writers() {
    let body = Body::Transparent;
    let bytes = nested(body);
    let rectangle = Rectangle {
        width: 33,
        height: 13,
        source: SRC,
        destination: DST,
        source_pitch: 160,
        destination_pitch: 176,
    };
    let mut oracle = machine(&bytes, rectangle, body, None);
    assert_eq!(
        oracle.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_ne!(oracle.state.regs[2], 0xdead_beef);
    assert_ne!(oracle.state.mmx[2], 0x8000_0000_1111_0002);
    let mut serial = machine(&bytes, rectangle, body, Some((CompileStrategy::Fast, 0)));
    serial.set_row_threshold(u64::MAX);
    assert_eq!(
        finish(&mut serial, 1 << 20, "transparent serial"),
        RunOutcome::ReachedStop
    );
    for workers in 0..=3 {
        let context = format!("transparent workers={workers}");
        let mut jit = machine(
            &bytes,
            rectangle,
            body,
            Some((CompileStrategy::Fast, workers)),
        );
        assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
        compare(&jit, &oracle, body, &context);
        compare_native(&jit, &serial, &context);
        assert!(
            jit.row_parallel_stats().batches > 0,
            "{context}: conditional rows were not admitted"
        );
    }
}

#[test]
fn row_prefix_resets_survive_conditional_inner_writes() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut outer = a.create_label();
    let mut inner = a.create_label();
    let mut skip = a.create_label();
    a.set_label(&mut outer).unwrap();
    a.mov(ecx, dword_ptr(ebp)).unwrap();
    a.xor(eax, eax).unwrap();
    a.set_label(&mut inner).unwrap();
    a.cmp(dword_ptr(esi), 0).unwrap();
    a.je(skip).unwrap();
    a.mov(eax, 17).unwrap();
    a.set_label(&mut skip).unwrap();
    a.mov(dword_ptr(edi), eax).unwrap();
    a.add(esi, 4).unwrap();
    a.add(edi, 4).unwrap();
    a.dec(ecx).unwrap();
    a.jne(inner).unwrap();
    a.add(esi, dword_ptr(ebp + 4)).unwrap();
    a.add(edi, dword_ptr(ebp + 8)).unwrap();
    a.dec(ebx).unwrap();
    a.jne(outer).unwrap();
    a.ret().unwrap();
    let bytes = a.assemble(u64::from(CODE)).unwrap();
    for width in [1, 17] {
        let rectangle = Rectangle {
            width,
            height: 9,
            source: SRC,
            destination: DST,
            source_pitch: (width * 4 + 16) as i32,
            destination_pitch: (width * 4 + 32) as i32,
        };
        // Reuse the transparent input pattern; this kernel changes no MMX state.
        let mut oracle = machine(&bytes, rectangle, Body::Transparent, None);
        assert_eq!(
            oracle.run_unicorn(STOP, 100_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(oracle.state.regs[0], 0);
        assert_eq!(
            oracle
                .memory()
                .read(offset(DST, 1, rectangle.destination_pitch), 4)
                .unwrap(),
            17u32.to_le_bytes(),
            "an earlier row must exercise the conditional writer"
        );
        let mut serial = machine(
            &bytes,
            rectangle,
            Body::Transparent,
            Some((CompileStrategy::Fast, 0)),
        );
        serial.set_row_threshold(u64::MAX);
        assert_eq!(
            finish(&mut serial, 1 << 20, "prefix reset serial"),
            RunOutcome::ReachedStop
        );
        for workers in [0, 3] {
            let context = format!("prefix reset width={width} workers={workers}");
            let mut jit = machine(
                &bytes,
                rectangle,
                Body::Transparent,
                Some((CompileStrategy::Fast, workers)),
            );
            assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
            assert_eq!(
                jit.state.regs[0], 0,
                "{context}: final transparent row lost its reset"
            );
            compare(&jit, &oracle, Body::Copy, &context);
            compare_native(&jit, &serial, &context);
            let stats = jit.row_parallel_stats();
            assert!(
                stats.batches > 0 && stats.rows > 1,
                "{context}: reset rows were not admitted"
            );
        }
    }
}

#[test]
fn unaligned_sse2_rows_preserve_complete_xmm_state() {
    let body = Body::Simd;
    let bytes = nested(body);
    let rectangle = Rectangle {
        width: 17,
        height: 9,
        source: SRC + 3,
        destination: DST + 5,
        source_pitch: 320,
        destination_pitch: 336,
    };
    let mut oracle = machine(&bytes, rectangle, body, None);
    assert_eq!(
        oracle.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_ne!(oracle.state.xmm[0], [0xa5; 16]);
    assert_ne!(oracle.state.xmm[1], [0xa5; 16]);
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut serial = machine(&bytes, rectangle, body, Some((strategy, 0)));
        serial.set_row_threshold(u64::MAX);
        assert_eq!(
            finish(&mut serial, 1 << 20, "SSE2 serial"),
            RunOutcome::ReachedStop
        );
        for workers in [0, 3] {
            let context = format!("SSE2 {strategy:?} workers={workers}");
            let mut jit = machine(&bytes, rectangle, body, Some((strategy, workers)));
            assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
            compare(&jit, &oracle, body, &context);
            compare_native(&jit, &serial, &context);
            let stats = jit.row_parallel_stats();
            assert!(
                stats.batches > 0 && stats.rows > 1,
                "{context}: SSE2 rows were not admitted"
            );
        }
    }
}

#[test]
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
fn two_guest_workers_disable_the_row_pool() {
    let mut m = adv_cpu::Executor::new(CODE, 4096).unwrap();
    m.set_row_workers(3).unwrap();
    assert_eq!(m.row_parallel_stats().workers, 3);
    m.set_guest_workers(2).unwrap();
    assert_eq!(m.guest_workers(), 2);
    let stats = m.row_parallel_stats();
    assert_eq!(stats.workers, 0);
    assert_eq!(stats.batches, 0);
    assert!(m.set_row_workers(1).is_err());
}

#[test]
fn budget_slices_preserve_the_complete_serial_state_and_eventually_admit_rows() {
    let body = Body::Mix;
    let bytes = nested(body);
    let rectangle = Rectangle {
        width: 17,
        height: 19,
        source: SRC,
        destination: DST,
        source_pitch: 96,
        destination_pitch: 112,
    };
    let mut oracle = machine(&bytes, rectangle, body, None);
    assert_eq!(
        oracle.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    let mut serial = machine(&bytes, rectangle, body, Some((CompileStrategy::Fast, 0)));
    serial.set_row_threshold(u64::MAX);
    assert_eq!(
        finish(&mut serial, 1 << 20, "sliced serial"),
        RunOutcome::ReachedStop
    );
    for slice in [1, 31, 257] {
        for workers in 0..=3 {
            let context = format!("sliced budget={slice} workers={workers}");
            let mut jit = machine(
                &bytes,
                rectangle,
                body,
                Some((CompileStrategy::Fast, workers)),
            );
            assert_eq!(finish(&mut jit, slice, &context), RunOutcome::ReachedStop);
            compare(&jit, &oracle, body, &context);
            compare_native(&jit, &serial, &context);
            if slice == 257 {
                assert!(
                    jit.row_parallel_stats().batches > 0,
                    "{context}: funded rows were not admitted"
                );
            }
        }
    }
}

#[test]
fn changing_the_row_width_rejects_stale_sampled_pointer_deltas() {
    let body = Body::Copy;
    let bytes = nested(body);
    let rectangle = Rectangle {
        width: 4,
        height: 5,
        source: SRC,
        destination: FRAME,
        source_pitch: 16,
        destination_pitch: 16,
    };
    let setup = |m: &mut Executor| {
        m.protect(FRAME, 4096, READ | WRITE).unwrap();
        let first_row: Vec<u8> = [2u32, 0, 0, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        m.write(SRC, &first_row).unwrap();
    };
    // The first row replaces width=4 with width=2 but leaves both pointer
    // corrections zero. Later rows advance eight bytes, so the sixteen-byte
    // delta sampled after that first row cannot describe their next entries.
    let mut oracle = machine(&bytes, rectangle, body, None);
    setup(&mut oracle);
    assert_eq!(
        oracle.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(oracle.memory().read(FRAME, 4).unwrap(), 2u32.to_le_bytes());
    assert_eq!(oracle.state.regs[6], SRC + 16 + 4 * 8);
    assert_eq!(oracle.state.regs[7], FRAME + 16 + 4 * 8);
    let mut serial = machine(&bytes, rectangle, body, Some((CompileStrategy::Fast, 0)));
    setup(&mut serial);
    serial.set_row_threshold(u64::MAX);
    assert_eq!(
        finish(&mut serial, 1 << 20, "changed width serial"),
        RunOutcome::ReachedStop
    );
    for workers in [0, 3] {
        let context = format!("changed width workers={workers}");
        let mut jit = machine(
            &bytes,
            rectangle,
            body,
            Some((CompileStrategy::Fast, workers)),
        );
        setup(&mut jit);
        assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
        compare(&jit, &oracle, body, &context);
        compare_native(&jit, &serial, &context);
        compare_bytes(&jit, &oracle, FRAME, END, &context);
        assert_eq!(
            jit.row_parallel_stats().batches,
            0,
            "{context}: stale row deltas were admitted"
        );
    }
}

#[test]
fn cross_row_aliases_and_overlapping_output_rows_remain_serial() {
    let body = Body::Copy;
    let bytes = nested(body);
    for rectangle in [
        Rectangle {
            width: 32,
            height: 9,
            source: SRC,
            // Each completed row becomes the following row's input.
            destination: SRC + 160,
            source_pitch: 160,
            destination_pitch: 160,
        },
        Rectangle {
            width: 32,
            height: 9,
            source: SRC,
            destination: DST,
            source_pitch: 160,
            destination_pitch: 64,
        },
    ] {
        let mut oracle = machine(&bytes, rectangle, body, None);
        assert_eq!(
            oracle.run_unicorn(STOP, 100_000).unwrap(),
            RunOutcome::ReachedStop
        );
        for workers in 0..=3 {
            let context = format!("alias {rectangle:?} workers={workers}");
            let mut jit = machine(
                &bytes,
                rectangle,
                body,
                Some((CompileStrategy::Fast, workers)),
            );
            assert_eq!(finish(&mut jit, 1 << 20, &context), RunOutcome::ReachedStop);
            compare(&jit, &oracle, body, &context);
            let stats = jit.row_parallel_stats();
            assert_eq!(stats.batches, 0, "{context}: dependent rows ran as a batch");
            assert!(
                stats.rejected > 0,
                "{context}: dependence guard was not exercised"
            );
        }
    }
}

#[test]
fn interruption_at_a_row_batch_checkpoint_preserves_a_resumable_serial_prefix() {
    let body = Body::Mix;
    let bytes = nested(body);
    let width = 64;
    let height = 128;
    let pitch = width * 4;
    let rectangle = Rectangle {
        width,
        height,
        source: SRC,
        destination: SRC + height * pitch + 4096,
        source_pitch: pitch as i32,
        destination_pitch: pitch as i32,
    };
    let mut jit = machine(&bytes, rectangle, body, Some((CompileStrategy::Fast, 1)));
    // Compile and establish the first serial row before arming interruption.
    let warm_budget = u64::from(width) + 3;
    assert_eq!(jit.run(STOP, warm_budget).unwrap(), RunOutcome::Budget);
    let warm_charge = warm_budget - jit.state.fuel;
    let warm_rows = jit.row_parallel_stats().rows;
    let flag = Arc::new(AtomicBool::new(false));
    jit.set_host_interrupt_flag(flag.clone());
    // Complete admitted rows in a bounded slice while leaving rows to resume.
    let budget = u64::from(width) * 64;
    assert_eq!(jit.run(STOP, budget).unwrap(), RunOutcome::Budget);
    assert!(jit.state.fuel <= budget);
    let consumed = warm_charge + budget - jit.state.fuel;
    assert!(jit.state.regs[3] > 0 && jit.state.regs[3] < height);
    assert!(
        jit.row_parallel_stats().rows > warm_rows,
        "checkpoint did not follow an admitted row batch"
    );
    // Signal only at this recorded checkpoint. The following entry must
    // consume the pending interrupt without altering the completed prefix.
    flag.store(true, Ordering::Release);
    assert_eq!(jit.run(STOP, budget).unwrap(), RunOutcome::Interrupted);
    assert_eq!(jit.state.fuel, budget, "interrupt charged unfinished work");

    let mut serial = machine(&bytes, rectangle, body, Some((CompileStrategy::Fast, 0)));
    serial.set_row_threshold(u64::MAX);
    assert_eq!(serial.run(STOP, consumed).unwrap(), RunOutcome::Budget);
    compare(&jit, &serial, body, "interrupted prefix");
    compare_native(&jit, &serial, "interrupted prefix");
    compare_bytes(
        &jit,
        &serial,
        SRC,
        data_end(rectangle, body),
        "interrupted prefix",
    );

    flag.store(false, Ordering::Release);
    assert_eq!(
        finish(&mut jit, 1 << 30, "interrupt resume"),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        finish(&mut serial, 1 << 30, "serial resume"),
        RunOutcome::ReachedStop
    );
    compare(&jit, &serial, body, "resumed prefix");
    compare_native(&jit, &serial, "resumed prefix");
    compare_bytes(
        &jit,
        &serial,
        SRC,
        data_end(rectangle, body),
        "resumed prefix",
    );
}
