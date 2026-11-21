//! Call-site emission for direct bodies (A64-CALL-2).

use crate::asm::Asm;
use crate::encode::Index::{Post, Pre};
use crate::encode::Size::X;
use crate::encode::{Reg, add_imm, blr, ldp, movk, movz, stp};
use crate::runtime;

use super::{RUNTIME, X0, X9, X12, X16};

const SP: Reg = Reg::new(31);

pub(in crate::translate) fn emit_call(asm: &mut Asm, body: adv_cpu_core::body::DirectBody) {
    let call = body.call as usize as u64;
    asm.emit(stp(X, Pre, X9, X12, SP, -16));
    asm.emit(add_imm(X, X0, RUNTIME, runtime::BODY, false));
    asm.emit(movz(X, X16, call as u16, 0));
    for shift in [16, 32, 48] {
        asm.emit(movk(X, X16, (call >> shift) as u16, shift));
    }
    asm.emit(blr(X16));
    asm.emit(ldp(X, Post, X9, X12, SP, 16));
}
