//! Value classification shared by the two floating-point emitters.

use crate::asm::{Asm, Label};
use crate::encode::Size::{W, X};
use crate::encode::*;

/// Extracts the bits and biased exponent + 1 of a scalar result. If requested,
/// also masks the exponent so zero, subnormal, infinite and NaN results give zero.
pub(super) fn result_bits(
    asm: &mut Asm,
    double: bool,
    v: VReg,
    bits: Reg,
    exp: Reg,
    tiny: Option<Reg>,
) {
    let (size, fraction, exponent) = if double { (X, 52, 11) } else { (W, 23, 8) };
    asm.emit_all(&[
        fmov_from_v(size, bits, v),
        ubfx(size, exp, bits, fraction, exponent),
        add_imm(W, exp, exp, 1, false),
    ]);
    if let Some(tiny) = tiny {
        asm.emit(and_imm(W, tiny, exp, (1 << exponent) - 2));
    }
}

/// Branches to `to`, or sets DE in `flags` without one, for a nonzero subnormal.
pub(super) fn scalar_denormal(
    asm: &mut Asm,
    double: bool,
    input: VReg,
    bits: Reg,
    exp: Reg,
    to: Option<Label>,
    flags: Reg,
) {
    let clean = asm.label();
    let size = if double { X } else { W };
    let fraction = if double { 52 } else { 23 };
    let exponent = if double { 11 } else { 8 };
    asm.emit_all(&[
        fmov_from_v(size, bits, input),
        ubfx(size, exp, bits, fraction, exponent),
    ]);
    asm.cbz(size, true, exp, clean);
    asm.emit(lsl_imm(size, bits, bits, exponent + 1));
    match to {
        Some(to) => asm.cbz(size, true, bits, to),
        None => {
            asm.cbz(size, false, bits, clean);
            asm.emit(orr_imm(W, flags, flags, 2));
        }
    }
    asm.bind(clean);
}

/// Leaves a nonzero mask for subnormal lanes in `v6`.
pub(super) fn packed_denormals(
    asm: &mut Asm,
    arr: Arr,
    double: bool,
    inputs: [Option<VReg>; 2],
    temp: Reg,
    [v4, v5, v6, other]: [VReg; 4],
) {
    asm.emit_all(&[
        movz(X, temp, 1, 0),
        vdup(arr, v4, temp),
        movz(W, temp, 0xffff, 0),
    ]);
    if double {
        asm.emit_all(&[
            movk(X, temp, 0xffff, 16),
            movk(X, temp, 0xffff, 32),
            movk(X, temp, 0x001f, 48),
        ]);
    } else {
        asm.emit(movk(W, temp, 0x00ff, 16));
    }
    asm.emit(vdup(arr, v5, temp));
    for (n, input) in inputs.into_iter().flatten().enumerate() {
        let target = if n == 0 { v6 } else { other };
        asm.emit_all(&[
            vshift(ShiftImm::Shl, arr, target, input, 1),
            vsame(Same::Sub, arr, target, target, v4),
            vsame(Same::Uqsub, arr, target, v5, target),
        ]);
        if n != 0 {
            asm.emit(vlogic(Logic::Orr, true, v6, v6, other));
        }
    }
}

pub(super) fn finish_denormals(
    asm: &mut Asm,
    double: bool,
    mask: VReg,
    [low, bits, flags]: [Reg; 3],
    to: Option<Label>,
) {
    // The 64-bit forms clear the high half.
    if double {
        asm.emit_all(&[
            vumov(3, low, mask, 0),
            vumov(3, bits, mask, 1),
            orr(X, bits, bits, low, Shift::Lsl, 0),
        ]);
    } else {
        asm.emit_all(&[umaxv(mask, mask), fmov_from_v(W, bits, mask)]);
    }
    match to {
        Some(to) => asm.cbz(X, true, bits, to),
        None => {
            let clean = asm.label();
            asm.cbz(X, false, bits, clean);
            asm.emit(orr_imm(W, flags, flags, 2));
            asm.bind(clean);
        }
    }
}

