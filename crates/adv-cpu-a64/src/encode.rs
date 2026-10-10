//! AArch64 instruction encoders.
//!
//! Each encoder is a `const fn` returning one instruction word, so the
//! translator pays only for shifts and ors. Field ranges are asserted:
//! a silently truncated field would be a wrong instruction.

/// A general-purpose register number. 31 is the zero register or SP,
/// depending on the instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reg(u8);

impl Reg {
    pub const fn new(number: u8) -> Self {
        assert!(number < 32);
        Self(number)
    }

    pub const fn number(self) -> u32 {
        self.0 as u32
    }

    /// The link register.
    pub const LR: Self = Self(30);
    /// The zero register in data-processing operands.
    pub const ZR: Self = Self(31);
}

/// Operand width: the `sf` bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    W,
    X,
}

impl Size {
    const fn sf(self) -> u32 {
        match self {
            Self::W => 0,
            Self::X => 1 << 31,
        }
    }

    const fn bits(self) -> u32 {
        match self {
            Self::W => 32,
            Self::X => 64,
        }
    }
}

/// Shift applied to the second register operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
}

const fn wide(base: u32, size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    assert!(shift.is_multiple_of(16) && shift < size.bits());
    base | size.sf() | (shift / 16) << 21 | (imm as u32) << 5 | rd.number()
}

/// `movz rd, #imm, lsl #shift`.
pub const fn movz(size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    wide(0x5280_0000, size, rd, imm, shift)
}

/// `movk rd, #imm, lsl #shift`.
pub const fn movk(size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    wide(0x7280_0000, size, rd, imm, shift)
}

const fn shifted(
    base: u32,
    size: Size,
    rd: Reg,
    rn: Reg,
    rm: Reg,
    shift: Shift,
    amount: u32,
) -> u32 {
    assert!(amount < size.bits());
    base | size.sf()
        | (shift as u32) << 22
        | rm.number() << 16
        | amount << 10
        | rn.number() << 5
        | rd.number()
}

/// `add rd, rn, rm, <shift> #amount`.
pub const fn add(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x0B00_0000, size, rd, rn, rm, shift, amount)
}

/// `sub rd, rn, rm, <shift> #amount`.
pub const fn sub(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x4B00_0000, size, rd, rn, rm, shift, amount)
}

/// `cmp rn, rm, <shift> #amount`, which sets NZCV in subtraction sense.
pub const fn cmp(size: Size, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x6B00_0000, size, Reg::ZR, rn, rm, shift, amount)
}

/// `cmn rn, rm, <shift> #amount`.
pub const fn cmn(size: Size, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x2B00_0000, size, Reg::ZR, rn, rm, shift, amount)
}

/// `and rd, rn, rm, <shift> #amount`.
pub const fn and(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x0A00_0000, size, rd, rn, rm, shift, amount)
}

/// `bic rd, rn, rm, <shift> #amount`.
pub const fn bic(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x0A20_0000, size, rd, rn, rm, shift, amount)
}

/// `orr rd, rn, rm, <shift> #amount`.
pub const fn orr(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x2A00_0000, size, rd, rn, rm, shift, amount)
}

/// `eor rd, rn, rm, <shift> #amount`.
pub const fn eor(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x4A00_0000, size, rd, rn, rm, shift, amount)
}

/// `mov rd, rm` between general registers (not SP).
pub const fn mov(size: Size, rd: Reg, rm: Reg) -> u32 {
    orr(size, rd, Reg::ZR, rm, Shift::Lsl, 0)
}

const fn immediate(base: u32, size: Size, rd: Reg, rn: Reg, imm: u32, shift12: bool) -> u32 {
    assert!(imm < 1 << 12);
    base | size.sf() | (shift12 as u32) << 22 | imm << 10 | rn.number() << 5 | rd.number()
}

/// `add rd, rn, #imm{, lsl #12}`; register 31 is SP on both sides.
pub const fn add_imm(size: Size, rd: Reg, rn: Reg, imm: u32, shift12: bool) -> u32 {
    immediate(0x1100_0000, size, rd, rn, imm, shift12)
}

