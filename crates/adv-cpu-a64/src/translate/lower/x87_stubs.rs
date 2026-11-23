//! Shared cold stubs for Tier-1 x87 runs (A64F-FP-7, A64A-FALL-5).
//!
//! Straight-line and cyclic x87 runs emit publication, reload, tag-check,
//! conversion and value-call sequences on every instruction's cold path.
//! Placing those sequences once in the process stub block keeps hot paths
//! unchanged while preventing code-cache blowup in x87-heavy code.

use std::mem::offset_of;

use crate::asm::{Asm, Label};
use crate::encode::Index::{Offset, Post, Pre};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Runtime, field};

const SP: Reg = Reg::new(31);
const RUNTIME: Reg = Reg::new(29);
const X0: Reg = Reg::new(0);
const X1: Reg = Reg::new(1);
const X2: Reg = Reg::new(2);
const X3: Reg = Reg::new(3);
const X5: Reg = Reg::new(5);
const X6: Reg = Reg::new(6);
const X7: Reg = Reg::new(7);
const X8: Reg = Reg::new(8);
const X9: Reg = Reg::new(9);
const X10: Reg = Reg::new(10);
const X11: Reg = Reg::new(11);
const X12: Reg = Reg::new(12);
const X13: Reg = Reg::new(13);
const X15: Reg = Reg::new(15);
const X16: Reg = Reg::new(16);
const X17: Reg = Reg::new(17);
const V0: VReg = VReg::new(0);
const V1: VReg = VReg::new(1);

const LOW: u32 = 64;
const SPAN: u32 = 1981;
const FRACTION: u64 = (1 << 52) - 1;

const VALUE_ACCESS: u32 = field(offset_of!(Runtime, value_access));
const VALUE_CALL: u32 = field(offset_of!(Runtime, value));

/// Byte offsets (in `stubs::Stubs`) or host addresses (in `lower::Stubs`) of
/// shared x87 cold stubs.
#[derive(Clone, Copy, Debug, Default)]
pub struct X87Stubs {
    /// Convert V0 (exact binary64 pair) into extended `(X0 = sig, X1 = sexp)`.
    /// Clobbers: X0, X1, X16, X17, V0, V1, LR.
    pub pair_to_extended: usize,
    /// Convert extended `(X0 = sig, X1 = sexp, W2 = narrow)` into `V0`.
    /// Returns `W0 = 0` on success, `1` on failure.
    /// Clobbers: X0, X16, X17, V0, V1, LR.
    pub pair_from_extended: usize,
    /// Fetch physical register `(W15 + W0) & 7` with `W2 = narrow` into `V0`.
    /// Returns `W0 = 0` on success, `1` on failure.
    /// Clobbers: X0, X1, X16, X17, V0, V1, LR.
    pub fetch: usize,
    /// Check that physical register `(W15 + W2) & 7` holds `V0`.
    /// Returns `W0 = 0` on match, `1` on mismatch.
    /// Clobbers: X0, X1, X2, X3, X16, X17, V0, V1, LR.
    pub match_slot: usize,
    /// Publish x87 model from 44-bit packed descriptor in `X0` and status in `W1`.
    /// Preserves X3..X15 (including X5, X7, X8, X15), X19..X29 and V7..V31.
    /// Clobbers: X0, X1, X2, X16, X17, V0, V1, LR.
    pub publish: usize,
    /// Check zero-tag payloads for `need.value` in `W0` with rotated tags in `W16`.
    /// Returns `W0 = 0` on success (preserving `W16`), `1` on failure.
    /// Clobbers: X0, X1, X2, X17, LR (and X16 on failure).
    pub tags: usize,
    /// Materialize deferred MMX state using `X5 = CpuState`.
    /// Clobbers: X0, X1, X16, X17, LR.
    pub materialize_mmx: usize,
    /// Execute a prepared x87 value call while preserving `V7`, `V16..V31` and
    /// Tier-1 live scalar state.
    /// Inputs: `W1 = helper index`, `W2 = access descriptor` (`0` = None,
    /// `width | (1 << 16)` = Read, `width | (2 << 16)` = Write).
    /// Returns value-call status in `W0`.
    pub value_call: usize,
}

