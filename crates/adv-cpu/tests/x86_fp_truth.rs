//! Interpreter results against values measured on an Intel Core Ultra 5 230F.
//!
//! Each case runs the instructions the hardware probe ran and compares every
//! bit the probe recorded: packed SSE `#XM` flag commitment and destination
//! suppression (SDM Vol. 1 11.5.3.2), x87 status words after loads of special
//! encodings, and x87 NaN operand selection.
use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Memory, READ, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const OUT: u32 = 0x20100;

fn memory() -> Memory {
    let mut m = Memory::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m
}

/// Runs `f`'s instructions one step at a time; returns the step that stopped
/// execution before the end, if any.
fn run(f: impl FnOnce(&mut CodeAssembler), s: &mut CpuState, m: &mut Memory) -> Option<Step> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    let count = a.instructions().len();
    let bytes = a.assemble(u64::from(CODE)).unwrap();
    m.write(CODE, &bytes).unwrap();
    s.eip = CODE;
    let mut interpreter = Interpreter::new();
    for _ in 0..count {
        match interpreter.step(s, m) {
            Step::Executed => {}
            other => return Some(other),
        }
    }
    assert_eq!(s.eip, CODE + bytes.len() as u32);
    None
}

/// Records `actual != expected` under `label` instead of stopping.
fn check<T: PartialEq + std::fmt::Debug>(
    bad: &mut Vec<String>,
    label: String,
    actual: T,
    expected: T,
) {
    if actual != expected {
        bad.push(format!("{label}: got {actual:x?}, Intel {expected:x?}"));
    }
}