/// `sub rd, rn, #imm{, lsl #12}`; register 31 is SP on both sides.
pub const fn sub_imm(size: Size, rd: Reg, rn: Reg, imm: u32, shift12: bool) -> u32 {
    immediate(0x5100_0000, size, rd, rn, imm, shift12)
}

/// `cmp rn, #imm{, lsl #12}`.
pub const fn cmp_imm(size: Size, rn: Reg, imm: u32, shift12: bool) -> u32 {
    immediate(0x7100_0000, size, Reg::ZR, rn, imm, shift12)
}

/// Register extension of an extended-register operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extend {
    Uxtw = 2,
    Uxtx = 3,
}

/// `add rd, rn, rm, <extend> #amount`, as in the window form
/// `add x23, x27, w16, uxtw`. Register 31 is SP for `rd` and `rn`.
pub const fn add_ext(size: Size, rd: Reg, rn: Reg, rm: Reg, extend: Extend, amount: u32) -> u32 {
    assert!(amount <= 4);
    0x0B20_0000
        | size.sf()
        | rm.number() << 16
        | (extend as u32) << 13
        | amount << 10
        | rn.number() << 5
        | rd.number()
}

/// `mov rd, rn` where either side may be SP.
pub const fn mov_sp(rd: Reg, rn: Reg) -> u32 {
    add_imm(Size::X, rd, rn, 0, false)
}

/// The `N:immr:imms` field of a logical immediate, if `imm` has one.
pub const fn bitmask(size: Size, imm: u64) -> Option<u32> {
    let imm = match size {
        Size::W if imm >> 32 != 0 => return None,
        Size::W => imm | imm << 32,
        Size::X => imm,
    };
    if imm == 0 || imm == u64::MAX {
        return None;
    }
    // The smallest element whose repetition gives `imm`.
    let mut element = 64;
    while element > 2 {
        let half = element / 2;
        let mask = (1u64 << half) - 1;
        if imm & mask != (imm >> half) & mask {
            break;
        }
        element = half;
    }
    let mask = if element == 64 {
        u64::MAX
    } else {
        (1u64 << element) - 1
    };
    let value = imm & mask;
    let ones = value.count_ones();
    // Rotate right until the run of ones sits at the bottom.
    let mut rotation = 0;
    while rotation < element {
        let rotated = if rotation == 0 {
            value
        } else {
            (value >> rotation | value << (element - rotation)) & mask
        };
        if rotated == (1u64 << ones) - 1 {
            let immr = (element - rotation) % element;
            let n = (element == 64) as u32;
            let imms = (!((element << 1) - 1) & 0x3f) | (ones - 1);
            return Some(n << 12 | immr << 6 | imms);
        }
        rotation += 1;
    }
    None
}

const fn logical(base: u32, size: Size, rd: Reg, rn: Reg, imm: u64) -> u32 {
    let field = match bitmask(size, imm) {
        Some(field) => field,
        None => panic!("not a logical immediate"),
    };
    base | size.sf() | field << 10 | rn.number() << 5 | rd.number()
}

/// `and rd, rn, #imm`; panics unless `imm` is a logical immediate.
pub const fn and_imm(size: Size, rd: Reg, rn: Reg, imm: u64) -> u32 {
    logical(0x1200_0000, size, rd, rn, imm)
}

/// `orr rd, rn, #imm`; panics unless `imm` is a logical immediate.
pub const fn orr_imm(size: Size, rd: Reg, rn: Reg, imm: u64) -> u32 {
    logical(0x3200_0000, size, rd, rn, imm)
}

/// `eor rd, rn, #imm`; panics unless `imm` is a logical immediate.
pub const fn eor_imm(size: Size, rd: Reg, rn: Reg, imm: u64) -> u32 {
    logical(0x5200_0000, size, rd, rn, imm)
}

