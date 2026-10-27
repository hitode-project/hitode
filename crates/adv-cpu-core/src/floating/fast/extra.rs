//! Forms with no binary64 lane. These use exact floating-point helpers.
//! Rejection is atomic.
use super::*;
use raw::{compare, operand, ordinary};
use std::cmp::Ordering;

pub fn execute(op: &NativeX87, state: &mut CpuState, memory: &mut Memory) -> Option<u32> {
    let i = &op.instruction;
    let m = i.mnemonic();
    if i.has_lock_prefix()
        || i.segment_prefix() == R::GS
        || state.mmx_mode != 0
        || state.x87.status & 0x80 != 0 && m != M::Fnclex
    {
        return None;
    }
    let mut status = state.x87.status;
    let mut top = state.x87.top();
    let mut write = None;
    let mut freed = None;
    let mut dp = None;
    let mut flags = None;
    let mut metadata = true;
    let epoch = memory.epoch;
    match m {
        M::Fld | M::Fild if i.op0_kind() == OpKind::Memory => {
            let at = address(i, state)?;
            let value = match (m, i.memory_size().size()) {
                (M::Fld, 10) => ordinary(Ext(read(memory, at)?))?,
                (M::Fild, 8) => {
                    let integer = i64::from_le_bytes(read(memory, at)?);
                    let magnitude = integer.unsigned_abs();
                    let sign = if integer < 0 { 0x8000 } else { 0 };
                    if magnitude == 0 {
                        Ext::default()
                    } else {
                        let shift = magnitude.leading_zeros();
                        Ext::from_parts(magnitude << shift, sign | (0x3fff + 63 - shift as u16))
                    }
                }
                _ => return None,
            };
            top = top.wrapping_sub(1) & 7;
            if state.x87.physical_tag(top) != 3 {
                return None;
            }
            write = Some((top, value));
            dp = Some(at);
            status &= !C1;
        }
        M::Fstp if i.op0_kind() == OpKind::Memory && i.memory_size().size() == 10 => {
            let at = address(i, state)?;
            if !writable(memory, at, 10) {
                return None;
            }
            let value = operand(state, 0)?;
            store(memory, at, &value.0)?;
            freed = Some(top);
            top = (top + 1) & 7;
            dp = Some(at);
            status &= !C1;
        }
        M::Fxam => {
            metadata = false;
            let value = state.x87.regs[top];
            let exp = value.sign_exponent() & 0x7fff;
            let sig = value.significand();
            let cc = if state.x87.physical_tag(top) == 3 {
                C3 | C0
            } else if exp == 0 && sig == 0 {
                C3
            } else if exp == 0 {
                C3 | C2
            } else if sig >> 63 == 0 {
                0
            } else if exp == 0x7fff {
                if sig == 1 << 63 { C2 | C0 } else { C0 }
            } else {
                C2
            };
            status = (status & !(C0 | C1 | C2 | C3))
                | cc
                | if value.sign_exponent() & 0x8000 != 0 {
                    C1
                } else {
                    0
                };
        }
        M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
            let a = operand(state, 0)?;
            let index = st(i.op1_register()).unwrap_or(1);
            let b = operand(state, index)?;
            let cc = match compare(a, b) {
                Ordering::Equal => 0x40,
                Ordering::Less => 1,
                Ordering::Greater => 0,
            };
            flags = Some((state.eflags & !0x8d5) | cc);
            if matches!(m, M::Fcomip | M::Fucomip) {
                freed = Some(top);
                top = (top + 1) & 7;
            }
        }
        M::Fnclex => {
            status &= !0x80ff;
            metadata = false;
        }
        _ => return None,
    }
    if let Some((slot, value)) = write {
        state.x87.regs[slot] = value;
        state.x87.set_physical_tag(slot, value.tag());
    }
    if let Some(slot) = freed {
        state.x87.set_physical_tag(slot, 3);
    }
    state.x87.status = status;
    state.x87.set_top(top);
    if metadata {
        state.x87.ip = state.eip;
        if let Some(opcode) = op.opcode {
            state.x87.opcode = opcode;
        }
    }
    if let Some(at) = dp {
        state.x87.dp = at;
    }
    if let Some(flags) = flags {
        state.eflags = flags;
    }
    state.refresh_mmx();
    state.eip = i.next_ip32();
    Some(if memory.epoch != epoch {
        X87_EXIT
    } else {
        X87_DONE
    })
}