pub(super) fn packed_arithmetic_mask(
    asm: &mut Asm,
    double: bool,
    op: Float,
    rounded: bool,
    [result, a, s]: [VReg; 3],
    temp: Reg,
    [zero, v3, v4, v5, v6]: [VReg; 5],
) {
    let arr = if double { Arr::D2 } else { Arr::S4 };
    // v6 lanes with an infinity or NaN, v5 lanes that may be tiny.
    if double {
        asm.emit(movz(X, temp, 0x7ff0, 48));
    } else {
        asm.emit(movz(W, temp, 0x7f80, 16));
    }
    asm.emit_all(&[
        vdup(arr, v4, temp),
        vlogic(Logic::And, true, v5, result, v4),
    ]);
    if rounded {
        asm.emit_all(&[
            movz(W, temp, 0x7f00, 16),
            vdup(arr, v4, temp),
            vlogic(Logic::And, true, v6, v5, v4),
            vsame(Same::Cmeq, arr, v6, v6, v4),
        ]);
    } else {
        asm.emit(vsame(Same::Cmeq, arr, v6, v5, v4));
    }
    if !matches!(op, Float::Fadd | Float::Fsub) {
        // A product with a zero factor, or a zero dividend, is
        // an exact zero.
        // Lanes below the binade above min normal: zeros,
        // subnormals and products rounded up to min normal,
        // which can be tiny after rounding.
        asm.emit(vzero(true, zero));
        if double {
            asm.emit(movz(X, temp, 0x0020, 48));
        } else {
            asm.emit(movz(W, temp, 0x0100, 16));
        }
        asm.emit_all(&[vdup(arr, v4, temp), vsame(Same::Uqsub, arr, v5, v4, v5)]);
        asm.emit(vcompare(Float::Fcmeq, false, double, v3, a, zero));
        if op == Float::Fmul {
            asm.emit_all(&[
                vcompare(Float::Fcmeq, false, double, v4, s, zero),
                vlogic(Logic::Orr, true, v3, v3, v4),
            ]);
        }
        asm.emit_all(&[
            vlogic(Logic::Bic, true, v5, v5, v3),
            vlogic(Logic::Orr, true, v6, v6, v5),
        ]);
    }
}

pub(super) fn nan_operands(
    asm: &mut Asm,
    scalar: bool,
    double: bool,
    inputs: &[VReg],
    temp: Reg,
    [v3, v4]: [VReg; 2],
    slow: Label,
) {
    for (n, &input) in inputs.iter().enumerate() {
        let target = if n == 0 { v3 } else { v4 };
        asm.emit(vcompare(Float::Fcmeq, scalar, double, target, input, input));
    }
    if inputs.len() == 2 {
        asm.emit(vlogic(Logic::And, true, v3, v3, v4));
    }
    if scalar {
        asm.emit(fmov_from_v(X, temp, v3));
        asm.cbz(W, false, temp, slow);
    } else {
        asm.emit(vnot(true, v3, v3));
        any_lane(asm, v3, temp, slow);
    }
}

pub(super) fn nan_low_pair(asm: &mut Asm, input: VReg, mask: VReg, temp: Reg, slow: Label) {
    // Two binary32 lanes in the low half.
    asm.emit_all(&[
        vfloat(Float::Fcmeq, false, mask, input, input),
        fmov_from_v(X, temp, mask),
        add_imm(X, temp, temp, 1, false),
    ]);
    asm.cbz(X, true, temp, slow);
}

pub(super) fn nan_result(
    asm: &mut Asm,
    scalar: bool,
    double: bool,
    result: VReg,
    mask: VReg,
    temp: Reg,
    slow: Label,
) {
    asm.emit(vcompare(Float::Fcmeq, scalar, double, mask, result, result));
    if scalar {
        asm.emit(fmov_from_v(W, temp, mask));
        asm.cbz(W, false, temp, slow);
    } else {
        asm.emit(vnot(true, mask, mask));
        any_lane(asm, mask, temp, slow);
    }
}

/// Branches if any byte of the lane mask is nonzero.
pub(super) fn any_lane(asm: &mut Asm, mask: VReg, temp: Reg, slow: Label) {
    asm.emit_all(&[umaxv(mask, mask), fmov_from_v(W, temp, mask)]);
    asm.cbz(W, true, temp, slow);
}

/// Branches on a binary64 signaling NaN. Its floating comparison is unordered
/// and its quiet bit is clear.
pub(super) fn snan(asm: &mut Asm, input: VReg, bits: Reg, fail: Label) {
    let clean = asm.label();
    asm.emit(fcmp(input, input));
    asm.b_cond(Cond::Vc, clean);
    asm.emit(fmov_from_v(X, bits, input));
    asm.tbz(false, bits, 51, fail);
    asm.bind(clean);
}