const fn bitfield(base: u32, size: Size, rd: Reg, rn: Reg, immr: u32, imms: u32) -> u32 {
    assert!(immr < size.bits() && imms < size.bits());
    let n = match size {
        Size::W => 0,
        Size::X => 1 << 22,
    };
    base | size.sf() | n | immr << 16 | imms << 10 | rn.number() << 5 | rd.number()
}

/// `ubfx rd, rn, #lsb, #width`.
pub const fn ubfx(size: Size, rd: Reg, rn: Reg, lsb: u32, width: u32) -> u32 {
    assert!(width > 0 && lsb + width <= size.bits());
    bitfield(0x5300_0000, size, rd, rn, lsb, lsb + width - 1)
}

/// `bfi rd, rn, #lsb, #width`.
pub const fn bfi(size: Size, rd: Reg, rn: Reg, lsb: u32, width: u32) -> u32 {
    assert!(width > 0 && lsb + width <= size.bits());
    let bits = size.bits();
    bitfield(0x3300_0000, size, rd, rn, (bits - lsb) % bits, width - 1)
}

/// `lsl rd, rn, #shift`.
pub const fn lsl_imm(size: Size, rd: Reg, rn: Reg, shift: u32) -> u32 {
    assert!(shift < size.bits());
    let bits = size.bits();
    bitfield(
        0x5300_0000,
        size,
        rd,
        rn,
        (bits - shift) % bits,
        bits - 1 - shift,
    )
}

/// `lsr rd, rn, #shift`.
pub const fn lsr_imm(size: Size, rd: Reg, rn: Reg, shift: u32) -> u32 {
    bitfield(0x5300_0000, size, rd, rn, shift, size.bits() - 1)
}

/// A SIMD and floating-point register number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VReg(u8);

impl VReg {
    pub const fn new(number: u8) -> Self {
        assert!(number < 32);
        Self(number)
    }

    pub const fn number(self) -> u32 {
        self.0 as u32
    }
}

/// Width of a SIMD and floating-point transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VSize {
    D,
    Q,
}

impl VSize {
    const fn bytes(self) -> u32 {
        match self {
            Self::D => 8,
            Self::Q => 16,
        }
    }
}

const fn unsigned_offset(base: u32, scale: u32, rt: u32, rn: Reg, offset: u32) -> u32 {
    assert!(offset.is_multiple_of(scale) && offset / scale < 1 << 12);
    base | (offset / scale) << 10 | rn.number() << 5 | rt
}

/// `ldr rt, [rn, #offset]`; `rn` 31 is SP.
pub const fn ldr(size: Size, rt: Reg, rn: Reg, offset: u32) -> u32 {
    match size {
        Size::W => unsigned_offset(0xB940_0000, 4, rt.number(), rn, offset),
        Size::X => unsigned_offset(0xF940_0000, 8, rt.number(), rn, offset),
    }
}

/// `str rt, [rn, #offset]`; `rn` 31 is SP.
pub const fn str(size: Size, rt: Reg, rn: Reg, offset: u32) -> u32 {
    match size {
        Size::W => unsigned_offset(0xB900_0000, 4, rt.number(), rn, offset),
        Size::X => unsigned_offset(0xF900_0000, 8, rt.number(), rn, offset),
    }
}

/// `ldr rt, [rn, wm, uxtw]`, or scaled by the access size with `scaled`:
/// the window form of a guest access.
pub const fn ldr_reg(size: Size, rt: Reg, rn: Reg, rm: Reg, scaled: bool) -> u32 {
    register_offset(0xB860_4800, size, rt, rn, rm, scaled)
}

/// `str rt, [rn, wm, uxtw]`, or scaled by the access size with `scaled`.
pub const fn str_reg(size: Size, rt: Reg, rn: Reg, rm: Reg, scaled: bool) -> u32 {
    register_offset(0xB820_4800, size, rt, rn, rm, scaled)
}

const fn register_offset(base: u32, size: Size, rt: Reg, rn: Reg, rm: Reg, scaled: bool) -> u32 {
    let size = match size {
        Size::W => 0,
        Size::X => 1 << 30,
    };
    base | size | rm.number() << 16 | (scaled as u32) << 12 | rn.number() << 5 | rt.number()
}