impl X87Stubs {
    /// Relocates byte offsets to host addresses based at `base`.
    pub const fn at(self, base: usize) -> Self {
        Self {
            pair_to_extended: base + self.pair_to_extended,
            pair_from_extended: base + self.pair_from_extended,
            fetch: base + self.fetch,
            match_slot: base + self.match_slot,
            publish: base + self.publish,
            tags: base + self.tags,
            materialize_mmx: base + self.materialize_mmx,
            value_call: base + self.value_call,
        }
    }
}

/// Emits all shared x87 cold stubs into `asm` and returns their byte offsets.
pub(super) fn emit(asm: &mut Asm) -> X87Stubs {
    let pair_to_extended_label = asm.label();
    asm.bind(pair_to_extended_label);
    let pair_to_extended = asm.offset();
    emit_pair_to_extended(asm);

    let pair_from_extended_nonzero = asm.label();
    let pair_from_extended_zero = asm.label();
    let pair_from_extended_fail = asm.label();
    let pair_from_extended = asm.offset();
    emit_pair_from_extended(
        asm,
        pair_from_extended_nonzero,
        pair_from_extended_zero,
        pair_from_extended_fail,
    );

    let fetch = asm.offset();
    emit_fetch(
        asm,
        pair_from_extended_nonzero,
        pair_from_extended_zero,
        pair_from_extended_fail,
    );

    let match_slot = asm.offset();
    emit_match_slot(asm, pair_to_extended_label);

    let publish = asm.offset();
    emit_publish(asm, pair_to_extended_label);

    let tags = asm.offset();
    emit_tags(asm);

    let materialize_mmx = asm.offset();
    crate::facts::materialize_mmx(asm);
    asm.emit(ret(Reg::LR));

    let value_call = asm.offset();
    emit_value_call(asm);

    X87Stubs {
        pair_to_extended,
        pair_from_extended,
        fetch,
        match_slot,
        publish,
        tags,
        materialize_mmx,
        value_call,
    }
}

fn emit_pair_to_extended(asm: &mut Asm) {
    let (zero, case_a, sign) = (asm.label(), asm.label(), asm.label());
    asm.emit_all(&[fmov_from_v(X, X16, V0), lsl_imm(X, X17, X16, 1)]);
    asm.cbz(X, false, X17, zero);
    // Extract the low lane into V1 before overwriting V0 with the scale.
    asm.emit_all(&[
        dup_high(V1, V0),
        ubfx(X, X17, X16, 52, 11),
        movz(W, X1, 2110, 0),
        sub(W, X1, X1, X17, Lsl, 0),
        lsl_imm(X, X1, X1, 52),
        fmov_to_v(X, V0, X1),
        dfloat(Float::Fmul, V1, V1, V0),
        fcvt_int(X, true, X0, V1),
    ]);
    let positive = asm.label();
    asm.tbz(false, X16, 63, positive);
    asm.emit(sub(X, X0, Reg::ZR, X0, Lsl, 0));
    asm.bind(positive);
    asm.emit_all(&[
        add_imm(W, X1, X17, 15360 >> 12, true),
        add_imm(W, X1, X1, 15360 & 0xfff, false),
        lsl_imm(X, X17, X16, 11),
        orr_imm(X, X17, X17, 1 << 63),
    ]);
    asm.tbz(false, X0, 63, case_a);
    asm.emit(tst_imm(X, X16, FRACTION));
    asm.b_cond(Cond::Ne, case_a);
    asm.emit(sub_imm(W, X1, X1, 1, false));
    asm.b(sign);
    asm.bind(case_a);
    asm.emit(add(X, X0, X17, X0, Shift::Asr, 1));
    asm.bind(sign);
    asm.emit_all(&[
        lsr_imm(X, X17, X16, 63),
        orr(W, X1, X1, X17, Lsl, 15),
        ret(Reg::LR),
    ]);
    asm.bind(zero);
    asm.emit_all(&[
        movz(X, X0, 0, 0),
        lsr_imm(X, X1, X16, 63),
        lsl_imm(W, X1, X1, 15),
        ret(Reg::LR),
    ]);
}

