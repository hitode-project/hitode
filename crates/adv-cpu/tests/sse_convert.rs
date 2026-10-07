mod support;

use adv_cpu::{CpuState, EXEC, READ, Region, WRITE, X87Register};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const DATA: u32 = 0x20000;

#[derive(Clone, Copy, Debug)]
enum Conversion {
    PsPi,
    TpsPi,
    PiPs,
    PsDq,
    TpsDq,
    DqPs,
    SsSi,
    TssSi,
    SdSi,
    TsdSi,
    SiSs,
    SiSd,
    PdDq,
    TpdDq,
}
use Conversion::*;

impl Conversion {
    fn width(self) -> usize {
        match self {
            PsPi | TpsPi | PiPs | SdSi | TsdSi => 8,
            SsSi | TssSi | SiSs | SiSd => 4,
            _ => 16,
        }
    }

    fn emit(self, a: &mut CodeAssembler, memory: bool) {
        match (self, memory) {
            (PsPi, false) => a.cvtps2pi(mm0, xmm1),
            (PsPi, true) => a.cvtps2pi(mm0, qword_ptr(edx)),
            (TpsPi, false) => a.cvttps2pi(mm0, xmm1),
            (TpsPi, true) => a.cvttps2pi(mm0, qword_ptr(edx)),
            (PiPs, false) => a.cvtpi2ps(xmm0, mm1),
            (PiPs, true) => a.cvtpi2ps(xmm0, qword_ptr(edx)),
            (PsDq, false) => a.cvtps2dq(xmm0, xmm1),
            (PsDq, true) => a.cvtps2dq(xmm0, xmmword_ptr(edx)),
            (TpsDq, false) => a.cvttps2dq(xmm0, xmm1),
            (TpsDq, true) => a.cvttps2dq(xmm0, xmmword_ptr(edx)),
            (DqPs, false) => a.cvtdq2ps(xmm0, xmm1),
            (DqPs, true) => a.cvtdq2ps(xmm0, xmmword_ptr(edx)),
            (SsSi, false) => a.cvtss2si(eax, xmm1),
            (SsSi, true) => a.cvtss2si(eax, dword_ptr(edx)),
            (TssSi, false) => a.cvttss2si(eax, xmm1),
            (TssSi, true) => a.cvttss2si(eax, dword_ptr(edx)),
            (SdSi, false) => a.cvtsd2si(eax, xmm1),
            (SdSi, true) => a.cvtsd2si(eax, qword_ptr(edx)),
            (TsdSi, false) => a.cvttsd2si(eax, xmm1),
            (TsdSi, true) => a.cvttsd2si(eax, qword_ptr(edx)),
            (SiSs, false) => a.cvtsi2ss(xmm0, ecx),
            (SiSs, true) => a.cvtsi2ss(xmm0, dword_ptr(edx)),
            (SiSd, false) => a.cvtsi2sd(xmm0, ecx),
            (SiSd, true) => a.cvtsi2sd(xmm0, dword_ptr(edx)),
            (PdDq, false) => a.cvtpd2dq(xmm0, xmm1),
            (PdDq, true) => a.cvtpd2dq(xmm0, xmmword_ptr(edx)),
            (TpdDq, false) => a.cvttpd2dq(xmm0, xmm1),
            (TpdDq, true) => a.cvttpd2dq(xmm0, xmmword_ptr(edx)),
        }
        .unwrap();
    }
}

fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.jmp(STOP as u64).unwrap();
    a.assemble(CODE as u64).unwrap()
}

fn machines(code: &[u8]) -> [Executor; 3] {
    std::array::from_fn(|index| {
        let mut m = Executor::new(CODE, 0x20000).unwrap();
        m.protect(CODE, 0x20000, READ | WRITE).unwrap();
        m.write(CODE, code).unwrap();
        m.protect(CODE, 0x2000, READ | EXEC).unwrap();
        if index == 0 {
            m.set_region(Region::new(CODE, code.len())).unwrap();
        }
        m
    })
}

fn initial(source: [u8; 16], mxcsr: u32) -> CpuState {
    let mut s = CpuState {
        eip: CODE,
        eflags: 0x8d7,
        mxcsr,
        ..CpuState::default()
    };
    s.regs[0] = 0x12345678;
    s.regs[1] = u32::from_le_bytes(source[..4].try_into().unwrap());
    s.regs[2] = DATA;
    s.xmm[0] = [0xa5; 16];
    s.xmm[1] = source;
    s.x87.status = 5 << 11 | 0x4100;
    s.x87.tag = 0xaaaa;
    for slot in 0..8 {
        s.mmx[slot] = if slot == 1 {
            u64::from_le_bytes(source[..8].try_into().unwrap())
        } else {
            0x1234567812345678 + slot as u64
        };
        s.x87.regs[slot] = X87Register::from_parts(s.mmx[slot], 0xffff);
    }
    s
}

