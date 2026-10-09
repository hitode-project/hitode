//! Independent hardware oracle: neither the interpreter nor floating::fast
//! computes these expected results. Save and restore the complete host FP image
//! within one asm block, including the caller's x87 stack and MXCSR.
use super::*;

#[repr(C, align(16))]
struct HostFp([u8; 512]);

fn hardware(a: f64, b: f64, op: Op, control: u16) -> ([u8; 10], u16) {
    // Match a pre-seeded canonical ST0. Loading m64 here would additionally
    // raise DE for a binary64 subnormal (which is a normal extended value).
    let a_raw = crate::backend::x87::encode_f64(a);
    let mut saved = HostFp([0; 512]);
    let mut raw = [0u8; 10];
    let mut status = 0u16;
    macro_rules! execute {
        ($instruction:literal) => {
            // SAFETY: every operand points to a live, correctly sized object;
            // FXSAVE's image is 16-byte aligned. All exceptions are masked.
            // FXRSTOR restores the original x87/MMX/XMM/MXCSR image before Rust
            // resumes; no stack pointer or general-purpose register is changed.
            unsafe {
                core::arch::asm!(
                    "fxsave64 [{saved}]",
                    "fninit",
                    "fldcw word ptr [{control}]",
                    "fld tbyte ptr [{a}]",
                    $instruction,
                    "fwait",
                    "fnstsw word ptr [{status}]",
                    "fstp tbyte ptr [{raw}]",
                    "fxrstor64 [{saved}]",
                    saved = in(reg) &mut saved,
                    control = in(reg) &control,
                    a = in(reg) &a_raw,
                    b = in(reg) &b,
                    status = in(reg) &mut status,
                    raw = in(reg) &mut raw,
                    options(nostack),
                );
            }
        };
    }
    match op {
        Op::Add => execute!("fadd qword ptr [{b}]"),
        Op::Sub => execute!("fsub qword ptr [{b}]"),
        Op::Mul => execute!("fmul qword ptr [{b}]"),
        Op::Div => execute!("fdiv qword ptr [{b}]"),
    }
    (raw, status)
}