fn emit_pair_from_extended(asm: &mut Asm, nonzero: Label, zero: Label, fail: Label) {
    asm.cbz(X, false, X0, zero);
    asm.bind(nonzero);
    asm.emit(and_imm(W, X17, X1, 0x7fff));
    asm.emit(tst_imm(X, X0, 1 << 63));
    asm.b_cond(Cond::Eq, fail);
    let below = 0x3fff - 1023 + LOW;
    asm.emit_all(&[
        sub_imm(W, X17, X17, below >> 12, true),
        sub_imm(W, X17, X17, below & 0xfff, false),
        cmp_imm(W, X17, SPAN, false),
    ]);
    asm.b_cond(Cond::Hi, fail);
    asm.emit_all(&[
        add_imm(W, X17, X17, LOW - 63, false),
        lsl_imm(X, X17, X17, 52),
        lsr_imm(W, X16, X1, 15),
        orr(X, X17, X17, X16, Lsl, 63),
        fmov_to_v(X, V0, X17),
        ubfx(X, X16, X0, 11, 1),
        add(X, X16, X0, X16, Lsl, 0),
        add_imm(X, X16, X16, 0x3ff, false),
        and_imm(X, X16, X16, !0x7ff),
        sub(X, X16, X0, X16, Lsl, 0),
    ]);
    let ok_narrow = asm.label();
    asm.cbz(W, false, X2, ok_narrow);
    asm.cbz(X, true, X16, fail);
    asm.bind(ok_narrow);
    // Keep the scale in V0 while computing low into V1, then spill low's bits
    // to X16 so high can be formed in V0 using V0 and V1 only.
    asm.emit_all(&[
        scvtf(X, V1, X16),
        dfloat(Float::Fmul, V1, V1, V0),
        fmov_from_v(X, X16, V1),
        ucvtf(V1, X0),
        dfloat(Float::Fmul, V0, V1, V0),
        fmov_to_v(X, V1, X16),
        vins(3, V0, 1, V1, 0),
        movz(W, X0, 0, 0),
        ret(Reg::LR),
    ]);
    asm.bind(zero);
    asm.emit(and_imm(W, X17, X1, 0x7fff));
    asm.cbz(W, true, X17, fail);
    asm.emit_all(&[
        lsr_imm(W, X16, X1, 15),
        lsl_imm(X, X16, X16, 63),
        fmov_to_v(X, V0, X16),
        movz(W, X0, 0, 0),
        ret(Reg::LR),
    ]);
    asm.bind(fail);
    asm.emit_all(&[movz(W, X0, 1, 0), ret(Reg::LR)]);
}

fn emit_fetch(asm: &mut Asm, nonzero: Label, zero: Label, fail: Label) {
    asm.emit_all(&[
        add(W, X16, X15, X0, Lsl, 0),
        and_imm(W, X16, X16, 7),
        add_imm(X, X17, X5, runtime::X87_REGS, false),
        add(X, X17, X17, X16, Lsl, 3),
        add(X, X17, X17, X16, Lsl, 1),
        ldr(X, X0, X17, 0),
        ldrh(X1, X17, 8),
    ]);
    asm.cbz(X, true, X0, nonzero);
    // A resident zero can become dirty at a merge. Keep a canonical normal tag
    // with a zero payload on the fallback path instead.
    asm.emit_all(&[
        lsl_imm(W, X16, X16, 1),
        ldrh(X17, X5, runtime::X87_TAG),
        shiftv(W, Shift::Lsr, X17, X17, X16),
    ]);
    asm.tbz(false, X17, 0, fail);
    asm.b(zero);
}