fn report(bad: Vec<String>) {
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

fn lanes(v: [u32; 4]) -> [u8; 16] {
    let mut out = [0; 16];
    for (chunk, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(v) {
        *chunk = x.to_le_bytes();
    }
    out
}

const QNAN: u32 = 0x7fc0_0000;
const SNAN: u32 = 0x7f80_0001;
const ONE: u32 = 0x3f80_0000;
const ONEP: u32 = 0x3fc0_0000;
const TINY: u32 = 0x3200_0000;

#[derive(Clone, Copy)]
enum Sse {
    Cvtps2dq,
    Cvttps2dq,
    Cvtpd2dq,
    Addps,
    Mulps,
}

// spec: A64F-FP-5
#[test]
fn packed_sse_xm_commits_only_pre_computation_flags_when_one_is_unmasked() {
    // (name, op, mxcsr in, xmm0, xmm1, Some(MXCSR at #XM) or None, MXCSR and xmm0 after)
    #[allow(clippy::type_complexity)]
    let cases: [(
        &str,
        Sse,
        u32,
        [u32; 4],
        [u32; 4],
        Option<u32>,
        u32,
        [u32; 4],
    ); 12] = [
        (
            "cvtps2dq IM0",
            Sse::Cvtps2dq,
            0x1f00,
            [9; 4],
            [ONEP, QNAN, ONE, ONE],
            Some(0x1f01),
            0,
            [9; 4],
        ),
        (
            "cvtps2dq PM0",
            Sse::Cvtps2dq,
            0x0f80,
            [9; 4],
            [ONEP, QNAN, ONE, ONE],
            Some(0x0fa1),
            0,
            [9; 4],
        ),
        (
            "cvtps2dq IM0 PM0",
            Sse::Cvtps2dq,
            0x0f00,
            [9; 4],
            [ONEP, QNAN, ONE, ONE],
            Some(0x0f01),
            0,
            [9; 4],
        ),
        (
            "cvttps2dq IM0",
            Sse::Cvttps2dq,
            0x1f00,
            [9; 4],
            [ONEP, QNAN, ONE, ONE],
            Some(0x1f01),
            0,
            [9; 4],
        ),
        (
            "addps inexact,SNaN IM0",
            Sse::Addps,
            0x1f00,
            [ONE; 4],
            [TINY, SNAN, ONE, ONE],
            Some(0x1f01),
            0,
            [ONE; 4],
        ),
        (
            "addps inexact,SNaN PM0",
            Sse::Addps,
            0x0f80,
            [ONE; 4],
            [TINY, SNAN, ONE, ONE],
            Some(0x0fa1),
            0,
            [ONE; 4],
        ),
        (
            "addps SNaN,inexact IM0",
            Sse::Addps,
            0x1f00,
            [ONE; 4],
            [SNAN, TINY, ONE, ONE],
            Some(0x1f01),
            0,
            [ONE; 4],
        ),
        (
            "mulps ovf,SNaN OM0",
            Sse::Mulps,
            0x1b80,
            [0x7f00_0000, ONE, ONE, ONE],
            [0x7f00_0000, SNAN, ONE, ONE],
            Some(0x1b89),
            0,
            [0x7f00_0000, ONE, ONE, ONE],
        ),
        (
            "addps denorm,inexact DM0",
            Sse::Addps,
            0x1e80,
            [ONE; 4],
            [0x0000_0001, TINY, ONE, ONE],
            Some(0x1e82),
            0,
            [ONE; 4],
        ),
        (
            "cvtpd2dq IM0",
            Sse::Cvtpd2dq,
            0x1f00,
            [9; 4],
            [0, 0x3ff8_0000, 0, 0x7ff8_0000],
            Some(0x1f01),
            0,
            [9; 4],
        ),
        // Fully masked: the instruction completes.
        (
            "addps inexact,SNaN masked",
            Sse::Addps,
            0x1f80,
            [ONE; 4],
            [TINY, SNAN, ONE, ONE],
            None,
            0x1fa1,
            [ONE, 0x7fc0_0001, 0x4000_0000, 0x4000_0000],
        ),
        (
            "cvtps2dq masked",
            Sse::Cvtps2dq,
            0x1f80,
            [9; 4],
            [ONEP, QNAN, ONE, ONE],
            None,
            0x1fa1,
            [2, 0x8000_0000, 1, 1],
        ),
    ];
    let mut bad = Vec::new();
    for (name, op, mxcsr, x0, x1, fault, after, out) in cases {
        let mut s = CpuState {
            mxcsr,
            ..CpuState::default()
        };
        s.xmm[0] = lanes(x0);
        s.xmm[1] = lanes(x1);
        let mut m = memory();
        let stop = run(
            |a| match op {
                Sse::Cvtps2dq => a.cvtps2dq(xmm0, xmm1).unwrap(),
                Sse::Cvttps2dq => a.cvttps2dq(xmm0, xmm1).unwrap(),
                Sse::Cvtpd2dq => a.cvtpd2dq(xmm0, xmm1).unwrap(),
                Sse::Addps => a.addps(xmm0, xmm1).unwrap(),
                Sse::Mulps => a.mulps(xmm0, xmm1).unwrap(),
            },
            &mut s,
            &mut m,
        );
        match fault {
            Some(at_fault) => {
                let trapped = matches!(stop, Some(Step::Trap { vector: 19, .. }));
                check(&mut bad, format!("{name}: #XM"), trapped, true);
                check(&mut bad, format!("{name}: MXCSR at #XM"), s.mxcsr, at_fault);
                check(
                    &mut bad,
                    format!("{name}: destination"),
                    s.xmm[0],
                    lanes(out),
                );
            }
            None => {
                check(
                    &mut bad,
                    format!("{name}: stop"),
                    format!("{stop:?}"),
                    "None".to_string(),
                );
                check(&mut bad, format!("{name}: MXCSR"), s.mxcsr, after);
                check(&mut bad, format!("{name}: result"), s.xmm[0], lanes(out));
            }
        }
    }
    report(bad);
}

fn ext(se: u16, sig: u64) -> [u8; 10] {
    let mut out = [0; 10];
    out[..8].copy_from_slice(&sig.to_le_bytes());
    out[8..].copy_from_slice(&se.to_le_bytes());
    out
}

#[derive(Clone, Copy)]
enum Load {
    M32,
    M64,
    M80,
    /// `fld m80`, then `fldz` and `faddp`.
    M80PlusZero,
}

/// Runs `fninit; fldcw; fld [DATA]; (fldz; faddp;) fnstsw ax; fstp tbyte [OUT]`
/// and returns AX and the stored value.
fn x87_load(load: Load, bytes: &[u8]) -> (u16, [u8; 10]) {
    let mut s = CpuState::default();
    let mut m = memory();
    m.write(DATA, bytes).unwrap();
    m.write(DATA + 0x40, &0x037fu16.to_le_bytes()).unwrap();
    let stop = run(
        |a| {
            a.fninit().unwrap();
            a.fldcw(word_ptr(DATA + 0x40)).unwrap();
            match load {
                Load::M32 => a.fld(dword_ptr(DATA)).unwrap(),
                Load::M64 => a.fld(qword_ptr(DATA)).unwrap(),
                Load::M80 | Load::M80PlusZero => a.fld(tword_ptr(DATA)).unwrap(),
            }
            if matches!(load, Load::M80PlusZero) {
                a.fldz().unwrap();
                a.faddp(st1, st0).unwrap();
            }
            a.fnstsw(ax).unwrap();
            a.fstp(tword_ptr(OUT)).unwrap();
        },
        &mut s,
        &mut m,
    );
    assert!(stop.is_none(), "unexpected stop {stop:?}");
    let out: [u8; 10] = m.read(OUT, 10).unwrap().try_into().unwrap();
    (s.regs[0] as u16, out)
}

// spec: A64F-FP-1
#[test]
fn x87_loads_of_special_encodings_match_intel() {
    let mut bad = Vec::new();
    // m32 / m64: (bits, status word, loaded value).
    let m32: [(u32, u16, (u16, u64)); 5] = [
        (0x0000_0001, 0x3802, (0x3f6a, 0x8000_0000_0000_0000)),
        (0x0040_0000, 0x3802, (0x3f80, 0x8000_0000_0000_0000)),
        (0x8040_0000, 0x3802, (0xbf80, 0x8000_0000_0000_0000)),
        (0x7f80_0001, 0x3801, (0x7fff, 0xc000_0100_0000_0000)),
        (0x7fc0_0001, 0x3800, (0x7fff, 0xc000_0100_0000_0000)),
    ];
    for (bits, sw, (se, sig)) in m32 {
        check(
            &mut bad,
            format!("fld m32 {bits:#010x}"),
            x87_load(Load::M32, &bits.to_le_bytes()),
            (sw, ext(se, sig)),
        );
    }
    let m64: [(u64, u16, (u16, u64)); 4] = [
        (
            0x0000_0000_0000_0001,
            0x3802,
            (0x3bcd, 0x8000_0000_0000_0000),
        ),
        (
            0x0008_0000_0000_0000,
            0x3802,
            (0x3c00, 0x8000_0000_0000_0000),
        ),
        (
            0x7ff0_0000_0000_0001,
            0x3801,
            (0x7fff, 0xc000_0000_0000_0800),
        ),
        (
            0x7ff8_0000_0000_0001,
            0x3800,
            (0x7fff, 0xc000_0000_0000_0800),
        ),
    ];
    for (bits, sw, (se, sig)) in m64 {
        check(
            &mut bad,
            format!("fld m64 {bits:#018x}"),
            x87_load(Load::M64, &bits.to_le_bytes()),
            (sw, ext(se, sig)),
        );
    }
    // m80: (name, encoding, after fld, after fld + 0).
    #[allow(clippy::type_complexity)]
    let m80: [(&str, (u16, u64), (u16, (u16, u64)), (u16, (u16, u64))); 8] = [
        (
            "denormal",
            (0x0000, 1),
            (0x3800, (0x0000, 1)),
            (0x3802, (0x0000, 1)),
        ),
        (
            "pseudo-denormal",
            (0x0000, 0x8000_0000_0000_0001),
            (0x3800, (0x0000, 0x8000_0000_0000_0001)),
            (0x3802, (0x0001, 0x8000_0000_0000_0001)),
        ),
        (
            "unnormal",
            (0x3fff, 0x4000_0000_0000_0000),
            (0x3800, (0x3fff, 0x4000_0000_0000_0000)),
            (0x3801, (0xffff, 0xc000_0000_0000_0000)),
        ),
        (
            "pseudo-infinity",
            (0x7fff, 0),
            (0x3800, (0x7fff, 0)),
            (0x3801, (0xffff, 0xc000_0000_0000_0000)),
        ),
        (
            "pseudo-NaN",
            (0x7fff, 0x4000_0000_0000_0000),
            (0x3800, (0x7fff, 0x4000_0000_0000_0000)),
            (0x3801, (0xffff, 0xc000_0000_0000_0000)),
        ),
        (
            "SNaN",
            (0x7fff, 0x8000_0000_0000_0001),
            (0x3800, (0x7fff, 0x8000_0000_0000_0001)),
            (0x3801, (0x7fff, 0xc000_0000_0000_0001)),
        ),
        (
            "QNaN",
            (0x7fff, 0xc000_0000_0000_0001),
            (0x3800, (0x7fff, 0xc000_0000_0000_0001)),
            (0x3800, (0x7fff, 0xc000_0000_0000_0001)),
        ),
        (
            "normal",
            (0x3fff, 0x8000_0000_0000_0000),
            (0x3800, (0x3fff, 0x8000_0000_0000_0000)),
            (0x3800, (0x3fff, 0x8000_0000_0000_0000)),
        ),
    ];
    for (name, (se, sig), (sw, (ls, lv)), (sw0, (zs, zv))) in m80 {
        let bytes = ext(se, sig);
        check(
            &mut bad,
            format!("fld m80 {name}"),
            x87_load(Load::M80, &bytes),
            (sw, ext(ls, lv)),
        );
        check(
            &mut bad,
            format!("fld m80 {name} + 0"),
            x87_load(Load::M80PlusZero, &bytes),
            (sw0, ext(zs, zv)),
        );
    }
    report(bad);
}

// spec: A64F-FP-1
#[test]
fn x87_nan_operand_selection_matches_intel() {
    const Q1: u64 = 0xc000_0000_0000_0001;
    const Q2: u64 = 0xc000_0000_0000_0002;
    const S1: u64 = 0x8000_0000_0000_0001;
    const S2: u64 = 0x8000_0000_0000_0002;
    const ONE80: u64 = 0x8000_0000_0000_0000;
    // (name, st1, st0, status word, result); identical for fadd and fmul.
    #[allow(clippy::type_complexity)]
    let cases: [(&str, (u16, u64), (u16, u64), u16, (u16, u64)); 10] = [
        (
            "QNaN small, large",
            (0x7fff, Q1),
            (0x7fff, Q2),
            0x3000,
            (0x7fff, Q2),
        ),
        (
            "QNaN large, small",
            (0x7fff, Q2),
            (0x7fff, Q1),
            0x3000,
            (0x7fff, Q2),
        ),
        (
            "QNaN, SNaN",
            (0x7fff, Q1),
            (0x7fff, S2),
            0x3001,
            (0x7fff, Q1),
        ),
        (
            "SNaN, QNaN",
            (0x7fff, S2),
            (0x7fff, Q1),
            0x3001,
            (0x7fff, Q1),
        ),
        (
            "SNaN small, large",
            (0x7fff, S1),
            (0x7fff, S2),
            0x3001,
            (0x7fff, Q2),
        ),
        (
            "SNaN large, small",
            (0x7fff, S2),
            (0x7fff, S1),
            0x3001,
            (0x7fff, Q2),
        ),
        (
            "QNaN, one",
            (0x7fff, Q1),
            (0x3fff, ONE80),
            0x3000,
            (0x7fff, Q1),
        ),
        (
            "one, SNaN",
            (0x3fff, ONE80),
            (0x7fff, S1),
            0x3001,
            (0x7fff, Q1),
        ),
        (
            "-QNaN, +QNaN",
            (0xffff, Q1),
            (0x7fff, Q1),
            0x3000,
            (0x7fff, Q1),
        ),
        (
            "+QNaN, -QNaN",
            (0x7fff, Q1),
            (0xffff, Q1),
            0x3000,
            (0x7fff, Q1),
        ),
    ];
    for multiply in [false, true] {
        for (name, (as_, av), (bs, bv), sw, (rs, rv)) in cases {
            let mut s = CpuState::default();
            let mut m = memory();
            m.write(DATA, &ext(as_, av)).unwrap();
            m.write(DATA + 0x10, &ext(bs, bv)).unwrap();
            m.write(DATA + 0x40, &0x037fu16.to_le_bytes()).unwrap();
            let stop = run(
                |a| {
                    a.fninit().unwrap();
                    a.fldcw(word_ptr(DATA + 0x40)).unwrap();
                    a.fld(tword_ptr(DATA)).unwrap();
                    a.fld(tword_ptr(DATA + 0x10)).unwrap();
                    if multiply {
                        a.fmul_2(st0, st1).unwrap();
                    } else {
                        a.fadd_2(st0, st1).unwrap();
                    }
                    a.fnstsw(ax).unwrap();
                    a.fstp(tword_ptr(OUT)).unwrap();
                    a.fstp(st0).unwrap();
                },
                &mut s,
                &mut m,
            );
            assert!(stop.is_none(), "unexpected stop {stop:?}");
            let out: [u8; 10] = m.read(OUT, 10).unwrap().try_into().unwrap();
            let op = if multiply { "fmul" } else { "fadd" };
            assert_eq!((s.regs[0] as u16, out), (sw, ext(rs, rv)), "{op} {name}");
        }
    }
}
