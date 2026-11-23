//! Classification and shared conversion helpers for native x87 lowering.
//! Arithmetic bounds keep operands within 2^±RANGE so neither binary64 results
//! nor precision-flag residuals can overflow, underflow or go denormal.
use super::{Memory, NativeX87, reg_index};
use crate::{CpuState, PAGE_SIZE, READ, WRITE, X87Register as Ext};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register as R};
pub mod diagnostics;
pub mod raw;

pub const PE: u16 = 0x20;
pub const C1: u16 = 0x200;
/// Exponent bound for arithmetic operands; see the module comment.
pub const RANGE: i32 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    St(u8),
    F32,
    F64,
    I16,
    I32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fast {
    Wait,
    Load(Source),
    /// FST/FSTP to m32/m64 or FIST/FISTP to m16/m32/m64.
    Store {
        width: u8,
        integer: bool,
        pop: bool,
    },
    StoreSt {
        index: u8,
        pop: bool,
    },
    Arith {
        op: Op,
        dest: u8,
        source: Source,
        reverse: bool,
        pop: bool,
    },
    /// FCOM/FCOMP/FCOMPP/FUCOM*: pops 0, 1 or 2.
    Compare {
        source: Source,
        pops: u8,
    },
    Exchange(u8),
    Negate,
    Absolute,
    Constant(bool),
    LoadControl,
    StoreControl,
    StoreStatus,
    /// Forms lowered by a64; the shared fast helper keeps its transactional path.
    Round,
    Sqrt,
    Scale,
    Examine,
    Load80,
    Store80,
    Leaf(M),
}

fn st(r: R) -> Option<u8> {
    Some(match r {
        R::ST0 => 0,
        R::ST1 => 1,
        R::ST2 => 2,
        R::ST3 => 3,
        R::ST4 => 4,
        R::ST5 => 5,
        R::ST6 => 6,
        R::ST7 => 7,
        _ => return None,
    })
}

/// The register-or-memory operand exactly as the transactional `source` reads it.
fn source(i: &Instruction, integer: bool) -> Option<Source> {
    if i.op0_kind() == OpKind::Memory {
        return match (i.memory_size().size(), integer) {
            (4, false) => Some(Source::F32),
            (8, false) => Some(Source::F64),
            (2, true) => Some(Source::I16),
            (4, true) => Some(Source::I32),
            _ => None,
        };
    }
    let register = if i.op_count() == 2 {
        i.op1_register()
    } else {
        i.op0_register()
    };
    Some(Source::St(st(register).unwrap_or(1)))
}