fn emit_match_slot(asm: &mut Asm, pair_to_extended: Label) {
    let fail = asm.label();
    asm.emit(stp(X, Pre, X2, Reg::LR, SP, -16));
    asm.bl(pair_to_extended);
    asm.emit_all(&[
        ldp(X, Post, X2, Reg::LR, SP, 16),
        add(W, X16, X15, X2, Lsl, 0),
        and_imm(W, X16, X16, 7),
        add_imm(X, X17, X5, runtime::X87_REGS, false),
        add(X, X17, X17, X16, Lsl, 3),
        add(X, X17, X17, X16, Lsl, 1),
        ldr(X, X2, X17, 0),
        ldrh(X3, X17, 8),
        cmp(X, X0, X2, Lsl, 0),
    ]);
    asm.b_cond(Cond::Ne, fail);
    asm.emit(cmp(W, X1, X3, Lsl, 0));
    asm.b_cond(Cond::Ne, fail);
    asm.emit_all(&[movz(W, X0, 0, 0), ret(Reg::LR)]);
    asm.bind(fail);
    asm.emit_all(&[movz(W, X0, 1, 0), ret(Reg::LR)]);
}

fn emit_publish(asm: &mut Asm, pair_to_extended: Label) {
    let (loop_head, next, freed_tag, update_status, store_status) = (
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
    );
    asm.emit_all(&[
        stp(X, Pre, X6, X7, SP, -32),
        stp(X, Offset, X8, Reg::LR, SP, 16),
        mov(X, X6, X0),
        mov(W, X7, X1),
    ]);
    asm.tbz(false, X6, 43, update_status);
    // Rotate the tags to TOP at entry, update every modified slot, rotate back.
    asm.emit_all(&[
        ldrh(X8, X5, runtime::X87_TAG),
        orr(W, X8, X8, X8, Lsl, 16),
        lsl_imm(W, X1, X15, 1),
        shiftv(W, Shift::Lsr, X8, X8, X1),
        movz(W, X2, 0, 0),
    ]);
    asm.bind(loop_head);
    asm.emit(and_imm(W, X16, X6, 0x1f));
    asm.cbz(W, false, X16, next);
    let freed_none = asm.label();
    asm.emit_all(&[lsr_imm(W, X17, X16, 3), cmp_imm(W, X17, 3, false)]);
    asm.b_cond(Cond::Eq, freed_none);
    // Copy v(24 + (X16 & 7)) into V0.
    asm.emit(and_imm(W, X17, X16, 7));
    let loaded = asm.label();
    for n in 0u8..8 {
        let next_n = (n < 7).then(|| asm.label());
        if let Some(next_n) = next_n {
            asm.emit(cmp_imm(W, X17, u32::from(n), false));
            asm.b_cond(Cond::Ne, next_n);
        }
        asm.emit(vmov(true, V0, VReg::new(24 + n)));
        if let Some(next_n) = next_n {
            asm.b(loaded);
            asm.bind(next_n);
        }
    }
    asm.bind(loaded);
    asm.bl(pair_to_extended);
    asm.emit_all(&[
        add(W, X16, X15, X2, Lsl, 0),
        and_imm(W, X16, X16, 7),
        add_imm(X, X17, X5, runtime::X87_REGS, false),
        add(X, X17, X17, X16, Lsl, 3),
        add(X, X17, X17, X16, Lsl, 1),
        str(X, X0, X17, 0),
        strh(X1, X17, 8),
        add(X, X17, X5, X16, Lsl, 3),
        str(X, X0, X17, runtime::MMX),
        lsl_imm(W, X16, X2, 1),
        movz(W, X17, 3, 0),
        shiftv(W, Shift::Lsl, X17, X17, X16),
    ]);
    // Bit 4 of X6 is 1 for Freed(Some), 0 for Dirty.
    asm.tbz(true, X6, 4, freed_tag);
    asm.emit_all(&[
        bic(W, X8, X8, X17, Lsl, 0),
        cmp_imm(X, X0, 0, false),
        cset(W, X0, Cond::Eq),
        shiftv(W, Shift::Lsl, X0, X0, X16),
        orr(W, X8, X8, X0, Lsl, 0),
    ]);
    asm.b(next);
    asm.bind(freed_none);
    asm.emit_all(&[
        lsl_imm(W, X16, X2, 1),
        movz(W, X17, 3, 0),
        shiftv(W, Shift::Lsl, X17, X17, X16),
    ]);
    asm.bind(freed_tag);
    asm.emit(orr(W, X8, X8, X17, Lsl, 0));
    asm.bind(next);
    asm.emit_all(&[
        lsr_imm(X, X6, X6, 5),
        add_imm(W, X2, X2, 1, false),
        cmp_imm(W, X2, 8, false),
    ]);
    asm.b_cond(Cond::Lo, loop_head);
    // Rotate tags back and write X87_TAG. After 8 iterations, X6 holds delta.
    asm.emit_all(&[
        and_imm(W, X8, X8, 0xffff),
        orr(W, X8, X8, X8, Lsl, 16),
        lsl_imm(W, X1, X15, 1),
        movz(W, X2, 16, 0),
        sub(W, X2, X2, X1, Lsl, 0),
        shiftv(W, Shift::Lsr, X8, X8, X2),
        strh(X8, X5, runtime::X87_TAG),
    ]);
    asm.b(store_status);
    asm.bind(update_status);
    asm.emit(lsr_imm(X, X6, X6, 40));
    asm.bind(store_status);
    asm.emit_all(&[
        and_imm(W, X6, X6, 7),
        add(W, X16, X15, X6, Lsl, 0),
        and_imm(W, X16, X16, 7),
        bfi(W, X7, X16, 11, 3),
        strh(X7, X5, runtime::X87_STATUS),
        ldp(X, Offset, X8, Reg::LR, SP, 16),
        ldp(X, Post, X6, X7, SP, 32),
        ret(Reg::LR),
    ]);
}

