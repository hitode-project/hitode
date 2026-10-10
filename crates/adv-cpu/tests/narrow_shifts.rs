mod support;

use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;

fn assemble(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.enable_auto_jit(false);
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m
}

fn initial(destination: usize, value: u32, count: Option<u8>, flags: u32) -> CpuState {
    let mut state = CpuState {
        regs: [
            0x89ab_cdef,
            0x7654_ba98,
            0xabcd_ef01,
            0xfedc_ba98,
            0x2567_89ab,
            0x3456_789a,
            0x4567_89ab,
            0x5678_9abc,
        ],
        eip: CODE,
        eflags: flags,
        ..CpuState::default()
    };
    let (slot, shift, mask) = if destination < 8 {
        (destination % 4, if destination < 4 { 0 } else { 8 }, 0xff)
    } else {
        (destination - 8, 0, 0xffff)
    };
    state.regs[slot] = (state.regs[slot] & !(mask << shift)) | ((value & mask) << shift);
    if let Some(count) = count {
        // Read CL before writeback even when the destination is CL, CH or CX.
        state.regs[1] = (state.regs[1] & !0xff) | u32::from(count);
    }
    state
}

fn shift_code(destination: usize, operation: usize, count: Option<u8>) -> Vec<u8> {
    assemble(|a| {
        macro_rules! shift {
            ($dst:expr) => {
                match (operation, count) {
                    (0, None) => a.shl($dst, cl),
                    (0, Some(count)) => a.shl($dst, i32::from(count)),
                    (1, None) => a.shr($dst, cl),
                    (1, Some(count)) => a.shr($dst, i32::from(count)),
                    (_, None) => a.sar($dst, cl),
                    (_, Some(count)) => a.sar($dst, i32::from(count)),
                }
                .unwrap()
            };
        }
        if destination < 8 {
            shift!([al, cl, dl, bl, ah, ch, dh, bh][destination]);
        } else {
            shift!([ax, cx, dx, bx, sp, bp, si, di][destination - 8]);
        }
    })
}