/// `ldur rt, [rn, #offset]` with an unscaled signed offset.
pub const fn ldur(size: Size, rt: Reg, rn: Reg, offset: i32) -> u32 {
    unscaled(0xB840_0000, size, rt, rn, offset)
}

/// `stur rt, [rn, #offset]` with an unscaled signed offset.
pub const fn stur(size: Size, rt: Reg, rn: Reg, offset: i32) -> u32 {
    unscaled(0xB800_0000, size, rt, rn, offset)
}

const fn unscaled(base: u32, size: Size, rt: Reg, rn: Reg, offset: i32) -> u32 {
    assert!(offset >= -256 && offset < 256);
    let size = match size {
        Size::W => 0,
        Size::X => 1 << 30,
    };
    base | size | (offset as u32 & 0x1ff) << 12 | rn.number() << 5 | rt.number()
}

/// `ldr dt/qt, [rn, #offset]`.
pub const fn ldr_v(size: VSize, rt: VReg, rn: Reg, offset: u32) -> u32 {
    match size {
        VSize::D => unsigned_offset(0xFD40_0000, 8, rt.number(), rn, offset),
        VSize::Q => unsigned_offset(0x3DC0_0000, 16, rt.number(), rn, offset),
    }
}

/// `str dt/qt, [rn, #offset]`.
pub const fn str_v(size: VSize, rt: VReg, rn: Reg, offset: u32) -> u32 {
    match size {
        VSize::D => unsigned_offset(0xFD00_0000, 8, rt.number(), rn, offset),
        VSize::Q => unsigned_offset(0x3D80_0000, 16, rt.number(), rn, offset),
    }
}

/// Addressing of a register pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Index {
    /// `[rn], #offset`.
    Post = 1,
    /// `[rn, #offset]`.
    Offset = 2,
    /// `[rn, #offset]!`.
    Pre = 3,
}

const fn pair(base: u32, scale: u32, index: Index, rt: u32, rt2: u32, rn: Reg, offset: i32) -> u32 {
    assert!(offset % scale as i32 == 0);
    let scaled = offset / scale as i32;
    assert!(scaled >= -64 && scaled < 64);
    base | (index as u32) << 23 | (scaled as u32 & 0x7f) << 15 | rt2 << 10 | rn.number() << 5 | rt
}

/// `stp rt, rt2` with `index` addressing; `rn` 31 is SP.
pub const fn stp(size: Size, index: Index, rt: Reg, rt2: Reg, rn: Reg, offset: i32) -> u32 {
    match size {
        Size::W => pair(0x2800_0000, 4, index, rt.number(), rt2.number(), rn, offset),
        Size::X => pair(0xA800_0000, 8, index, rt.number(), rt2.number(), rn, offset),
    }
}

/// `ldp rt, rt2` with `index` addressing; `rn` 31 is SP.
pub const fn ldp(size: Size, index: Index, rt: Reg, rt2: Reg, rn: Reg, offset: i32) -> u32 {
    stp(size, index, rt, rt2, rn, offset) | 1 << 22
}

/// `stp dt/qt, dt2/qt2` with `index` addressing.
pub const fn stp_v(size: VSize, index: Index, rt: VReg, rt2: VReg, rn: Reg, offset: i32) -> u32 {
    let base = match size {
        VSize::D => 0x6C00_0000,
        VSize::Q => 0xAC00_0000,
    };
    pair(
        base,
        size.bytes(),
        index,
        rt.number(),
        rt2.number(),
        rn,
        offset,
    )
}

/// `ldp dt/qt, dt2/qt2` with `index` addressing.
pub const fn ldp_v(size: VSize, index: Index, rt: VReg, rt2: VReg, rn: Reg, offset: i32) -> u32 {
    stp_v(size, index, rt, rt2, rn, offset) | 1 << 22
}

/// Condition codes, by encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq = 0,
    Ne,
    Hs,
    Lo,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
}

