//! Call-site emission for direct bodies (A64-CALL-2, A64-CALL-3, A64F-FP-5).

use adv_cpu_core::body::{DirectBody, Input};

use crate::asm::Asm;
use crate::encode::Index::{Post, Pre};
use crate::encode::Size::{W, X};
use crate::encode::{Reg, add_imm, bfi, blr, ldp, ldr, mov, movk, movz, stp, str};
use crate::runtime;

use super::{RUNTIME, X0, X2, X9, X12, X16, fp_state};

const SP: Reg = Reg::new(31);
const X17: Reg = Reg::new(17);

pub(crate) fn emit_call(asm: &mut Asm, body: DirectBody, afp: bool) {
    let call = body.call as usize as u64;
    let observes_fp = afp && (body.reads_fp_state() || body.writes_mxcsr());
    let restore_fpcr = afp && !body.fp_clean;
    let isolate_fpsr = afp && !observes_fp && !body.fp_clean;
    let frame = if isolate_fpsr { 32 } else { 16 };

    asm.emit(stp(X, Pre, X9, X12, SP, -frame));
    if observes_fp {
        // Fold lazy SSE FPSR into CpuState::mxcsr and refresh any pre-loaded
        // Mxcsr / FloatControl argument registers (w1..w7).
        fp_state::emit_fold_fpsr(asm, X0, X16, X17);
        asm.emit_all(&[
            ldr(X, X16, RUNTIME, runtime::CPU_STATE),
            ldr(W, X17, X16, runtime::MXCSR),
        ]);
        for (slot, input) in body.inputs.iter().enumerate() {
            let rd = Reg::new(1 + slot as u8);
            match input {
                Some(Input::Mxcsr) => asm.emit(mov(W, rd, X17)),
                Some(Input::FloatControl) => asm.emit(bfi(W, rd, X17, 0, 16)),
                _ => {}
            }
        }
    }
    if restore_fpcr {
        fp_state::emit_restore_host_fpcr(asm, X0, X16);
    }
    if isolate_fpsr {
        fp_state::emit_isolate_fpsr_before(asm, X16, X17);
        asm.emit(str(X, X16, SP, 16));
    }
    asm.emit(add_imm(X, X0, RUNTIME, runtime::BODY, false));
    asm.emit(movz(X, X16, call as u16, 0));
    for shift in [16, 32, 48] {
        asm.emit(movk(X, X16, (call >> shift) as u16, shift));
    }
    asm.emit(blr(X16));
    if isolate_fpsr {
        // x0 and x1 carry BodyResult; x2, x16, x17 are free scratch.
        asm.emit(ldr(X, X16, SP, 16));
        fp_state::emit_isolate_fpsr_after(asm, X16, X17, X2);
    }
    asm.emit(ldp(X, Post, X9, X12, SP, frame));
}