fn emit_tags(asm: &mut Asm) {
    let (loop_head, next, fail) = (asm.label(), asm.label(), asm.label());
    asm.emit_all(&[mov(W, X2, X16), mov(W, X1, X0), movz(W, X0, 0, 0)]);
    asm.bind(loop_head);
    asm.tbz(false, X1, 0, next);
    asm.emit_all(&[lsl_imm(W, X17, X0, 1), shiftv(W, Shift::Lsr, X17, X2, X17)]);
    asm.tbz(true, X17, 1, fail);
    asm.tbz(false, X17, 0, next);
    // Tag 1 (zero) must hold an exact zero payload so publication preserves it.
    asm.emit_all(&[
        add(W, X16, X15, X0, Lsl, 0),
        and_imm(W, X16, X16, 7),
        add_imm(X, X17, X5, runtime::X87_REGS, false),
        add(X, X17, X17, X16, Lsl, 3),
        add(X, X17, X17, X16, Lsl, 1),
        ldr(X, X16, X17, 0),
    ]);
    asm.cbz(X, true, X16, fail);
    asm.emit_all(&[ldrh(X16, X17, 8), and_imm(W, X16, X16, 0x7fff)]);
    asm.cbz(W, true, X16, fail);
    asm.bind(next);
    asm.emit_all(&[lsr_imm(W, X1, X1, 1), add_imm(W, X0, X0, 1, false)]);
    asm.cbz(W, true, X1, loop_head);
    asm.emit_all(&[mov(W, X16, X2), movz(W, X0, 0, 0), ret(Reg::LR)]);
    asm.bind(fail);
    asm.emit_all(&[movz(W, X0, 1, 0), ret(Reg::LR)]);
}