fn compare(m: &mut [Executor; 3], state: CpuState, source: [u8; 16], unicorn: bool) {
    for machine in m.iter_mut() {
        machine.state = state.clone();
        machine.write(DATA, &source).unwrap();
    }
    let native = m[0].run(STOP, 1000).unwrap();
    let interpreted = m[1].run(STOP, 1000).unwrap();
    assert_eq!(native, interpreted);
    let mut native_state = m[0].state.clone();
    // Native fuel is charged by blocks, while the interpreter charges each
    // instruction. It is scheduling metadata, not architectural state.
    native_state.fuel = m[1].state.fuel;
    assert_eq!(native_state, m[1].state, "complete interpreter state");
    if unicorn {
        assert_eq!(m[2].run_unicorn(STOP, 1000).unwrap(), native);
        assert_eq!(m[0].state.regs, m[2].state.regs);
        assert_eq!(m[0].state.eflags & 0xcd5, m[2].state.eflags & 0xcd5);
        assert_eq!(m[0].state.xmm, m[2].state.xmm);
        assert_eq!(m[0].state.mmx, m[2].state.mmx);
        // Unicorn omits conversion exception status, including precision.
        // Full MXCSR is checked above against the exact interpreter.
        assert_eq!(m[0].state.mxcsr & !0x3f, m[2].state.mxcsr & !0x3f);
    }
}

