//! Entry facts are projected from canonical CPU state before lowering. Joins
//! forget whole fields, including each independently tracked x87 tag.

use adv_cpu_core::CpuState;

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::{
    Reg, Shift, add_imm, and, and_imm, eor, ldr, ldrh, lsl_imm, movz, orr, shiftv, str, strh, ubfx,
};
use crate::runtime;

pub(crate) const CONTROL: u64 = 0xffff;
pub(crate) const TOP: u64 = 7 << 16;
const TAG_SHIFT: u32 = 19;
pub(crate) const TAGS: u64 = 0xffff << TAG_SHIFT;
pub(crate) const MMX: u64 = 0xff07 << 35;
pub(crate) const ES: u64 = 1 << 51;
pub(crate) const PE: u64 = 1 << 52;
pub(crate) const MXCSR: u64 = 0x3ff << 53;
pub(crate) const ALL: u64 = CONTROL | TOP | TAGS | MMX | ES | PE | MXCSR;
const MXCSR_CLASS: u32 = 0xffc0;
const MMX_MODE: u32 = 0xff07;

const CPU: Reg = Reg::new(5);
const VALUE: Reg = Reg::new(16);
const SCRATCH: Reg = Reg::new(17);
const ROTATE: Reg = Reg::new(6);

pub(crate) fn tags(slots: u8) -> u64 {
    (0..8).fold(0, |mask, slot| {
        mask | if slots & (1 << slot) != 0 {
            3 << (TAG_SHIFT + 2 * slot)
        } else {
            0
        }
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Key {
    mask: u64,
    value: u64,
}

impl Key {
    pub(crate) fn read(cpu: &CpuState, mask: u64) -> Self {
        if mask == 0 {
            return Self::default();
        }
        let x87 = &cpu.x87;
        let status = x87.status;
        let top = (status >> 11) & 7;
        let tag = x87.tag.rotate_right(u32::from(top) * 2);
        let value = u64::from(x87.control)
            | (u64::from(top) << 16)
            | (u64::from(tag) << TAG_SHIFT)
            | (u64::from(cpu.mmx_mode & MMX_MODE) << 35)
            | (u64::from(status & 0x80 != 0) * ES)
            | (u64::from(status & 0x20 != 0) * PE)
            | (u64::from(cpu.mxcsr & MXCSR_CLASS) << 47);
        Self {
            mask,
            value: value & mask,
        }
    }

    /// Standalone lowering and the worker path still supply these two facts.
    pub(crate) fn x87(control: u16, pe: bool) -> Self {
        Self {
            mask: CONTROL | PE,
            value: u64::from(control) | (u64::from(pe) * PE),
        }
    }

    pub(crate) fn control(self) -> Option<u16> {
        (self.mask & CONTROL != 0).then_some(self.value as u16)
    }

    pub(crate) fn pe(self) -> bool {
        self.value & PE != 0
    }

    pub(crate) fn pending(self) -> Option<bool> {
        (self.mask & ES != 0).then_some(self.value & ES != 0)
    }

    pub(crate) fn mmx(self) -> Option<u32> {
        (self.mask & MMX != 0).then_some(((self.value & MMX) >> 35) as u32)
    }

    pub(crate) fn tag(self, slot: usize) -> Option<u8> {
        let shift = TAG_SHIFT + 2 * slot as u32;
        (self.mask & (3 << shift) != 0).then_some(((self.value >> shift) & 3) as u8)
    }

    pub(crate) fn mxcsr(self) -> Option<u32> {
        (self.mask & MXCSR != 0).then_some(((self.value & MXCSR) >> 47) as u32)
    }

    pub(crate) fn mask(self) -> u64 {
        self.mask
    }

    pub(crate) fn proves(self, other: Self) -> bool {
        self.mask & other.mask == other.mask && self.value & other.mask == other.value
    }

    pub(crate) fn project(self, mask: u64) -> Self {
        Self {
            mask: self.mask & mask,
            value: self.value & mask,
        }
    }

    pub(crate) fn join(self, other: Self) -> Self {
        let both = self.mask & other.mask;
        let different = self.value ^ other.value;
        let mut mask = both;
        for field in [CONTROL, TOP, MMX, ES, PE, MXCSR] {
            if different & field != 0 {
                mask &= !field;
            }
        }
        for slot in 0..8 {
            let field = 3 << (TAG_SHIFT + 2 * slot);
            if different & field != 0 {
                mask &= !field;
            }
        }
        Self {
            mask,
            value: self.value & mask,
        }
    }

    /// Branches on a mismatch without changing guest NZCV or flag recipes.
    pub(crate) fn guard(self, asm: &mut Asm, miss: Label) {
        if self.mask == 0 {
            return;
        }
        asm.emit(ldr(X, CPU, Reg::new(29), runtime::CPU_STATE));
        if let Some(control) = self.control() {
            asm.emit(ldrh(VALUE, CPU, runtime::X87_CONTROL));
            compare(asm, control, u16::MAX, true, miss);
        }
        let top = ((self.value & TOP) >> 16) as u32;
        let mask = if self.mask & TOP != 0 { 0x3800 } else { 0 }
            | if self.mask & ES != 0 { 0x80 } else { 0 }
            | if self.mask & PE != 0 { 0x20 } else { 0 };
        if mask != 0 {
            let value = (top << 11) as u16
                | if self.value & ES != 0 { 0x80 } else { 0 }
                | if self.value & PE != 0 { 0x20 } else { 0 };
            asm.emit(ldrh(VALUE, CPU, runtime::X87_STATUS));
            compare(asm, value, mask, true, miss);
        }
        if self.mask & TAGS != 0 {
            let mut mask = (self.mask >> TAG_SHIFT) as u16;
            let mut value = (self.value >> TAG_SHIFT) as u16;
            asm.emit(ldrh(VALUE, CPU, runtime::X87_TAG));
            if self.mask & TOP != 0 {
                mask = mask.rotate_left(top * 2);
                value = value.rotate_left(top * 2);
            } else {
                asm.emit_all(&[
                    ldrh(ROTATE, CPU, runtime::X87_STATUS),
                    ubfx(W, ROTATE, ROTATE, 11, 3),
                    lsl_imm(W, ROTATE, ROTATE, 1),
                    orr(W, VALUE, VALUE, VALUE, Lsl, 16),
                    shiftv(W, Shift::Lsr, VALUE, VALUE, ROTATE),
                ]);
            }
            compare(asm, value, mask, false, miss);
        }
        if self.mask & MMX != 0 {
            asm.emit(ldr(W, VALUE, CPU, runtime::MMX_MODE));
            compare(
                asm,
                ((self.value & MMX) >> 35) as u16,
                MMX_MODE as u16,
                false,
                miss,
            );
        }
        if let Some(mxcsr) = self.mxcsr() {
            asm.emit(ldr(W, VALUE, CPU, runtime::MXCSR));
            compare(asm, mxcsr as u16, MXCSR_CLASS as u16, false, miss);
        }
    }
}

/// Commit the deferred MMX image before a generated instruction observes x87.
/// The MMX array already holds the architectural significands of every slot.
pub(crate) fn materialize_mmx(asm: &mut Asm) {
    let mode = VALUE;
    let data = SCRATCH;
    let slot = Reg::new(0);
    let exponent = Reg::new(1);
    let done = asm.label();
    let emms = asm.label();
    let clear = asm.label();
    asm.emit(ldr(W, mode, CPU, runtime::MMX_MODE));
    asm.cbz(W, false, mode, done);
    asm.tbz(false, mode, 0, emms);
    asm.emit(movz(W, exponent, 0xffff, 0));
    for index in 0..8 {
        let write = asm.label();
        let next = asm.label();
        asm.tbz(false, mode, 2, write);
        asm.tbz(false, mode, index + 8, next);
        asm.bind(write);
        asm.emit_all(&[
            ldr(X, data, CPU, runtime::MMX + 8 * index),
            add_imm(X, slot, CPU, runtime::X87_REGS + 10 * index, false),
            str(X, data, slot, 0),
            strh(exponent, slot, 8),
        ]);
        asm.bind(next);
    }
    asm.emit_all(&[
        ldrh(data, CPU, runtime::X87_STATUS),
        and_imm(W, data, data, 0xffff_c7ff),
        strh(data, CPU, runtime::X87_STATUS),
        strh(Reg::ZR, CPU, runtime::X87_TAG),
    ]);
    asm.bind(emms);
    asm.tbz(false, mode, 1, clear);
    asm.emit_all(&[movz(W, data, 0xffff, 0), strh(data, CPU, runtime::X87_TAG)]);
    asm.bind(clear);
    asm.emit(str(W, Reg::ZR, CPU, runtime::MMX_MODE));
    asm.bind(done);
}

/// Reconcile a clean preload with its x87 alias. A pending MMX write
/// already owns its significand, and a clean refresh leaves the exponent.
pub(crate) fn refresh_mmx_slot(asm: &mut Asm, register: u8) {
    let register = u32::from(register);
    let offset = runtime::X87_REGS + 10 * register;
    let done = asm.label();
    let refresh = asm.label();
    asm.emit_all(&[
        ldrh(SCRATCH, CPU, offset + 8),
        add_imm(W, SCRATCH, SCRATCH, 1, false),
    ]);
    asm.tbz(true, SCRATCH, 16, done);
    asm.emit(ldr(W, VALUE, CPU, runtime::MMX_MODE));
    asm.tbz(false, VALUE, 0, refresh);
    asm.tbz(false, VALUE, 2, done);
    asm.tbz(true, VALUE, 8 + register, done);
    asm.bind(refresh);
    asm.emit_all(&[
        add_imm(X, SCRATCH, CPU, offset, false),
        ldr(X, VALUE, SCRATCH, 0),
        str(X, VALUE, CPU, runtime::MMX + 8 * register),
    ]);
    asm.bind(done);
}

pub(crate) fn masked_mxcsr(class: u32) -> bool {
    class & (MXCSR_CLASS & !0x6000) == 0x1f80
}

pub(crate) fn default_mxcsr(class: u32) -> bool {
    class & MXCSR_CLASS == 0x1f80
}

fn compare(asm: &mut Asm, value: u16, mask: u16, halfword: bool, miss: Label) {
    if value != 0 {
        asm.emit_all(&[
            movz(W, SCRATCH, value, 0),
            eor(W, VALUE, VALUE, SCRATCH, Lsl, 0),
        ]);
    }
    if !halfword || mask != u16::MAX {
        asm.emit_all(&[
            movz(W, SCRATCH, mask, 0),
            and(W, VALUE, VALUE, SCRATCH, Lsl, 0),
        ]);
    }
    asm.cbz(W, true, VALUE, miss);
}

#[cfg(test)]
mod tests {
    use super::{Asm, CONTROL, ES, Key, MMX, MXCSR, TOP, tags};
    use adv_cpu_core::CpuState;

    // spec: A64A-ROLE-3 A64A-FALL-3
    #[test]
    fn joins_keep_whole_fields() {
        let mut cpu = CpuState::default();
        let mask = CONTROL | TOP | tags(1) | MMX | ES | MXCSR;
        let original = Key::read(&cpu, mask);
        cpu.x87.control ^= 0x0200;
        cpu.x87.set_top(3);
        let observed = Key::read(&cpu, mask);
        let joined = original.join(observed);
        assert_eq!(joined.mask(), mask & !(CONTROL | TOP));
        assert!(original.proves(joined));
        assert!(observed.proves(joined));
    }

    // spec: A64A-PLACE-2
    #[test]
    fn empty_key_and_control_guard_have_fixed_cost() {
        for (mask, instructions) in [(0, 0), (CONTROL, 5)] {
            let key = Key::read(&CpuState::default(), mask);
            let mut asm = Asm::default();
            let miss = asm.label();
            key.guard(&mut asm, miss);
            asm.bind(miss);
            assert_eq!(asm.finish().len(), instructions);
        }
    }
}