pub fn classify(i: &Instruction) -> Option<Fast> {
    if i.has_lock_prefix() {
        return None;
    }
    let m = i.mnemonic();
    let memory = i.op0_kind() == OpKind::Memory;
    Some(match m {
        M::Wait => Fast::Wait,
        M::Frndint => Fast::Round,
        M::Fsqrt => Fast::Sqrt,
        M::Fscale => Fast::Scale,
        M::Fxam => Fast::Examine,
        M::Fld | M::Fild => Fast::Load(source(i, m == M::Fild)?),
        M::Fst | M::Fstp | M::Fist | M::Fistp if memory => {
            let width = i.memory_size().size() as u8;
            let integer = matches!(m, M::Fist | M::Fistp);
            if !(matches!(width, 4 | 8) || integer && width == 2) {
                return None;
            }
            Fast::Store {
                width,
                integer,
                pop: matches!(m, M::Fstp | M::Fistp),
            }
        }
        M::Fst | M::Fstp => Fast::StoreSt {
            index: st(i.op0_register()).unwrap_or(0),
            pop: m == M::Fstp,
        },
        M::Fadd
        | M::Faddp
        | M::Fiadd
        | M::Fsub
        | M::Fsubp
        | M::Fsubr
        | M::Fsubrp
        | M::Fisub
        | M::Fisubr
        | M::Fmul
        | M::Fmulp
        | M::Fimul
        | M::Fdiv
        | M::Fdivp
        | M::Fdivr
        | M::Fdivrp
        | M::Fidiv
        | M::Fidivr => {
            let pop = matches!(
                m,
                M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp
            );
            let dest = if i.op_count() == 2 {
                st(i.op0_register()).unwrap_or(0)
            } else {
                u8::from(pop)
            };
            let integer = matches!(
                m,
                M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr
            );
            Fast::Arith {
                op: match m {
                    M::Fadd | M::Faddp | M::Fiadd => Op::Add,
                    M::Fmul | M::Fmulp | M::Fimul => Op::Mul,
                    M::Fdiv | M::Fdivp | M::Fdivr | M::Fdivrp | M::Fidiv | M::Fidivr => Op::Div,
                    _ => Op::Sub,
                },
                dest,
                source: source(i, integer)?,
                reverse: matches!(
                    m,
                    M::Fsubr | M::Fsubrp | M::Fisubr | M::Fdivr | M::Fdivrp | M::Fidivr
                ),
                pop,
            }
        }
        M::Fcom | M::Fcomp | M::Fcompp | M::Fucom | M::Fucomp | M::Fucompp => Fast::Compare {
            source: source(i, false)?,
            pops: match m {
                M::Fcom | M::Fucom => 0,
                M::Fcomp | M::Fucomp => 1,
                _ => 2,
            },
        },
        M::Fxch => Fast::Exchange(st(i.op1_register()).unwrap_or(1)),
        M::Fchs => Fast::Negate,
        M::Fabs => Fast::Absolute,
        M::Fldz => Fast::Constant(false),
        M::Fld1 => Fast::Constant(true),
        M::Fldcw => Fast::LoadControl,
        M::Fnstcw | M::Fstcw => Fast::StoreControl,
        M::Fnstsw | M::Fstsw => Fast::StoreStatus,
        _ => return None,
    })
}

/// An exact double within `2^±range`, or a zero.
fn double(v: Ext, range: i32) -> Option<f64> {
    let sign = u64::from(v.sign_exponent() >> 15) << 63;
    let exponent = i32::from(v.sign_exponent() & 0x7fff);
    let significand = v.significand();
    if exponent == 0 && significand == 0 {
        return Some(f64::from_bits(sign));
    }
    let unbiased = exponent - 0x3fff;
    if significand >> 63 == 0
        || significand & 0x7ff != 0
        || !(-1022..=1023).contains(&unbiased)
        || unbiased.abs() > range
    {
        return None;
    }
    let bits = sign | (((unbiased + 1023) as u64) << 52) | ((significand << 1) >> 12);
    Some(f64::from_bits(bits))
}

pub fn bounded(value: f64, range: i32) -> bool {
    value == 0.0 || (((value.to_bits() >> 52) & 0x7ff) as i32 - 1023).abs() <= range
}

/// Exact extended encoding of a zero or normal double.
pub fn extended(value: f64) -> Ext {
    let bits = value.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    if exponent == 0 {
        debug_assert_eq!(bits << 1, 0);
        return Ext::from_parts(0, sign);
    }
    Ext::from_parts(
        1 << 63 | (bits & ((1 << 52) - 1)) << 11,
        sign | (exponent - 1023 + 0x3fff) as u16,
    )
}

/// Exact `a * b - fl(a * b)`; operands are within `2^±RANGE`.
#[inline]
fn product_error(a: f64, b: f64, p: f64) -> f64 {
    a.mul_add(b, -p)
}

/// The rounded result and the sign of `exact - rounded` (0 when exact).
fn arithmetic(op: Op, a: f64, b: f64) -> (f64, f64) {
    match op {
        Op::Add | Op::Sub => {
            let b = if op == Op::Sub { -b } else { b };
            let s = a + b;
            let bb = s - a;
            (s, (a - (s - bb)) + (b - bb))
        }
        Op::Mul => {
            let p = a * b;
            (p, product_error(a, b, p))
        }
        Op::Div => {
            let q = a / b;
            // r = a - q*b is computed exactly enough to give its sign; the
            // exact quotient minus q has the sign of r/b.
            let r = (a - q * b) - product_error(q, b, q * b);
            // Preserve a nonzero residual instead of dividing it a second
            // time. Only the sign of exact - rounded is consumed by callers.
            let sign = f64::from_bits(r.to_bits() ^ (b.to_bits() & (1 << 63)));
            (q, r.abs().copysign(sign))
        }
    }
}