fn words(values: [u32; 4]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.chunks_exact_mut(4).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn doubles(values: [u64; 2]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.chunks_exact_mut(8).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn float_to_integer_all_rc_specials_and_random_match_interpreter_and_unicorn() {
    let singles = [
        0,
        0x80000000,
        1,
        0x80000001,
        0x007fffff,
        0x00800000,
        0x7fc12345,
        0x7f812345,
        0xffc12345,
        0x7f800000,
        0xff800000,
        0x4effffff,
        0x4f000000,
        0xcf000000,
        0xcf000001,
        0.5f32.to_bits(),
        (-0.5f32).to_bits(),
        1.5f32.to_bits(),
        (-2.5f32).to_bits(),
    ];
    let wide = [
        0,
        1 << 63,
        1,
        (1 << 63) | 1,
        0x000fffffffffffff,
        0x7ff8000012345678,
        0x7ff0000012345678,
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        2147483647.5f64.to_bits(),
        2147483648f64.to_bits(),
        2147483648f64.to_bits() - 1,
        2147483648f64.to_bits() + 1,
        (-2147483648f64).to_bits(),
        (-2147483648f64).to_bits() - 1,
        (-2147483648f64).to_bits() + 1,
        (-2147483648.5f64).to_bits(),
        (-2147483649f64).to_bits(),
        0.5f64.to_bits(),
        (-0.5f64).to_bits(),
        1.5f64.to_bits(),
        (-2.5f64).to_bits(),
    ];
    for kind in [
        PsPi, TpsPi, PsDq, TpsDq, SsSi, TssSi, SdSi, TsdSi, PdDq, TpdDq,
    ] {
        for memory in [false, true] {
            let mut m = machines(&assemble(|a| kind.emit(a, memory)));
            let mut seed = 0x3141592653589793;
            for rc in 0..4 {
                for index in 0..276 {
                    let source = if matches!(kind, SdSi | TsdSi | PdDq | TpdDq) {
                        if index < wide.len() {
                            doubles([wide[index], wide[(index + 5) % wide.len()]])
                        } else {
                            doubles([next(&mut seed), next(&mut seed)])
                        }
                    } else if index < singles.len() {
                        words(std::array::from_fn(|lane| {
                            singles[(index + lane * 5) % singles.len()]
                        }))
                    } else {
                        words(std::array::from_fn(|_| next(&mut seed) as u32))
                    };
                    compare(
                        &mut m,
                        initial(source, 0x1f80 | (rc << 13) | (index as u32 & 0x3f)),
                        source,
                        true,
                    );
                }
            }
        }
    }
}

#[test]
fn integer_to_float_all_rc_exact_inexact_and_random_preserve_upper_lanes() {
    let cases = [
        0,
        1,
        u32::MAX,
        i32::MIN as u32,
        i32::MAX as u32,
        16777215,
        16777216,
        16777217,
        (-16777217i32) as u32,
    ];
    for kind in [PiPs, DqPs, SiSs, SiSd] {
        for memory in [false, true] {
            let mut m = machines(&assemble(|a| kind.emit(a, memory)));
            let mut seed = 0x123456789abcdef;
            for rc in 0..4 {
                for index in 0..265 {
                    let source = words(std::array::from_fn(|lane| {
                        if index < cases.len() {
                            cases[(index + lane) % cases.len()]
                        } else {
                            next(&mut seed) as u32
                        }
                    }));
                    compare(&mut m, initial(source, 0x1f80 | (rc << 13)), source, true);
                }
            }
        }
    }
}

#[test]
fn conversion_daz_ftz_unmasked_and_sticky_flags_use_exact_semantics() {
    for kind in [
        PsPi, TpsPi, PiPs, PsDq, TpsDq, DqPs, SsSi, TssSi, SdSi, TsdSi, SiSs, SiSd, PdDq, TpdDq,
    ] {
        let mut m = machines(&assemble(|a| kind.emit(a, false)));
        for controls in [0x1fc0, 0x9f80, 0x9fc0, 0, 0x1f00, 0x0f80, 0x1e80] {
            for rc in 0..4 {
                for source in [
                    words([1, 0x80000001, 0, 0]),
                    words([0x7fc00000, 1.5f32.to_bits(), 0, 0]),
                    words([16777217, i32::MAX as u32, 0, 0]),
                    doubles([1.5f64.to_bits(), f64::NAN.to_bits()]),
                ] {
                    compare(
                        &mut m,
                        initial(source, controls | (rc << 13) | 0x15),
                        source,
                        false,
                    );
                }
            }
        }
    }
}

#[test]
fn conversion_memory_faults_preserve_full_state_and_prior_prefix() {
    for kind in [
        PsPi, TpsPi, PiPs, PsDq, TpsDq, DqPs, SsSi, TssSi, SdSi, TsdSi, SiSs, SiSd, PdDq, TpdDq,
    ] {
        let code = assemble(|a| {
            a.mov(ebx, 42).unwrap();
            kind.emit(a, true);
        });
        for crossing in [false, true] {
            let mut m = machines(&code);
            let source = words([1.5f32.to_bits(); 4]);
            let mut state = initial(source, 0x1f80);
            state.regs[2] = DATA + 4096 - if crossing { kind.width() as u32 / 2 } else { 0 };
            for machine in &mut m {
                machine.protect(DATA + 4096, 4096, 0).unwrap();
            }
            compare(&mut m, state.clone(), source, false);
            assert_eq!(m[0].state.regs[3], 42);
            assert_eq!(m[0].state.mxcsr, state.mxcsr);
            assert_eq!(m[0].state.xmm, state.xmm);
            assert_eq!(m[0].state.x87, state.x87);
            assert_eq!(m[0].state.mmx, state.mmx);
        }
    }
}

#[test]
fn mmx_conversion_pending_exception_precedes_source_fault_and_keeps_aliases() {
    for kind in [PsPi, TpsPi, PiPs] {
        let mut m = machines(&assemble(|a| kind.emit(a, true)));
        let source = words([1f32.to_bits(); 4]);
        let mut state = initial(source, 0x1f80);
        state.x87.status |= 0x8081;
        state.regs[2] = DATA + 4096;
        for machine in &mut m {
            machine.protect(DATA + 4096, 4096, 0).unwrap();
        }
        compare(&mut m, state.clone(), source, false);
        assert_eq!(m[0].state.eip, CODE);
        assert_eq!(m[0].state.x87, state.x87);
    }
}

#[test]
fn common_conversions_execute_natively_and_publish_mmx_state() {
    for kind in [
        PsPi, TpsPi, PiPs, PsDq, TpsDq, DqPs, SsSi, TssSi, SdSi, TsdSi, SiSs, SiSd,
    ] {
        let mut m = machines(&assemble(|a| kind.emit(a, false)));
        let source = match kind {
            PiPs | DqPs | SiSs | SiSd => words([1, 2, 3, 4]),
            SdSi | TsdSi => doubles([1.5f64.to_bits(), 0]),
            _ => words([1.5f32.to_bits(), (-2.5f32).to_bits(), 0, 0]),
        };
        for rc in 0..4 {
            compare(&mut m, initial(source, 0x1f80 | (rc << 13)), source, true);
        }
        if matches!(kind, PsPi | TpsPi | PiPs) {
            assert_eq!(m[0].state.x87.top(), 0);
            assert_eq!(m[0].state.x87.tag, 0);
        }
        if std::env::var_os("ADV_JIT_VERIFY").is_none() {
            assert_eq!(m[0].stats.interpreter_instructions, 0, "{kind:?}");
            assert!(m[0].stats.native_entries > 0);
        }
    }
}