const fn displacement(offset: i32, bits: u32) -> u32 {
    assert!(offset % 4 == 0);
    let words = offset / 4;
    assert!(words >= -(1 << (bits - 1)) && words < 1 << (bits - 1));
    words as u32 & ((1 << bits) - 1)
}

/// `b` to `offset` bytes from this instruction.
pub const fn b(offset: i32) -> u32 {
    0x1400_0000 | displacement(offset, 26)
}

/// `bl` to `offset` bytes from this instruction.
pub const fn bl(offset: i32) -> u32 {
    0x9400_0000 | displacement(offset, 26)
}

/// `b.cond` to `offset` bytes from this instruction.
pub const fn b_cond(cond: Cond, offset: i32) -> u32 {
    0x5400_0000 | displacement(offset, 19) << 5 | cond as u32
}

/// `cbz` (or `cbnz` when `nonzero`) to `offset` bytes from this instruction.
pub const fn cbz(size: Size, nonzero: bool, rt: Reg, offset: i32) -> u32 {
    0x3400_0000 | size.sf() | (nonzero as u32) << 24 | displacement(offset, 19) << 5 | rt.number()
}

/// `tbz` (or `tbnz` when `nonzero`) on `bit` to `offset` bytes from this
/// instruction.
pub const fn tbz(nonzero: bool, rt: Reg, bit: u32, offset: i32) -> u32 {
    assert!(bit < 64);
    0x3600_0000
        | (bit >> 5) << 31
        | (nonzero as u32) << 24
        | (bit & 31) << 19
        | displacement(offset, 14) << 5
        | rt.number()
}

/// `adr rd`, `offset` bytes from this instruction.
pub const fn adr(rd: Reg, offset: i32) -> u32 {
    assert!(offset >= -(1 << 20) && offset < 1 << 20);
    let imm = offset as u32;
    0x1000_0000 | (imm & 3) << 29 | (imm >> 2 & 0x7ffff) << 5 | rd.number()
}

/// `br rn`.
pub const fn br(rn: Reg) -> u32 {
    0xD61F_0000 | rn.number() << 5
}

/// `blr rn`.
pub const fn blr(rn: Reg) -> u32 {
    0xD63F_0000 | rn.number() << 5
}

/// `mrs rt, nzcv`.
pub const fn mrs_nzcv(rt: Reg) -> u32 {
    0xD53B_4200 | rt.number()
}

/// `msr nzcv, rt`.
pub const fn msr_nzcv(rt: Reg) -> u32 {
    0xD51B_4200 | rt.number()
}

/// `ret rn`.
pub const fn ret(rn: Reg) -> u32 {
    0xD65F_0000 | rn.number() << 5
}

/// `brk #imm`.
pub const fn brk(imm: u16) -> u32 {
    0xD420_0000 | (imm as u32) << 5
}

/// `nop`.
pub const NOP: u32 = 0xD503_201F;

#[cfg(test)]
mod tests {
    use super::Extend::*;
    use super::Shift::*;
    use super::Size::*;
    use super::*;

    const fn r(number: u8) -> Reg {
        Reg::new(number)
    }

    const fn v(number: u8) -> VReg {
        VReg::new(number)
    }

    const SP: Reg = Reg::new(31);

