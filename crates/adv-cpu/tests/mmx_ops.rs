mod support;
use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn machine(bytes: &[u8], jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn start(m: &mut Executor) {
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m.state.eflags = 0xcd7;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

#[test]
fn packed_mmx_arithmetic_shuffles_and_saturation_match_unicorn() {
    let values = [
        0u64,
        u64::MAX,
        0x0123_4567_89ab_cdef,
        0x8000_7fff_ffff_0001,
        0x00ff_0100_ff00_0080,
        0x8080_8080_7f7f_7f7f,
    ];
    for operation in 0..29 {
        let bytes = code(|a| {
            a.movq(mm0, qword_ptr(DATA as i32)).unwrap();
            a.movq(mm1, qword_ptr(DATA as i32 + 8)).unwrap();
            match operation {
                0 => a.pxor(mm0, mm1),
                1 => a.pand(mm0, mm1),
                2 => a.pandn(mm0, mm1),
                3 => a.por(mm0, mm1),
                4 => a.paddb(mm0, mm1),
                5 => a.paddw(mm0, mm1),
                6 => a.paddd(mm0, mm1),
                7 => a.psubb(mm0, mm1),
                8 => a.psubw(mm0, mm1),
                9 => a.psubd(mm0, mm1),
                10 => a.paddusb(mm0, mm1),
                11 => a.paddusw(mm0, mm1),
                12 => a.psubusb(mm0, mm1),
                13 => a.psubusw(mm0, mm1),
                14 => a.pmullw(mm0, mm1),
                15 => a.pmulhuw(mm0, mm1),
                16 => a.packuswb(mm0, mm1),
                17 => a.packsswb(mm0, mm1),
                18 => a.punpcklbw(mm0, mm1),
                19 => a.punpckhbw(mm0, mm1),
                20 => a.punpcklwd(mm0, mm1),
                21 => a.punpckhwd(mm0, mm1),
                22 => a.punpckldq(mm0, mm1),
                23 => a.punpckhdq(mm0, mm1),
                24 => a.paddb(mm0, qword_ptr(DATA as i32 + 8)),
                25 => a.pand(mm0, qword_ptr(DATA as i32 + 8)),
                26 => a.pmullw(mm0, qword_ptr(DATA as i32 + 8)),
                27 => a.pavgb(mm0, mm1),
                _ => a.pavgw(mm0, mm1),
            }
            .unwrap();
            a.movq(qword_ptr(DATA as i32 + 32), mm0).unwrap();
            a.emms().unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&bytes, true);
        let mut uc = machine(&bytes, false);
        for &left in &values {
            for &right in &values {
                for m in [&mut jit, &mut uc] {
                    start(m);
                    m.write(DATA, &left.to_le_bytes()).unwrap();
                    m.write(DATA + 8, &right.to_le_bytes()).unwrap();
                }
                assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(
                    uc.run_unicorn(STOP, 10000).unwrap(),
                    RunOutcome::ReachedStop
                );
                assert_eq!(
                    jit.memory().read(DATA + 32, 8).unwrap(),
                    uc.memory().read(DATA + 32, 8).unwrap(),
                    "operation {operation}, left {left:x}, right {right:x}"
                );
                assert_eq!(jit.state.mmx, uc.state.mmx, "operation {operation}");
                assert_eq!(jit.state.eflags, uc.state.eflags);
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0, "operation {operation}");
    }
}

#[test]
fn mmx_shift_counts_at_and_above_lane_width_zero_the_result() {
    for operation in 0..4 {
        for count in [0, 1, 7, 15, 16, 31, 32, 63, 255] {
            let bytes = code(|a| {
                a.movq(mm0, qword_ptr(DATA as i32)).unwrap();
                match operation {
                    0 => a.psrlw(mm0, count),
                    1 => a.psllw(mm0, count),
                    2 => a.psrld(mm0, count),
                    _ => a.pslld(mm0, count),
                }
                .unwrap();
                a.movq(qword_ptr(DATA as i32 + 32), mm0).unwrap();
                a.ret().unwrap();
            });
            let mut jit = machine(&bytes, true);
            let mut uc = machine(&bytes, false);
            for m in [&mut jit, &mut uc] {
                start(m);
                m.write(DATA, &0x1234_5678_9abc_def0u64.to_le_bytes())
                    .unwrap();
            }
            assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(
                uc.run_unicorn(STOP, 10000).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(
                jit.memory().read(DATA + 32, 8).unwrap(),
                uc.memory().read(DATA + 32, 8).unwrap()
            );
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn movd_and_emms_preserve_x87_state_across_native_interpreter_boundaries() {
    let bytes = code(|a| {
        a.fld1().unwrap();
        a.movd(mm3, ebx).unwrap();
        a.movq(qword_ptr(DATA as i32 + 32), mm3).unwrap();
        a.movd(edx, mm3).unwrap();
        a.movd(mm4, dword_ptr(DATA as i32 + 8)).unwrap();
        a.movd(dword_ptr(DATA as i32 + 40), mm4).unwrap();
        a.emms().unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(DATA as i32 + 48)).unwrap();
        a.fnstsw(ax).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, true);
    let mut uc = machine(&bytes, false);
    for m in [&mut jit, &mut uc] {
        start(m);
        m.state.regs[3] = 0x89ab_cdef;
        m.write(DATA + 8, &0x7654_3210u32.to_le_bytes()).unwrap();
    }
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(
        jit.memory().read(DATA + 32, 24).unwrap(),
        uc.memory().read(DATA + 32, 24).unwrap()
    );
    assert_eq!(jit.state.mmx, uc.state.mmx);
    // Save the full underlying engine state and make x87 tag/TOP changes visible.
    let _ = jit.save_context().unwrap();
    let _ = uc.save_context().unwrap();
}

#[test]
fn mmx_store_fault_checks_both_pages_before_writing() {
    let bytes = code(|a| {
        a.movq(mm0, qword_ptr(DATA as i32)).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, true);
    jit.protect(DATA + 4096, 4096, READ).unwrap();
    start(&mut jit);
    jit.state.regs[7] = DATA + 4092;
    jit.write(DATA, &u64::MAX.to_le_bytes()).unwrap();
    assert!(
        matches!(jit.run(STOP,100).unwrap(),RunOutcome::WriteFault{address,..}if address==DATA+4092)
    );
    assert_eq!(jit.memory().read(DATA + 4092, 8).unwrap(), [0; 8]);
}

#[test]
fn mmx_values_survive_native_region_dispatch_and_x87_emms_boundary() {
    const HELPER: u32 = 0x13000;
    let bytes = code(|a| {
        a.movq(mm2, qword_ptr(DATA as i32)).unwrap();
        a.call(u64::from(HELPER)).unwrap();
        a.movq(qword_ptr(DATA as i32 + 32), mm2).unwrap();
        a.emms().unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(DATA as i32 + 48)).unwrap();
        a.ret().unwrap();
    });
    let helper = {
        let mut a = CodeAssembler::new(32).unwrap();
        a.paddb(mm2, mm2).unwrap();
        a.ret().unwrap();
        a.assemble(u64::from(HELPER)).unwrap()
    };
    let mut jit = machine(&bytes, true);
    let mut uc = machine(&bytes, false);
    for m in [&mut jit, &mut uc] {
        m.write(HELPER, &helper).unwrap();
        m.protect(HELPER, 4096, READ | EXEC).unwrap();
        start(m);
        m.write(DATA, &0x0123_4567_89ab_cdefu64.to_le_bytes())
            .unwrap();
    }
    jit.add_region(Region::new(HELPER, helper.len())).unwrap();
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        jit.memory().read(DATA + 32, 24).unwrap(),
        uc.memory().read(DATA + 32, 24).unwrap()
    );
    assert_eq!(jit.state.mmx, uc.state.mmx);
    assert!(jit.stats.native_entries >= 3);
}