fn emit_value_call(asm: &mut Asm) {
    const FRAME: i32 = 384;
    asm.emit_all(&[
        stp(X, Pre, X7, X8, SP, -FRAME),
        stp(X, Offset, X9, X10, SP, 16),
        stp(X, Offset, X11, X12, SP, 32),
        mrs_nzcv(X16),
        stp(X, Offset, X5, X16, SP, 48),
        stp(X, Offset, Reg::LR, X13, SP, 64),
        stp(X, Offset, X15, X1, SP, 80),
        str(X, X2, SP, 96),
        stp_v(VSize::Q, Offset, VReg::new(16), VReg::new(17), SP, 112),
        stp_v(VSize::Q, Offset, VReg::new(18), VReg::new(19), SP, 144),
        stp_v(VSize::Q, Offset, VReg::new(20), VReg::new(21), SP, 176),
        stp_v(VSize::Q, Offset, VReg::new(22), VReg::new(23), SP, 208),
        stp_v(VSize::Q, Offset, VReg::new(24), VReg::new(25), SP, 240),
        stp_v(VSize::Q, Offset, VReg::new(26), VReg::new(27), SP, 272),
        stp_v(VSize::Q, Offset, VReg::new(28), VReg::new(29), SP, 304),
        stp_v(VSize::Q, Offset, VReg::new(30), VReg::new(31), SP, 336),
        vstr(VMem::Q, VReg::new(7), SP, 368),
    ]);
    let (call_value, done) = (asm.label(), asm.label());
    asm.emit(lsr_imm(W, X16, X2, 16));
    asm.cbz(W, false, X16, call_value);
    asm.emit_all(&[
        mov(X, X0, RUNTIME),
        and_imm(W, X1, X2, 0xffff),
        sub_imm(W, X2, X16, 1, false),
        ldr(X, X16, RUNTIME, VALUE_ACCESS),
        blr(X16),
    ]);
    asm.cbz(W, true, X0, done);
    asm.bind(call_value);
    asm.emit_all(&[
        mov(X, X0, RUNTIME),
        ldr(W, X1, SP, 88),
        ldr(X, X16, RUNTIME, VALUE_CALL),
        blr(X16),
        ldr(W, X2, SP, 96),
        lsr_imm(W, X16, X2, 16),
        cmp_imm(W, X16, 2, false),
    ]);
    asm.b_cond(Cond::Ne, done);
    asm.tbz(false, X0, 8, done);
    asm.emit_all(&[
        mov(X, X0, RUNTIME),
        and_imm(W, X1, X2, 0xffff),
        movz(W, X2, 2, 0),
        ldr(X, X16, RUNTIME, VALUE_ACCESS),
        blr(X16),
    ]);
    asm.bind(done);
    asm.emit_all(&[
        vldr(VMem::Q, VReg::new(7), SP, 368),
        ldp_v(VSize::Q, Offset, VReg::new(30), VReg::new(31), SP, 336),
        ldp_v(VSize::Q, Offset, VReg::new(28), VReg::new(29), SP, 304),
        ldp_v(VSize::Q, Offset, VReg::new(26), VReg::new(27), SP, 272),
        ldp_v(VSize::Q, Offset, VReg::new(24), VReg::new(25), SP, 240),
        ldp_v(VSize::Q, Offset, VReg::new(22), VReg::new(23), SP, 208),
        ldp_v(VSize::Q, Offset, VReg::new(20), VReg::new(21), SP, 176),
        ldp_v(VSize::Q, Offset, VReg::new(18), VReg::new(19), SP, 144),
        ldp_v(VSize::Q, Offset, VReg::new(16), VReg::new(17), SP, 112),
        ldr(X, X15, SP, 80),
        ldp(X, Offset, Reg::LR, X13, SP, 64),
        ldp(X, Offset, X5, X16, SP, 48),
        msr_nzcv(X16),
        ldp(X, Offset, X11, X12, SP, 32),
        ldp(X, Offset, X9, X10, SP, 16),
        ldp(X, Post, X7, X8, SP, FRAME),
        ret(Reg::LR),
    ]);
}