struct DekkerGuard(bool);
impl DekkerGuard {
    fn set(value: bool) -> Self {
        Self(testing::FORCE_DEKKER.replace(value))
    }
}
impl Drop for DekkerGuard {
    fn drop(&mut self) {
        testing::FORCE_DEKKER.set(self.0);
    }
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
fn bounded(seed: &mut u64) -> f64 {
    let bits = next(seed);
    let exponent = ((bits >> 52) % 801) + (1023 - 400);
    f64::from_bits((bits & 0x800f_ffff_ffff_ffff) | (exponent << 52))
}
/// A bounded value with a 24-bit significand, as PC=24 code keeps them.
fn single(seed: &mut u64) -> f64 {
    f64::from_bits(bounded(seed).to_bits() & !crate::floating::fast::SINGLE_TAIL)
}

#[test]
fn arithmetic_then_wait_matches_hardware_across_controls_and_special_values() {
    let values = [
        (1.0, 3.0),
        (1.0, 2f64.powi(-63)),
        (-1.0, 1.0),
        (-0.0, -0.0),
        (f64::from_bits(1), 2.0),
        (f64::MIN_POSITIVE, f64::MIN_POSITIVE),
        (f64::MAX, f64::MAX),
        (f64::INFINITY, f64::NEG_INFINITY),
        (f64::from_bits(0x7ff8_0000_0000_1234), 1.0),
        (f64::from_bits(0x7ff0_0000_0000_1234), 1.0),
    ];
    for op in [Op::Add, Op::Sub, Op::Mul, Op::Div] {
        let code = bytes(|a| {
            match op {
                Op::Add => a.fadd(qword_ptr(esi)),
                Op::Sub => a.fsub(qword_ptr(esi)),
                Op::Mul => a.fmul(qword_ptr(esi)),
                Op::Div => a.fdiv(qword_ptr(esi)),
            }
            .unwrap();
            a.wait().unwrap();
        });
        let mut mem = memory(&code);
        let compiled = compile(&Region::new(CODE, code.len()), &mut mem).unwrap();
        for pc in [0, 2, 3] {
            for rc in 0..4 {
                let control = 0x7f | pc << 8 | rc << 10;
                for (a, b) in values {
                    let (raw, status) = hardware(a, b, op, control);
                    let mut actual = state();
                    actual.x87.control = control;
                    let slot = actual.x87.top();
                    let value = X87Register(crate::backend::x87::encode_f64(a));
                    actual.x87.regs[slot] = value;
                    actual.x87.set_physical_tag(slot, value.tag());
                    actual.refresh_mmx();
                    mem.write(DATA, &b.to_le_bytes()).unwrap();
                    assert_eq!(
                        testing::run(&compiled, &mut actual, &mut mem),
                        Exit::Dispatch as u32
                    );
                    assert_eq!(
                        actual.x87.regs[slot].0, raw,
                        "{op:?} PC={pc} RC={rc} a={a:?} b={b:?}"
                    );
                    assert_eq!(
                        actual.x87.status & 0x023f,
                        status & 0x023f,
                        "{op:?} PC={pc} RC={rc} a={a:?} b={b:?}"
                    );
                    assert_eq!(actual.eflags, state().eflags);
                }
            }
        }
    }
}

#[test]
fn native_arithmetic_matches_hardware_raw80_and_precision_flags() {
    let edge = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        1.5,
        -1.5,
        3.0,
        -3.0,
        1.0 + f64::EPSILON,
        1.0 - f64::EPSILON / 2.0,
        2f64.powi(-400),
        -2f64.powi(-400),
        2f64.powi(400),
        -2f64.powi(400),
        2f64.powi(-53),
        -2f64.powi(-53),
    ];
    let mut count = 0usize;
    let before = testing::calls();
    for dekker in [false, true] {
        let _guard = DekkerGuard::set(dekker);
        for op in [Op::Add, Op::Sub, Op::Mul, Op::Div] {
            let code = bytes(|a| {
                match op {
                    Op::Add => a.fadd(qword_ptr(esi)),
                    Op::Sub => a.fsub(qword_ptr(esi)),
                    Op::Mul => a.fmul(qword_ptr(esi)),
                    Op::Div => a.fdiv(qword_ptr(esi)),
                }
                .unwrap();
            });
            let mut mem = memory(&code);
            let compiled = compile(&Region::new(CODE, code.len()), &mut mem).unwrap();
            let mut seed = 0xd93f_517a_982b_410du64;
            let random = (0..8192).map(|_| (bounded(&mut seed), bounded(&mut seed)));
            let edges = edge.iter().flat_map(|&a| edge.iter().map(move |&b| (a, b)));
            let mut exact = 0;
            let mut down = 0;
            let mut up = 0;
            for (a, b) in edges.chain(random) {
                if op == Op::Div && b == 0.0 {
                    continue; // Divide-by-zero rejection is tested separately.
                }
                let (raw, status) = hardware(a, b, op, 0x027f);
                let mut actual = state();
                actual.x87.control = 0x027f;
                let slot = actual.x87.top();
                let value = X87Register(crate::backend::x87::encode_f64(a));
                actual.x87.regs[slot] = value;
                actual.x87.set_physical_tag(slot, value.tag());
                actual.refresh_mmx();
                mem.write(DATA, &b.to_le_bytes()).unwrap();
                assert_eq!(
                    testing::run(&compiled, &mut actual, &mut mem),
                    Exit::Dispatch as u32
                );
                assert_eq!(
                    actual.x87.regs[slot].0,
                    raw,
                    "raw80 op={op:?} dekker={dekker} a={:016x} b={:016x}",
                    a.to_bits(),
                    b.to_bits()
                );
                assert_eq!(
                    actual.x87.status & 0x023f,
                    status & 0x023f,
                    "exceptions/C1 op={op:?} dekker={dekker} a={:016x} b={:016x}",
                    a.to_bits(),
                    b.to_bits()
                );
                if status & 0x20 == 0 {
                    exact += 1;
                } else if status & 0x200 == 0 {
                    down += 1;
                } else {
                    up += 1;
                }
                count += 1;
            }
            assert!(exact > 0 && down > 0 && up > 0, "oracle coverage: {op:?}");
        }
    }
    assert_eq!(
        testing::calls() - before,
        0,
        "hardware corpus must exercise native emission"
    );
    assert_eq!(count, 67520);
    eprintln!("hardware x87 oracle: {count} cases, raw80 + exception/C1 bits, zero helper calls");
}