    /// Every case pairs an encoder call with the assembly clang encoded.
    fn cases() -> Vec<(u32, &'static str)> {
        vec![
            (movz(W, r(19), 0xffff, 16), "movz w19, #0xffff, lsl #16"),
            (movz(X, r(0), 0x1234, 0), "movz x0, #0x1234"),
            (movz(X, r(27), 1, 48), "movz x27, #0x1, lsl #48"),
            (movk(W, r(1), 7, 0), "movk w1, #0x7"),
            (movk(X, r(0), 0xbeef, 32), "movk x0, #0xbeef, lsl #32"),
            (add(W, r(19), r(19), r(20), Lsl, 0), "add w19, w19, w20"),
            (add(X, r(23), r(27), r(23), Lsl, 0), "add x23, x27, x23"),
            (add(X, r(0), r(1), r(2), Lsl, 3), "add x0, x1, x2, lsl #3"),
            (sub(W, r(0), r(1), r(2), Asr, 31), "sub w0, w1, w2, asr #31"),
            (sub(X, r(5), r(6), r(7), Lsr, 12), "sub x5, x6, x7, lsr #12"),
            (ret(Reg::LR), "ret"),
            (ret(r(1)), "ret x1"),
            (brk(0), "brk #0x0"),
            (brk(0xf00d), "brk #0xf00d"),
            (NOP, "nop"),
            (
                ldr_reg(W, r(19), r(27), r(20), false),
                "ldr w19, [x27, w20, uxtw]",
            ),
            (
                ldr_reg(X, r(0), r(27), r(16), true),
                "ldr x0, [x27, w16, uxtw #3]",
            ),
            (
                str_reg(W, r(1), r(27), r(16), false),
                "str w1, [x27, w16, uxtw]",
            ),
            (
                str_reg(W, r(1), r(27), r(16), true),
                "str w1, [x27, w16, uxtw #2]",
            ),
            (ldur(W, r(19), r(23), -4), "ldur w19, [x23, #-4]"),
            (ldur(X, r(0), r(24), 255), "ldur x0, [x24, #255]"),
            (stur(W, r(19), r(23), -256), "stur w19, [x23, #-256]"),
            (cmp(W, r(20), r(21), Lsl, 0), "cmp w20, w21"),
            (cmn(X, r(0), r(1), Lsl, 2), "cmn x0, x1, lsl #2"),
            (and(W, r(0), r(1), r(2), Lsl, 0), "and w0, w1, w2"),
            (bic(W, r(4), r(4), r(5), Lsl, 0), "bic w4, w4, w5"),
            (orr(W, r(3), r(3), r(4), Lsl, 11), "orr w3, w3, w4, lsl #11"),
            (eor(W, r(4), r(4), r(4), Lsr, 4), "eor w4, w4, w4, lsr #4"),
            (mov(X, r(17), r(1)), "mov x17, x1"),
            (
                add_imm(W, r(16), r(23), 4095, false),
                "add w16, w23, #0xfff",
            ),
            (add_imm(X, r(0), r(1), 1, true), "add x0, x1, #0x1, lsl #12"),
            (sub_imm(X, SP, SP, 160, false), "sub sp, sp, #0xa0"),
            (cmp_imm(W, r(0), 0x3c, false), "cmp w0, #0x3c"),
            (
                add_ext(X, r(23), r(27), r(16), Uxtw, 0),
                "add x23, x27, w16, uxtw",
            ),
            (
                add_ext(X, r(0), r(27), r(1), Uxtw, 2),
                "add x0, x27, w1, uxtw #2",
            ),
            (mov_sp(r(16), SP), "mov x16, sp"),
            (mov_sp(SP, r(16)), "mov sp, x16"),
            (and_imm(W, r(3), r(2), 0xc0), "and w3, w2, #0xc0"),
            (and_imm(W, r(4), r(11), 0x10), "and w4, w11, #0x10"),
            (eor_imm(W, r(4), r(4), 1), "eor w4, w4, #0x1"),
            (
                orr_imm(X, r(0), r(1), 0xffff_0000_ffff_0000),
                "orr x0, x1, #0xffff0000ffff0000",
            ),
            (
                and_imm(X, r(5), r(6), 0x5555_5555_5555_5555),
                "and x5, x6, #0x5555555555555555",
            ),
            (
                and_imm(X, r(5), r(6), 0x8000_0000_0000_0001),
                "and x5, x6, #0x8000000000000001",
            ),
            (ubfx(W, r(3), r(1), 11, 1), "ubfx w3, w1, #11, #1"),
            (ubfx(X, r(3), r(1), 32, 32), "lsr x3, x1, #32"),
            (bfi(W, r(19), r(0), 8, 8), "bfi w19, w0, #8, #8"),
            (lsl_imm(W, r(2), r(2), 24), "lsl w2, w2, #24"),
            (lsr_imm(W, r(2), r(1), 24), "lsr w2, w1, #24"),
            (lsl_imm(X, r(0), r(1), 32), "lsl x0, x1, #32"),
            (ldr(X, r(0), r(29), 0), "ldr x0, [x29]"),
            (ldr(W, r(1), r(0), 36), "ldr w1, [x0, #0x24]"),
            (str(X, r(16), r(29), 32), "str x16, [x29, #0x20]"),
            (str(W, r(4), r(0), 16380), "str w4, [x0, #0x3ffc]"),
            (ldr_v(VSize::D, v(8), SP, 8), "ldr d8, [sp, #0x8]"),
            (str_v(VSize::Q, v(16), r(16), 112), "str q16, [x16, #0x70]"),
            (
                stp(X, Index::Pre, r(29), r(30), SP, -160),
                "stp x29, x30, [sp, #-0xa0]!",
            ),
            (
                ldp(X, Index::Post, r(29), r(30), SP, 160),
                "ldp x29, x30, [sp], #0xa0",
            ),
            (
                stp(X, Index::Offset, r(19), r(20), SP, 16),
                "stp x19, x20, [sp, #0x10]",
            ),
            (
                ldp(W, Index::Offset, r(19), r(20), r(0), 0),
                "ldp w19, w20, [x0]",
            ),
            (
                stp(W, Index::Offset, r(23), r(24), r(0), 16),
                "stp w23, w24, [x0, #0x10]",
            ),
            (
                stp_v(VSize::D, Index::Offset, v(8), v(9), SP, 96),
                "stp d8, d9, [sp, #0x60]",
            ),
            (
                ldp_v(VSize::Q, Index::Offset, v(22), v(23), r(16), 96),
                "ldp q22, q23, [x16, #0x60]",
            ),
            (b(8), "b #8"),
            (b(-0x800_0000), "b #-134217728"),
            (bl(0x7ff_fffc), "bl #134217724"),
            (b_cond(Cond::Ne, -8), "b.ne #-8"),
            (b_cond(Cond::Le, 0x100), "b.le #256"),
            (cbz(W, false, r(1), 16), "cbz w1, #16"),
            (cbz(X, true, r(9), -4), "cbnz x9, #-4"),
            (tbz(true, r(9), 63, 32), "tbnz x9, #63, #32"),
            (tbz(false, r(1), 4, -32), "tbz w1, #4, #-32"),
            (adr(r(17), 12), "adr x17, #12"),
            (adr(r(0), -1), "adr x0, #-1"),
            (br(r(17)), "br x17"),
            (blr(r(16)), "blr x16"),
            (mrs_nzcv(r(1)), "mrs x1, NZCV"),
            (msr_nzcv(r(2)), "msr NZCV, x2"),
        ]
    }

