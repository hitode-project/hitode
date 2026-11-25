//! Entry facts are projected from canonical CPU state before lowering. Control
//! bitfields (`CONTROL`, `MXCSR`, `MXCSR_STICKY`, `X87_STATUS_REST`,
//! `X87_EXACT`) join bitwise over agreed bits while multi-bit indices (`TOP`,
//! `MMX`, per-slot tags, `ESP_LOW`, `EBP_DELTA`) stay atomic.

use adv_cpu_core::memory::{READ, SHARED, WINDOW_SPAN_WORDS, WRITE};
use adv_cpu_core::{CpuState, HostFeatures};

use crate::asm::{Asm, Label};
use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::{
    Cond, Extend, Narrow, Reg, Shift, add, add_ext, add_imm, and, and_imm, bitmask, clz, cmp,
    cmp_imm, eor, eor_imm, ldr, ldr_narrow, ldr_reg, ldrh, ldur, lsl_imm, lsr_imm, mov, movk, movz,
    orr, rbit, ret, shiftv, str, strh, sub, sub_ext, tst_imm, ubfx,
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

// These fields extend the Tier-1 encoding for resident entry. TOP, ES and PE
// keep their Tier-1 positions, even when a form assumes the entire status word.
const X87_STATUS_REST: u64 = 0x7ff;
const ESP_LOW_SHIFT: u32 = 11;
const ESP_LOW: u64 = 0xf << ESP_LOW_SHIFT;
const EBP_DELTA_SHIFT: u32 = 15;
const EBP_DELTA: u64 = 0xffff_ffff << EBP_DELTA_SHIFT;
const MXCSR_STICKY_SHIFT: u32 = 47;
const MXCSR_STICKY: u64 = 0x3f << MXCSR_STICKY_SHIFT;
const X87_EXACT_SHIFT: u32 = 53;
const ESP: Reg = Reg::new(23);
const EBP: Reg = Reg::new(24);

fn status_bits(status: u16) -> u16 {
    (status & 0x1f) | ((status >> 1) & 0x20) | ((status >> 2) & 0x1c0) | ((status >> 5) & 0x600)
}

fn status_word(bits: u16) -> u16 {
    (bits & 0x1f) | ((bits & 0x20) << 1) | ((bits & 0x1c0) << 2) | ((bits & 0x600) << 5)
}

fn floating_facts(control: u16, status: u16, tags: u16, mode: u32, mxcsr: u32) -> u64 {
    let top = (status >> 11) & 7;
    let tag = tags.rotate_right(u32::from(top) * 2);
    u64::from(control)
        | (u64::from(top) << 16)
        | (u64::from(tag) << TAG_SHIFT)
        | (u64::from(mode & MMX_MODE) << 35)
        | (u64::from(status & 0x80 != 0) * ES)
        | (u64::from(status & 0x20 != 0) * PE)
        | (u64::from(mxcsr & MXCSR_CLASS) << 47)
}

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

/// Extra-word bit budget for extended resident/opt facts beyond the primary
/// 64-bit value and context words (e.g., per-slot binary32 exactness).
pub(crate) const EXTRA_X87_EXACT_F32_SHIFT: u32 = 0;
pub(crate) const EXTRA_X87_EXACT_F32: u64 = 0xff << EXTRA_X87_EXACT_F32_SHIFT;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Key {
    mask: u64,
    value: u64,
    context_mask: u64,
    context: u64,
    extra_mask: u64,
    extra: u64,
}

impl Key {
    pub(crate) fn read(cpu: &CpuState, mask: u64) -> Self {
        if mask == 0 {
            return Self::default();
        }
        let x87 = &cpu.x87;
        Self {
            mask,
            value: floating_facts(x87.control, x87.status, x87.tag, cpu.mmx_mode, cpu.mxcsr) & mask,
            ..Self::default()
        }
    }

    pub(crate) fn resident(assumed: &Assumed, exact: u8) -> Self {
        let has_afp = HostFeatures::get().contains(HostFeatures::AFP);
        Self::resident_with_afp(assumed, exact, has_afp)
    }

    pub(crate) fn resident_with_afp(assumed: &Assumed, exact: u8, has_afp: bool) -> Self {
        let mut mask = MMX;
        let mut context_mask = u64::from(exact) << X87_EXACT_SHIFT;
        let mut context = context_mask;
        if assumed.x87 {
            mask |= CONTROL | TOP | TAGS | ES | PE;
            context_mask |= X87_STATUS_REST;
            context |= u64::from(status_bits(assumed.status));
        }
        if assumed.mmx {
            mask |= ES;
        }
        if assumed.mxcsr_mask != 0 {
            let ctrl = u64::from(assumed.mxcsr_mask & MXCSR_CLASS);
            mask |= ctrl << 47;
            // With FEAT_AFP and all SSE exceptions masked, sticky bits live lazily
            // in hardware FPSR; keep PE (bit 5) only so sticky churn across
            // IE/DE/ZE/OE/UE never causes version bloat or thrashing. Without
            // FEAT_AFP, with unmasked exceptions, or when the caller explicitly
            // requests a full 32-bit exact match (`u32::MAX`), match every requested
            // sticky bit.
            let mut sticky = assumed.mxcsr_mask & 0x3f;
            if has_afp && assumed.mxcsr_mask != u32::MAX && assumed.mxcsr & 0x1f80 == 0x1f80 {
                sticky &= 0x20;
            }
            if sticky != 0 {
                context_mask |= u64::from(sticky) << MXCSR_STICKY_SHIFT;
                context |= u64::from(assumed.mxcsr & sticky) << MXCSR_STICKY_SHIFT;
            }
        }
        if let Some(low) = assumed.esp_low {
            context_mask |= ESP_LOW;
            context |= u64::from(low) << ESP_LOW_SHIFT;
        }
        if let Some(delta) = assumed.ebp {
            context_mask |= EBP_DELTA;
            context |= u64::from(delta as u32) << EBP_DELTA_SHIFT;
        }
        let status = if assumed.x87 {
            assumed.status | u16::from(assumed.top) << 11
        } else {
            0
        };
        Self {
            mask,
            value: floating_facts(assumed.control, status, assumed.tags, 0, assumed.mxcsr) & mask,
            context_mask,
            context,
            extra_mask: 0,
            extra: 0,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_exact_f32(mut self, slots: u8) -> Self {
        let bits = u64::from(slots) << EXTRA_X87_EXACT_F32_SHIFT;
        self.extra_mask |= bits;
        self.extra |= bits;
        self
    }

    pub(crate) fn holds(self, cpu: &CpuState) -> bool {
        let observed = Self::read(cpu, self.mask);
        if !observed.proves(self.project(self.mask)) {
            return false;
        }
        let context = u64::from(status_bits(cpu.x87.status))
            | (u64::from(cpu.regs[4] & 15) << ESP_LOW_SHIFT)
            | (u64::from(cpu.regs[5].wrapping_sub(cpu.regs[4])) << EBP_DELTA_SHIFT)
            | (u64::from(cpu.mxcsr & 0x3f) << MXCSR_STICKY_SHIFT);
        let exact = (self.context_mask >> X87_EXACT_SHIFT) as u8;
        let exact_f32 =
            ((self.extra_mask & EXTRA_X87_EXACT_F32) >> EXTRA_X87_EXACT_F32_SHIFT) as u8;
        context & (self.context_mask & ((1 << X87_EXACT_SHIFT) - 1))
            == self.context & ((1 << X87_EXACT_SHIFT) - 1)
            && (0..8).all(|slot| exact & (1 << slot) == 0 || binary64(cpu.x87.regs[slot]).is_some())
            && (0..8).all(|slot| {
                exact_f32 & (1 << slot) == 0
                    || binary64(cpu.x87.regs[slot]).is_some_and(|bits| {
                        let d = f64::from_bits(bits);
                        f64::from(d as f32).to_bits() == bits
                    })
            })
    }

    /// Standalone lowering and the worker path still supply these two facts.
    pub(crate) fn x87(control: u16, pe: bool) -> Self {
        Self {
            mask: CONTROL | PE,
            value: u64::from(control) | (u64::from(pe) * PE),
            ..Self::default()
        }
    }

    pub(crate) fn control(self) -> Option<u16> {
        (self.mask & CONTROL == CONTROL).then_some(self.value as u16)
    }

    /// Known `(value, mask)` bitfield for the x87 control word after bitwise joins.
    pub(crate) fn control_bits(self) -> (u16, u16) {
        ((self.value & CONTROL) as u16, (self.mask & CONTROL) as u16)
    }

    pub(crate) fn pe(self) -> bool {
        self.value & PE != 0
    }

    pub(crate) fn pending(self) -> Option<bool> {
        (self.mask & ES != 0).then_some(self.value & ES != 0)
    }

    pub(crate) fn mmx(self) -> Option<u32> {
        (self.mask & MMX == MMX).then_some(((self.value & MMX) >> 35) as u32)
    }

    pub(crate) fn tag(self, slot: usize) -> Option<u8> {
        let shift = TAG_SHIFT + 2 * slot as u32;
        (self.mask & (3 << shift) == 3 << shift).then_some(((self.value >> shift) & 3) as u8)
    }

    pub(crate) fn mxcsr(self) -> Option<u32> {
        (self.mask & MXCSR == MXCSR).then_some(((self.value & MXCSR) >> 47) as u32)
    }

    /// Known `(value, mask)` bitfield for MXCSR (control bits 15:6 and sticky bits 5:0).
    pub(crate) fn mxcsr_bits(self) -> (u32, u32) {
        let ctrl_val = ((self.value & MXCSR) >> 47) as u32;
        let ctrl_mask = ((self.mask & MXCSR) >> 47) as u32;
        let sticky_val = ((self.context & MXCSR_STICKY) >> MXCSR_STICKY_SHIFT) as u32;
        let sticky_mask = ((self.context_mask & MXCSR_STICKY) >> MXCSR_STICKY_SHIFT) as u32;
        (ctrl_val | sticky_val, ctrl_mask | sticky_mask)
    }

    pub(crate) fn mask(self) -> u64 {
        self.mask
    }

    pub(crate) fn uses_resident_context(self) -> bool {
        self.context_mask != 0 || self.extra_mask != 0
    }

    pub(crate) fn proves(self, other: Self) -> bool {
        self.mask & other.mask == other.mask
            && self.value & other.mask == other.value
            && self.context_mask & other.context_mask == other.context_mask
            && self.context & other.context_mask == other.context
            && self.extra_mask & other.extra_mask == other.extra_mask
            && self.extra & other.extra_mask == other.extra
    }

    pub(crate) fn project(self, mask: u64) -> Self {
        Self {
            mask: self.mask & mask,
            value: self.value & mask,
            ..Self::default()
        }
    }

    /// Bitwise join over control/status bitfields (`CONTROL`, `MXCSR`, `ES`, `PE`,
    /// `X87_STATUS_REST`, `MXCSR_STICKY`, `X87_EXACT`, `extra`), preserving
    /// whole-field atomicity only for multi-bit indices (`TOP`, `MMX`, per-slot
    /// tags, `ESP_LOW`, `EBP_DELTA`).
    pub(crate) fn join(self, other: Self) -> Self {
        let both = self.mask & other.mask;
        let different = self.value ^ other.value;
        let mut mask = both & !(different & (CONTROL | ES | PE | MXCSR));
        for field in [TOP, MMX] {
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
        let context_difference = self.context ^ other.context;
        let mut context_mask = (self.context_mask & other.context_mask)
            & !(context_difference & (X87_STATUS_REST | MXCSR_STICKY | (0xff << X87_EXACT_SHIFT)));
        for field in [ESP_LOW, EBP_DELTA] {
            if context_difference & field != 0 {
                context_mask &= !field;
            }
        }
        let extra_mask = (self.extra_mask & other.extra_mask) & !(self.extra ^ other.extra);
        Self {
            mask,
            value: self.value & mask,
            context_mask,
            context: self.context & context_mask,
            extra_mask,
            extra: self.extra & extra_mask,
        }
    }

    /// Branches on a mismatch without changing guest NZCV or flag recipes.
    pub(crate) fn guard(self, asm: &mut Asm, miss: Label) {
        if self.mask == 0 {
            return;
        }
        asm.emit(ldr(X, CPU, Reg::new(29), runtime::CPU_STATE));
        self.guard_cpu(asm, CPU, false, miss);
    }

    pub(crate) fn guard_mmx(self, asm: &mut Asm, cpu: Reg, resident: bool, miss: Label) {
        asm.emit(ldr(W, VALUE, cpu, runtime::MMX_MODE));
        let mode = self.mmx().unwrap();
        if resident {
            asm.cbz(W, true, VALUE, miss);
        } else {
            compare(asm, mode as u16, MMX_MODE as u16, false, false, miss);
        }
    }

    pub(crate) fn guard_resident(self, asm: &mut Asm, cpu: Reg, miss: Label) {
        self.guard_cpu(asm, cpu, true, miss);
        if self.context_mask & ESP_LOW != 0 {
            asm.emit(and_imm(W, VALUE, ESP, 15));
            asm.emit(movz(
                X,
                SCRATCH,
                ((self.context & ESP_LOW) >> ESP_LOW_SHIFT) as u16,
                0,
            ));
            asm.emit(eor(W, VALUE, VALUE, SCRATCH, Lsl, 0));
            asm.cbz(W, true, VALUE, miss);
        }
        if self.context_mask & EBP_DELTA != 0 {
            asm.emit(sub(W, VALUE, EBP, ESP, Lsl, 0));
            let delta = ((self.context & EBP_DELTA) >> EBP_DELTA_SHIFT) as u32;
            immediate(asm, SCRATCH, delta);
            asm.emit(eor(W, VALUE, VALUE, SCRATCH, Lsl, 0));
            asm.cbz(W, true, VALUE, miss);
        }
    }

    fn guard_cpu(self, asm: &mut Asm, cpu: Reg, resident: bool, miss: Label) {
        let (control_val, control_mask) = self.control_bits();
        if control_mask != 0 {
            asm.emit(ldrh(VALUE, cpu, runtime::X87_CONTROL));
            compare(asm, control_val, control_mask, true, resident, miss);
        }
        let top = ((self.value & TOP) >> 16) as u32;
        let rest_mask = (self.context_mask & X87_STATUS_REST) as u16;
        let mask = status_word(rest_mask)
            | if self.mask & TOP != 0 { 0x3800 } else { 0 }
            | if self.mask & ES != 0 { 0x80 } else { 0 }
            | if self.mask & PE != 0 { 0x20 } else { 0 };
        if resident && mask == 0x80 && !self.pending().unwrap() {
            asm.emit(ldrh(VALUE, cpu, runtime::X87_STATUS));
            asm.tbz(true, VALUE, 7, miss);
        } else if mask != 0 {
            let value = status_word((self.context & X87_STATUS_REST) as u16)
                | (top << 11) as u16
                | if self.value & ES != 0 { 0x80 } else { 0 }
                | if self.value & PE != 0 { 0x20 } else { 0 };
            asm.emit(ldrh(VALUE, cpu, runtime::X87_STATUS));
            compare(asm, value, mask, true, resident, miss);
        }
        if self.mask & TAGS != 0 {
            let mut mask = (self.mask >> TAG_SHIFT) as u16;
            let mut value = (self.value >> TAG_SHIFT) as u16;
            asm.emit(ldrh(VALUE, cpu, runtime::X87_TAG));
            if self.mask & TOP != 0 {
                mask = mask.rotate_left(top * 2);
                value = value.rotate_left(top * 2);
            } else {
                asm.emit_all(&[
                    ldrh(ROTATE, cpu, runtime::X87_STATUS),
                    ubfx(W, ROTATE, ROTATE, 11, 3),
                    lsl_imm(W, ROTATE, ROTATE, 1),
                    orr(W, VALUE, VALUE, VALUE, Lsl, 16),
                    shiftv(W, Shift::Lsr, VALUE, VALUE, ROTATE),
                ]);
            }
            compare(asm, value, mask, resident, resident, miss);
        }
        if !resident && self.mask & MMX != 0 {
            self.guard_mmx(asm, cpu, false, miss);
        }
        let (mxcsr_val, mxcsr_mask) = self.mxcsr_bits();
        if mxcsr_mask != 0 {
            asm.emit(ldr(W, VALUE, cpu, runtime::MXCSR));
            let full = mxcsr_mask as u16 == u16::MAX;
            compare(
                asm,
                mxcsr_val as u16,
                mxcsr_mask as u16,
                full,
                resident,
                miss,
            );
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

fn compare(asm: &mut Asm, value: u16, mask: u16, full_load: bool, resident: bool, miss: Label) {
    if value != 0 || resident {
        asm.emit_all(&[
            movz(if resident { X } else { W }, SCRATCH, value, 0),
            eor(W, VALUE, VALUE, SCRATCH, Lsl, 0),
        ]);
    }
    if !full_load || mask != u16::MAX {
        if resident && bitmask(W, u64::from(mask)).is_some() {
            asm.emit(and_imm(W, VALUE, VALUE, u64::from(mask)));
        } else {
            asm.emit_all(&[
                movz(W, SCRATCH, mask, 0),
                and(W, VALUE, VALUE, SCRATCH, Lsl, 0),
            ]);
        }
    }
    asm.cbz(W, true, VALUE, miss);
}

#[derive(Debug)]
pub(crate) struct Assumed {
    pub control: u16,
    pub status: u16,
    pub mxcsr: u32,
    pub top: u8,
    pub tags: u16,
    /// ESP modulo 16, when the unit aligns ESP.
    pub esp_low: Option<u8>,
    /// EBP minus entry ESP when EBP addresses the frame.
    pub ebp: Option<i32>,
    pub denormal: bool,
    /// Whether the form runs x87 instructions, so that the x87 facts hold.
    pub x87: bool,
    /// Whether the form runs MMX or EMMS instructions, requiring no pending x87 exception.
    pub mmx: bool,
    /// The MXCSR bits the form assumes: all of them when it runs SSE
    /// floating-point or control instructions, else the control bits.
    pub mxcsr_mask: u32,
}

/// The binary64 bits of an x87 register that holds a binary64 value
/// exactly; `None` for NaN, unnormals and values binary64 cannot hold.
pub(crate) fn binary64(reg: adv_cpu_core::state::X87Register) -> Option<u64> {
    let (m, se) = (reg.significand(), reg.sign_exponent());
    let sign = u64::from(se & 0x8000) << 48;
    let e = i32::from(se & 0x7fff);
    if m == 0 {
        return (e == 0).then_some(sign);
    }
    if m >> 63 == 0 {
        return None;
    }
    if e == 0x7fff {
        return (m == 1 << 63).then_some(sign | 0x7ff << 52);
    }
    let exp = e - 16383;
    if exp >= -1022 {
        return (exp <= 1023 && m & 0x7ff == 0)
            .then(|| sign | ((exp + 1023) as u64) << 52 | (m >> 11) & ((1 << 52) - 1));
    }
    let shift = u32::try_from(-1011 - exp).ok().filter(|&s| s < 64)?;
    (m & ((1 << shift) - 1) == 0).then_some(sign | m >> shift)
}

/// Converts the physical x87 input to binary64 bits in T.
pub(crate) fn guard_binary64(asm: &mut Asm, i: u8, miss: Label) {
    let zero = asm.label();
    let special = asm.label();
    let tiny = asm.label();
    let done = asm.label();
    asm.emit_all(&[
        add_imm(X, X4, X0, runtime::X87_REGS, false),
        ldur(X, T, X4, 10 * i32::from(i)),
        ldrh(X2, X4, 10 * u32::from(i) + 8),
        ubfx(W, X3, X2, 0, 15),
    ]);
    asm.cbz(X, false, T, zero);
    asm.tbz(false, T, 63, miss);
    asm.emit(movz(X, U, 0x7fff, 0));
    asm.emit(cmp(W, X3, U, Lsl, 0));
    asm.b_cond(Cond::Eq, special);

    // Biased binary64 exponent. Subnormals use a variable right shift.
    asm.emit(movz(X, U, 16383 - 1023, 0));
    asm.emit(sub(W, X3, X3, U, Lsl, 0));
    asm.emit(cmp_imm(W, X3, 0, false));
    asm.b_cond(Cond::Le, tiny);
    asm.emit(cmp_imm(W, X3, 0x7ff, false));
    asm.b_cond(Cond::Ge, miss);
    asm.emit(tst_imm(X, T, 0x7ff));
    asm.b_cond(Cond::Ne, miss);
    asm.emit_all(&[ubfx(X, T, T, 11, 52), orr(X, T, T, X3, Lsl, 52)]);
    asm.b(done);

    asm.bind(tiny);
    asm.emit(movz(X, U, 12, 0));
    asm.emit(sub(W, X3, U, X3, Lsl, 0));
    asm.emit(cmp_imm(W, X3, 64, false));
    asm.b_cond(Cond::Hs, miss);
    asm.emit_all(&[
        shiftv(X, Lsr, U, T, X3),
        shiftv(X, Lsl, X4, U, X3),
        cmp(X, X4, T, Lsl, 0),
    ]);
    asm.b_cond(Cond::Ne, miss);
    asm.emit(mov(X, T, U));
    asm.b(done);

    asm.bind(special);
    asm.emit(movz(X, U, 0x8000, 48));
    asm.emit(cmp(X, T, U, Lsl, 0));
    asm.b_cond(Cond::Ne, miss);
    asm.emit(movz(X, T, 0x7ff0, 48));
    asm.b(done);

    asm.bind(zero);
    asm.cbz(W, true, X3, miss);
    asm.bind(done);
    asm.emit_all(&[ubfx(W, X2, X2, 15, 1), orr(X, T, T, X2, Lsl, 63)]);
}

const PAGE_SHIFT: u32 = adv_cpu_core::memory::PAGE_SIZE.trailing_zeros();
const MEMORY_BASE: u32 = std::mem::offset_of!(runtime::Runtime, memory_base) as u32;
const MEMORY_PAGES: u32 = std::mem::offset_of!(runtime::Runtime, memory_pages) as u32;
const PLAIN_PAGES: u32 = std::mem::offset_of!(runtime::Runtime, plain_pages) as u32;

const X0: Reg = Reg::new(0);
const X1: Reg = Reg::new(1);
const X2: Reg = Reg::new(2);
const X3: Reg = Reg::new(3);
const X4: Reg = Reg::new(4);
const X5: Reg = CPU;
const RT: Reg = Reg::new(29);
const T: Reg = VALUE;
const U: Reg = SCRATCH;

fn immediate(asm: &mut Asm, reg: Reg, value: u32) {
    if value & 0xffff == 0 && value != 0 {
        asm.emit(movz(X, reg, (value >> 16) as u16, 16));
    } else {
        asm.emit(movz(X, reg, value as u16, 0));
        if value >> 16 != 0 {
            asm.emit(movk(X, reg, (value >> 16) as u16, 16));
        }
    }
}

/// Test every page of a statically sized resident span.
pub(crate) fn plain_span(asm: &mut Asm, addr: Reg, bytes: u32, write: bool, miss: Label) {
    let next = asm.label();
    asm.emit_all(&[ldr(W, X0, RT, MEMORY_BASE), cmp(W, addr, X0, Lsl, 0)]);
    asm.b_cond(Cond::Lo, miss);
    asm.emit_all(&[mov(W, X1, addr), sub_ext(X, X1, X1, X0, Extend::Uxtw, 0)]);
    immediate(asm, X2, bytes - 1);
    asm.emit_all(&[
        add(X, X2, X1, X2, Lsl, 0),
        lsr_imm(X, X1, X1, 12),
        lsr_imm(X, X2, X2, 12),
        ldr(W, X3, RT, MEMORY_PAGES),
        cmp(X, X2, X3, Lsl, 0),
    ]);
    asm.b_cond(Cond::Hs, miss);
    asm.emit(ldr(X, X3, RT, PLAIN_PAGES));
    asm.emit(lsr_imm(W, X0, X0, 12));

    asm.bind(next);
    asm.emit_all(&[
        lsr_imm(W, X4, X1, 6),
        add(W, X4, X4, X4, Lsl, 1),
        add_ext(X, X4, X3, X4, Extend::Uxtw, 3),
        ldr(X, X5, X4, if write { 8 } else { 0 }),
        shiftv(X, Lsr, X5, X5, X1),
    ]);
    asm.tbz(false, X5, 0, miss);
    if write {
        asm.emit_all(&[ldr(X, X5, X4, 16), shiftv(X, Lsr, X5, X5, X1)]);
        asm.tbz(true, X5, 0, miss);
    }
    // The guest permission bitmap does not include the host
    // protection intersection when host pages exceed 4 KiB.
    asm.emit_all(&[
        add(W, X5, X0, X1, Lsl, 0),
        ldr(X, X4, RT, runtime::CLASSES),
        ldr_narrow(Narrow::Byte, X5, X4, X5),
    ]);
    if write {
        asm.tbz(false, X5, WRITE.trailing_zeros(), miss);
        asm.tbz(true, X5, adv_cpu_core::memory::EXEC.trailing_zeros(), miss);
    } else {
        asm.tbz(false, X5, READ.trailing_zeros(), miss);
        asm.tbz(true, X5, SHARED.trailing_zeros(), miss);
    }
    asm.emit_all(&[add_imm(W, X1, X1, 1, false), cmp(W, X1, X2, Lsl, 0)]);
    asm.b_cond(Cond::Ls, next);
}

const WINDOW_SPANS: u32 = std::mem::offset_of!(runtime::Runtime, window_spans) as u32;
const LEAF_SHIFT: u32 = PAGE_SHIFT + 6;

fn span_word(asm: &mut Asm, level: usize, backwards: bool, index: Reg, dst: Reg) {
    let words = WINDOW_SPAN_WORDS[level];
    let offset = WINDOW_SPAN_WORDS[..level].iter().sum::<usize>() as u32 * 8;
    let base = if level == 0 {
        X5
    } else {
        immediate(asm, U, offset);
        asm.emit(add(X, U, X5, U, Lsl, 0));
        U
    };
    let index = if backwards && words != 1 {
        asm.emit(eor_imm(X, dst, index, (words - 1) as u64));
        dst
    } else {
        index
    };
    asm.emit(ldr_reg(X, dst, base, index, true));
    if backwards {
        asm.emit(rbit(X, dst, dst));
        if level == WINDOW_SPAN_WORDS.len() - 1 {
            asm.emit(lsr_imm(X, dst, dst, 60));
        }
    } else if level == WINDOW_SPAN_WORDS.len() - 1 {
        asm.emit(and_imm(X, dst, dst, 15));
    }
}

/// Tests the projected inclusive byte range in X0..=X1. On a miss, X2
/// contains the first denied byte. X3 holds the batch count.
pub(crate) fn batch_span(asm: &mut Asm, write: bool, backwards: bool, search: Label, hit: Label) {
    let denied = asm.label();
    let check = asm.label();
    asm.emit_all(&[
        ldr(X, X5, RT, WINDOW_SPANS + 8 * u32::from(write)),
        lsr_imm(X, X0, X0, PAGE_SHIFT),
        lsr_imm(X, X2, X0, 6),
    ]);
    span_word(asm, 0, backwards, X2, X4);
    asm.emit(shiftv(X, Lsr, X4, X4, X0));
    asm.cbz(X, true, X4, denied);
    asm.emit(cmp(X, X2, X1, Lsr, LEAF_SHIFT));
    asm.b_cond(Cond::Eq, hit);

    // Test the next leaf directly before asking the hierarchy for a bound.
    let farther = asm.label();
    asm.emit(add_imm(X, X2, X2, 1, false));
    asm.emit(cmp(X, X2, X1, Lsr, LEAF_SHIFT));
    asm.b_cond(Cond::Ne, farther);
    asm.emit(cmp_imm(X, X2, 1 << 2, true));
    asm.b_cond(Cond::Hs, farther);
    span_word(asm, 0, backwards, X2, X4);
    asm.cbz(X, false, X4, hit);
    asm.emit(lsl_imm(X, X0, X2, 6));
    asm.b(denied);

    asm.bind(farther);
    asm.bl(search);
    asm.b(check);

    asm.bind(denied);
    asm.emit_all(&[
        rbit(X, X4, X4),
        clz(X, X4, X4),
        add(X, X2, X0, X4, Lsl, 0),
        lsl_imm(X, X2, X2, PAGE_SHIFT),
    ]);
    asm.bind(check);
    asm.emit(cmp(X, X2, X1, Lsl, 0));
    asm.b_cond(Cond::Hi, hit);
}

/// Finds the first denied page at or beyond leaf X2 in projected order.
/// The fixed-height tree yields either the byte boundary or 4 GiB.
pub(crate) fn batch_span_search(asm: &mut Asm, entry: Label, backwards: bool) {
    let end = asm.label();
    let finish = asm.label();
    let found = std::array::from_fn::<_, 4, _>(|_| asm.label());
    asm.bind(entry);
    for (level, &found) in found.iter().enumerate() {
        let children = WINDOW_SPAN_WORDS[level.saturating_sub(1)];
        let (limit, shifted) = if children >= 4096 {
            ((children >> 12) as u32, true)
        } else {
            (children as u32, false)
        };
        asm.emit(cmp_imm(X, X2, limit, shifted));
        asm.b_cond(Cond::Hs, end);
        if level == 0 {
            span_word(asm, 0, backwards, X2, X4);
            asm.cbz(X, true, X4, found);
            asm.emit(add_imm(X, X2, X2, 1, false));
        } else {
            asm.emit(lsr_imm(X, T, X2, 6));
            span_word(asm, level, backwards, T, X4);
            asm.emit(shiftv(X, Lsr, X4, X4, X2));
            asm.cbz(X, true, X4, found);
            asm.emit(add_imm(X, X2, T, 1, false));
        }
    }
    asm.bind(end);
    asm.emit(movz(X, X2, 1, 32));
    asm.emit(ret(Reg::LR));
    for (level, &found) in found.iter().enumerate() {
        asm.bind(found);
        if level == 0 {
            asm.emit(lsl_imm(X, X2, X2, 6));
        }
        asm.emit_all(&[rbit(X, X4, X4), clz(X, X4, X4), add(X, X2, X2, X4, Lsl, 0)]);
        for child in (0..level).rev() {
            span_word(asm, child, backwards, X2, X4);
            asm.emit_all(&[rbit(X, X4, X4), clz(X, X4, X4), add(X, X2, X4, X2, Lsl, 6)]);
        }
        asm.b(finish);
    }
    asm.bind(finish);
    asm.emit_all(&[lsl_imm(X, X2, X2, PAGE_SHIFT), ret(Reg::LR)]);
}

#[cfg(test)]
mod tests {
    use super::{Asm, Assumed, CONTROL, ES, Key, MMX, MXCSR, TOP, tags};
    use adv_cpu_core::CpuState;

    // spec: A64-OPT-10
    #[test]
    fn same_leaf_span_hit_cost() {
        use super::{batch_span, ldr_reg};
        for backwards in [false, true] {
            let mut asm = Asm::new();
            let search = asm.label();
            let hit = asm.label();
            batch_span(&mut asm, false, backwards, search, hit);
            asm.bind(search);
            asm.bind(hit);
            let words = asm.finish();
            // The first CBNZ branches over the extension path. The common
            // leaf hits through one of the two following conditional branches.
            let branch = 5 + usize::from(backwards) * 2;
            let imm = (words[branch] >> 5) & 0x7ffff;
            assert_eq!(words[branch] & 0xff00_0000, 0xb500_0000);
            let short = branch + 3;
            let bounded = branch + 7;
            assert!(short <= 10 && bounded <= 14);
            assert!(branch + imm as usize + 6 <= words.len());
            let leaf = ldr_reg(
                super::X,
                super::X4,
                super::X5,
                if backwards { super::X4 } else { super::X2 },
                true,
            );
            assert_eq!(words[..branch].iter().filter(|&&w| w == leaf).count(), 1);
        }
    }

    // spec: A64A-ROLE-3 A64A-FALL-3 A64F-FP-5
    #[test]
    fn joins_keep_agreed_control_bits_and_atomic_indices() {
        let mut cpu = CpuState::default();
        let mask = CONTROL | TOP | tags(1) | MMX | ES | MXCSR;
        let original = Key::read(&cpu, mask);
        cpu.x87.control ^= 0x0200;
        cpu.x87.set_top(3);
        let observed = Key::read(&cpu, mask);
        let joined = original.join(observed);
        // Bitwise join keeps all agreed CONTROL bits (e.g. rounding mode and
        // exception masks) while dropping only the differing PC bit 9 and the
        // atomic 3-bit TOP index.
        assert_eq!(joined.mask(), (mask & !TOP) & !0x0200);
        assert_eq!(joined.control(), None);
        assert_eq!(joined.control_bits().1, 0xffff & !0x0200);
        assert!(original.proves(joined));
        assert!(observed.proves(joined));
    }

    // spec: A64F-FP-5 A64-OPT-4
    #[test]
    fn resident_key_trims_mxcsr_sticky_bits_with_afp_when_masked() {
        let mut cpu = CpuState::default();
        cpu.mxcsr = 0x1fa0; // all masked + PE set
        let assumed = Assumed {
            control: cpu.x87.control,
            status: cpu.x87.status & !0x3800,
            mxcsr: cpu.mxcsr,
            top: cpu.x87.top() as u8,
            tags: cpu.x87.tag,
            esp_low: None,
            ebp: None,
            denormal: false,
            x87: false,
            mmx: false,
            mxcsr_mask: 0xffff,
        };
        // With FEAT_AFP and masked exceptions, only PE (bit 5) stays in the key.
        let afp_key = Key::resident_with_afp(&assumed, 0, true);
        assert_eq!(afp_key.mxcsr_bits().1, 0xffe0);
        for sticky in [0x01u32, 0x02, 0x04, 0x08, 0x10, 0x1f] {
            let mut variant = cpu.clone();
            variant.mxcsr = 0x1fa0 | sticky;
            assert!(
                afp_key.holds(&variant),
                "AFP masked key must accept sticky={sticky:#x} without version bloat"
            );
        }
        let mut no_pe = cpu.clone();
        no_pe.mxcsr = 0x1f80;
        assert!(!afp_key.holds(&no_pe));

        // Without FEAT_AFP, all 6 sticky bits are matched exactly.
        let no_afp_key = Key::resident_with_afp(&assumed, 0, false);
        assert_eq!(no_afp_key.mxcsr_bits().1, 0xffff);
        let mut with_de = cpu.clone();
        with_de.mxcsr = 0x1fa2;
        assert!(!no_afp_key.holds(&with_de));

        // With unmasked exceptions, even with FEAT_AFP all 6 sticky bits stay exact.
        let mut unmasked = assumed;
        unmasked.mxcsr = 0x1f20; // IM unmasked + PE
        let unmasked_key = Key::resident_with_afp(&unmasked, 0, true);
        assert_eq!(unmasked_key.mxcsr_bits().1, 0xffff);
    }

    // spec: A64A-ROLE-3 A64-OPT-4
    #[test]
    fn key_extra_tracks_exact_f32_slots_and_joins_bitwise() {
        use adv_cpu_core::state::X87Register;

        let mut cpu = CpuState::default();
        // Slot 0 holds 1.5 (exact f32), slot 1 holds 1.0 + 2^-40 (f64 only).
        let exact_bits = 1.5f64.to_bits();
        let wide_bits = (1.0f64 + f64::from_bits(0x3d70_0000_0000_0000)).to_bits();
        let to_raw = |bits: u64| {
            let exp = ((bits >> 52) & 0x7ff) as u16 + (16383 - 1023);
            let frac = bits & ((1 << 52) - 1);
            X87Register::from_parts((1 << 63) | (frac << 11), exp)
        };
        cpu.x87.regs[0] = to_raw(exact_bits);
        cpu.x87.regs[1] = to_raw(wide_bits);
        let k0 = Key::default().with_exact_f32(1);
        let k01 = Key::default().with_exact_f32(3);
        assert!(k0.holds(&cpu));
        assert!(!k01.holds(&cpu));
        let joined = k0.join(k01);
        assert!(joined.holds(&cpu));
        assert!(k0.proves(joined));
        assert!(k01.proves(joined));
    }

    // spec: A64A-ROLE-3
    #[test]
    fn resident_key_reads_context_and_exact_x87_inputs() {
        use adv_cpu_core::state::X87Register;

        let mut cpu = CpuState::default();
        cpu.regs[4] = 0x1014;
        cpu.regs[5] = 0x1040;
        cpu.x87.status = 0x4e22;
        cpu.mxcsr |= 0x20;
        let assumed = Assumed {
            control: cpu.x87.control,
            status: cpu.x87.status & !0x3800,
            mxcsr: cpu.mxcsr,
            top: cpu.x87.top() as u8,
            tags: cpu.x87.tag,
            esp_low: Some(4),
            ebp: Some(0x2c),
            denormal: true,
            x87: true,
            mmx: false,
            mxcsr_mask: u32::MAX,
        };
        let key = Key::resident(&assumed, 1);
        assert!(key.holds(&cpu));
        cpu.regs[4] += 16;
        cpu.regs[5] += 16;
        assert!(key.holds(&cpu));

        let mut changed = cpu.clone();
        changed.regs[4] += 4;
        changed.regs[5] += 4;
        assert!(!key.holds(&changed));
        changed = cpu.clone();
        changed.regs[5] += 16;
        assert!(!key.holds(&changed));
        changed = cpu.clone();
        changed.mxcsr ^= 0x20;
        assert!(!key.holds(&changed));
        changed = cpu.clone();
        changed.x87.status ^= 2;
        assert!(!key.holds(&changed));
        changed = cpu.clone();
        changed.x87.regs[0] = X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff);
        assert!(!key.holds(&changed));
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