fn accessible(memory: &Memory, address: u32, len: usize, write: bool) -> bool {
    if u64::from(address) + len as u64 > 1 << 32 {
        return false;
    }
    let Ok(range) = memory.committed(address, len) else {
        return false;
    };
    let need = if write { WRITE } else { READ };
    memory.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
        .iter()
        .all(|p| p & need != 0)
}

fn address(i: &Instruction, state: &CpuState) -> Option<u32> {
    let register = |r: R| {
        reg_index(r).map(|slot| state.regs[slot] & if r.size() == 2 { 0xffff } else { u32::MAX })
    };
    let mut address = i.memory_displacement32();
    if i.memory_base() != R::None {
        address = address.wrapping_add(register(i.memory_base())?);
    }
    if i.memory_index() != R::None {
        address =
            address.wrapping_add(register(i.memory_index())?.wrapping_mul(i.memory_index_scale()));
    }
    if i.memory_base().size() == 2 || i.memory_index().size() == 2 {
        address &= 0xffff;
    }
    if i.segment_prefix() == R::FS {
        address = address.wrapping_add(state.fs_base);
    }
    Some(address)
}

fn read<const N: usize>(memory: &Memory, address: u32) -> Option<[u8; N]> {
    if let Some(pointer) = memory.direct(address, N, false) {
        // SAFETY: `direct` bounds the range to one owned ordinary page.
        return Some(unsafe { pointer.cast::<[u8; N]>().read_unaligned() });
    }
    // A lazy file read may fail observably. Returning None after attempting it
    // would retry I/O inside the same instruction and could swallow the fault.
    // Shared/file/audio accesses therefore belong to the transaction, not this
    // speculative fast path. Ordinary cross-page reads remain eligible.
    if memory.shared_range(address, N) || !accessible(memory, address, N, false) {
        return None;
    }
    memory.read_data(address, N).ok()?.as_ref().try_into().ok()
}

/// The value of `source` and, for memory, its address. `range` bounds the
/// exponent of register operands.
fn load(
    source: Source,
    i: &Instruction,
    state: &CpuState,
    memory: &Memory,
    range: i32,
) -> Option<f64> {
    let x87 = &state.x87;
    if let Source::St(index) = source {
        let slot = x87.physical_index(usize::from(index));
        if x87.physical_tag(slot) == 3 {
            return None;
        }
        return double(x87.regs[slot], range);
    }
    let at = address(i, state)?;
    let value = match source {
        Source::F32 => {
            let bits = u32::from_le_bytes(read(memory, at)?);
            // Zero or normal; denormals, infinities and NaNs signal or quiet.
            if bits & 0x7fff_ffff != 0 && !matches!(bits >> 23 & 0xff, 1..=254) {
                return None;
            }
            f64::from(f32::from_bits(bits))
        }
        Source::F64 => {
            let bits = u64::from_le_bytes(read(memory, at)?);
            if bits << 1 != 0 && !matches!(bits >> 52 & 0x7ff, 1..=2046) {
                return None;
            }
            f64::from_bits(bits)
        }
        Source::I16 => f64::from(i16::from_le_bytes(read(memory, at)?)),
        Source::I32 => f64::from(i32::from_le_bytes(read(memory, at)?)),
        Source::St(_) => unreachable!(),
    };
    // Memory operands of arithmetic obey the same bound as registers.
    bounded(value, range).then_some(value)
}

/// Fraction bits of a binary64 below a 24-bit significand.
pub const SINGLE_TAIL: u64 = (1 << 29) - 1;

/// Round a zero or normal binary64 to a 24-bit significand, nearest-even,
/// keeping its exponent (x87 PC=24 does not narrow the exponent range).
/// Returns the result and whether its magnitude grew; unchanged when exact.
pub fn round_single_precision(value: f64) -> (f64, bool) {
    let bits = value.to_bits();
    let tail = bits & SINGLE_TAIL;
    let up = tail + (bits >> 29 & 1) > 1 << 28;
    let rounded = (bits & !SINGLE_TAIL) + if up { 1 << 29 } else { 0 };
    (f64::from_bits(rounded), up)
}