#[test]
fn single_precision_arithmetic_matches_hardware_raw80_and_precision_flags() {
    // 24-bit operands across the bounded exponent range: PC=24 keeps the
    // extended exponent, so products and quotients leave the binary32 range.
    let edge = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        1.5,
        3.0,
        -3.0,
        1.0 + 2f64.powi(-23),
        1.0 - 2f64.powi(-24),
        2f64.powi(-24),
        -2f64.powi(-25),
        16_777_215.0,
        2f64.powi(-400),
        -2f64.powi(400),
        f64::from(0.1f32),
        f64::from(-1.0f32 / 3.0),
    ];
    let mut count = 0usize;
    let before = testing::calls();
    for dekker in [false, true] {
        let _guard = DekkerGuard::set(dekker);
        for op in [Op::Add, Op::Sub, Op::Mul, Op::Div] {
            let code = bytes(|a| {
                match op {
                    Op::Add => a.fadd(qword_ptr(esi)),
                    Op::Sub => a.fsub(qword_ptr(esi)),
                    Op::Mul => a.fmul(qword_ptr(esi)),
                    Op::Div => a.fdiv(qword_ptr(esi)),
                }
                .unwrap();
            });
            let mut mem = memory(&code);
            let compiled = compile(&Region::new(CODE, code.len()), &mut mem).unwrap();
            let mut seed = 0x6a09_e667_f3bc_c909u64;
            // Nearby exponents make sums with exact, halfway and carrying tails.
            let random = (0..8192).map(|n| {
                let a = single(&mut seed);
                let b = if n % 2 == 0 {
                    single(&mut seed)
                } else {
                    let shift = (next(&mut seed) % 40) as i32 - 20;
                    a * 2f64.powi(shift) * (1.0 + f64::from(next(&mut seed) as u16) / 65536.0)
                };
                let b = f64::from_bits(b.to_bits() & !crate::floating::fast::SINGLE_TAIL);
                // Out-of-range operands take the helper, as at PC=53.
                let exponent = ((b.to_bits() >> 52) & 0x7ff) as i32 - 1023;
                (a, if exponent.abs() <= 400 { b } else { a })
            });
            let edges = edge.iter().flat_map(|&a| edge.iter().map(move |&b| (a, b)));
            let (mut exact, mut down, mut up) = (0, 0, 0);
            for (a, b) in edges.chain(random) {
                if op == Op::Div && b == 0.0 {
                    continue;
                }
                let (raw, status) = hardware(a, b, op, 0x007f);
                let mut actual = state();
                actual.x87.control = 0x007f;
                let slot = actual.x87.top();
                let value = X87Register(crate::backend::x87::encode_f64(a));
                actual.x87.regs[slot] = value;
                actual.x87.set_physical_tag(slot, value.tag());
                actual.refresh_mmx();
                mem.write(DATA, &b.to_le_bytes()).unwrap();
                assert_eq!(
                    testing::run(&compiled, &mut actual, &mut mem),
                    Exit::Dispatch as u32
                );
                assert_eq!(
                    actual.x87.regs[slot].0,
                    raw,
                    "raw80 op={op:?} dekker={dekker} a={:016x} b={:016x}",
                    a.to_bits(),
                    b.to_bits()
                );
                assert_eq!(
                    actual.x87.status & 0x023f,
                    status & 0x023f,
                    "exceptions/C1 op={op:?} dekker={dekker} a={:016x} b={:016x}",
                    a.to_bits(),
                    b.to_bits()
                );
                if status & 0x20 == 0 {
                    exact += 1;
                } else if status & 0x200 == 0 {
                    down += 1;
                } else {
                    up += 1;
                }
                count += 1;
            }
            assert!(exact > 0 && down > 0 && up > 0, "oracle coverage: {op:?}");
        }
    }
    assert_eq!(
        testing::calls() - before,
        0,
        "PC=24 corpus must exercise native emission"
    );
    eprintln!("PC=24 hardware x87 oracle: {count} cases, zero helper calls");
}