    #[test]
    fn encoders_match_clang_vectors_and_cover_every_vector() {
        let text = include_str!("../testdata/encode.tsv");
        let mut vectors = std::collections::HashMap::new();
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (word, asm) = line.split_once('\t').expect("word<TAB>assembly");
            let word = u32::from_str_radix(word, 16).expect("hex word");
            assert!(vectors.insert(asm, word).is_none(), "duplicate {asm}");
        }
        for (word, asm) in cases() {
            let expected = vectors
                .remove(asm)
                .unwrap_or_else(|| panic!("no vector for {asm}"));
            assert_eq!(word, expected, "{asm}: {word:08x} != {expected:08x}");
        }
        assert!(
            vectors.is_empty(),
            "vectors without a case: {:?}",
            vectors.keys()
        );
    }

    #[test]
    fn bitmask_rejects_values_without_a_rotated_run() {
        for (size, imm) in [
            (W, 0),
            (W, 0xffff_ffff),
            (W, 0xffff_f12a),
            (W, 1 << 32),
            (X, 0x5),
        ] {
            assert_eq!(bitmask(size, imm), None, "{imm:#x}");
        }
    }

    #[test]
    #[should_panic]
    fn out_of_range_shift_is_rejected() {
        let _ = add(W, r(0), r(0), r(0), Lsl, 32);
    }
}