fn check(
    native: &mut Executor,
    interpreted: &mut Executor,
    unicorn: &mut Executor,
    stop: u32,
    flags_mask: u32,
) {
    assert_eq!(native.run(stop, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        interpreted.run(stop, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        unicorn.run_unicorn(stop, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(native.state.regs, unicorn.state.regs);
    assert_eq!(
        native.state.eflags & flags_mask,
        unicorn.state.eflags & flags_mask
    );
    // Undefined CF/OF/AF follow the interpreter's existing deterministic policy.
    // Fuel measures native blocks versus interpreted instructions.
    interpreted.state.fuel = native.state.fuel;
    assert_eq!(native.state, interpreted.state);
}

#[test]
fn byte_and_word_shifts_match_both_references_for_all_registers_and_counts() {
    let counts = [0, 1, 2, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255];
    let values = [
        0, 1, 2, 3, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x5555, 0xaaaa,
    ];
    let mut cases = 0;
    for destination in 0..16 {
        let bits = if destination < 8 { 8 } else { 16 };
        for operation in 0..3 {
            for encoded in std::iter::once(None).chain(counts.map(Some)) {
                let bytes = shift_code(destination, operation, encoded);
                let stop = CODE + bytes.len() as u32;
                let mut native = machine(&bytes, Some(CompileStrategy::Speed));
                let mut interpreted = machine(&bytes, None);
                let mut unicorn = machine(&bytes, None);
                let dynamic = encoded.map_or(counts.to_vec(), |count| vec![count]);
                for count in dynamic {
                    for value in values {
                        for flags in [0x202, 0xed7] {
                            let state = initial(
                                destination,
                                value,
                                encoded.is_none().then_some(count),
                                flags,
                            );
                            for m in [&mut native, &mut interpreted, &mut unicorn] {
                                m.state = state.clone();
                            }
                            let masked = count & 31;
                            let mut flags_mask = u32::MAX;
                            if masked != 0 {
                                flags_mask &= !0x10; // AF is undefined.
                                if masked != 1 {
                                    flags_mask &= !0x800; // OF is undefined.
                                }
                                if operation != 2 && masked >= bits {
                                    flags_mask &= !1; // Logical shifts do not define CF here.
                                }
                            }
                            check(
                                &mut native,
                                &mut interpreted,
                                &mut unicorn,
                                stop,
                                flags_mask,
                            );
                            cases += 1;
                        }
                    }
                }
                support::x64_shape!(assert_eq!(
                    native.stats.interpreter_entries, 0,
                    "destination={destination}, operation={operation}, count={encoded:?}"
                ));
                support::x64_shape!(assert_eq!(interpreted.stats.native_entries, 0));
            }
        }
    }
    assert_eq!(cases, 32_256);
}

#[test]
fn narrow_shift_flags_are_consumed_inside_native_fast_and_speed_regions() {
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for count in [0u8, 1, 4, 8, 16, 32, 33] {
            let bytes = assemble(|a| {
                // Make every arithmetic flag lazy before the shift. Masked zero
                // must preserve those flags even though it becomes a raw image.
                a.add(ebx, eax).unwrap();
                a.shr(dl, i32::from(count)).unwrap();
                a.setb(byte_ptr(DATA as i32)).unwrap();
                a.setp(byte_ptr(DATA as i32 + 1)).unwrap();
                a.setz(byte_ptr(DATA as i32 + 2)).unwrap();
                a.sets(byte_ptr(DATA as i32 + 3)).unwrap();
                a.seto(byte_ptr(DATA as i32 + 4)).unwrap();
                a.sar(ax, 1).unwrap();
                a.setb(byte_ptr(DATA as i32 + 5)).unwrap();
                a.seto(byte_ptr(DATA as i32 + 6)).unwrap();
            });
            let stop = CODE + bytes.len() as u32;
            let mut native = machine(&bytes, Some(strategy));
            let mut interpreted = machine(&bytes, None);
            let mut unicorn = machine(&bytes, None);
            for value in [0, 1, 0x7f, 0x80, 0xff] {
                let mut state = initial(2, value, None, 0xed7);
                state.regs[0] = 0x1234_8001;
                state.regs[3] = 0x7fff_ffff;
                for m in [&mut native, &mut interpreted, &mut unicorn] {
                    m.state = state.clone();
                    m.write(DATA, &[0; 7]).unwrap();
                }
                // SETcc after a multi-bit shift consumes the interpreter's
                // deterministic OF; compare defined flags with Unicorn, and
                // all flags/output bytes with the interpreter.
                assert_eq!(native.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(interpreted.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(
                    unicorn.run_unicorn(stop, 100).unwrap(),
                    RunOutcome::ReachedStop
                );
                interpreted.state.fuel = native.state.fuel;
                assert_eq!(native.state, interpreted.state);
                assert_eq!(native.state.regs, unicorn.state.regs);
                let actual = native.memory().read(DATA, 7).unwrap();
                assert_eq!(actual, interpreted.memory().read(DATA, 7).unwrap());
                let reference = unicorn.memory().read(DATA, 7).unwrap();
                for n in 0..7 {
                    if n == 4 && !matches!(count & 31, 0 | 1) {
                        continue;
                    }
                    if n == 0 && count & 31 >= 8 {
                        continue;
                    }
                    assert_eq!(actual[n], reference[n], "count={count}, flag byte={n}");
                }
            }
            support::x64_shape!(assert_eq!(native.stats.interpreter_entries, 0));
        }
    }
}

#[test]
fn pal_style_mmx_rotate_and_loop_keeps_narrow_shifts_native() {
    let bytes = assemble(|a| {
        let mut again = a.create_label();
        a.mov(ecx, 4096).unwrap();
        a.movq(mm0, qword_ptr(DATA as i32)).unwrap();
        a.movq(mm1, qword_ptr(DATA as i32 + 8)).unwrap();
        a.set_label(&mut again).unwrap();
        a.rol(ebx, 8).unwrap();
        a.shr(dl, 1).unwrap();
        a.shr(ax, 4).unwrap();
        a.pxor(mm0, mm1).unwrap();
        a.movq(qword_ptr(DATA as i32 + 16), mm0).unwrap();
        a.loop_(again).unwrap();
        a.emms().unwrap();
    });
    let stop = CODE + bytes.len() as u32;
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        let mut native = machine(&bytes, Some(strategy));
        let mut interpreted = machine(&bytes, None);
        let mut unicorn = machine(&bytes, None);
        for m in [&mut native, &mut interpreted, &mut unicorn] {
            m.state = initial(2, 0x81, None, 0x202);
            m.write(DATA, &0x0123_4567_89ab_cdefu64.to_le_bytes())
                .unwrap();
            m.write(DATA + 8, &0xfedc_ba98_7654_3210u64.to_le_bytes())
                .unwrap();
        }
        check(&mut native, &mut interpreted, &mut unicorn, stop, !0x810);
        assert_eq!(native.state.regs[0], 0x89ab_0000);
        assert_eq!(native.state.regs[2], 0xabcd_ef00);
        assert_eq!(
            native.memory().read(DATA + 16, 8).unwrap(),
            interpreted.memory().read(DATA + 16, 8).unwrap()
        );
        assert_eq!(
            native.memory().read(DATA + 16, 8).unwrap(),
            unicorn.memory().read(DATA + 16, 8).unwrap()
        );
        support::x64_shape!(assert_eq!(native.stats.interpreter_entries, 0));
        support::x64_shape!(assert_eq!(native.stats.interpreter_instructions, 0));
    }
}
