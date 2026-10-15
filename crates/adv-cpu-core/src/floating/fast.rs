//! Host-double execution of the x87 operations that dominate compiled float
//! loops. It applies only where the result is bit-identical to the
//! transactional path: no pending exception, live MMX state committed, the
//! touched stack slots valid. Copies and comparisons keep ordinary raw80;
//! stores round its significand once. Arithmetic requires exact doubles and
//! also needs round-to-nearest and masked exceptions, and operands within 2^±400
//! so neither the result nor the error terms used for the precision flag can
//! overflow, underflow or go denormal. Precision control 53 uses the binary64
//! result directly. Precision control 24 additionally needs 24-bit operands:
//! rounding their binary64 result again to 24 bits is then the correctly rounded
//! result (53 >= 2 * 24 + 2), with the extended exponent range kept. Everything
//! at PC=64 requires an exact result, including signed zero; division also
//! bounds the quotient so its product residual cannot underflow;
//! other arithmetic cases return `None` before state changes and take the transaction.
use super::{Memory, NativeX87, X87_DONE, X87_EXIT, reg_index};
use crate::{CpuState, PAGE_SIZE, READ, WRITE, X87Register as Ext};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register as R};
pub mod diagnostics;
mod extra;
pub mod raw;
pub use extra::execute as execute_other;

pub const PE: u16 = 0x20;
const C0: u16 = 0x100;
pub const C1: u16 = 0x200;
const C2: u16 = 0x400;
const C3: u16 = 0x4000;
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
    /// FST/FSTP to m32/m64 (width 4/8) or FIST/FISTP to m32/m64 (integer).
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
    Scale,
    Examine,
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
    if i.has_lock_prefix() || i.segment_prefix() == R::GS {
        return None;
    }
    let m = i.mnemonic();
    let memory = i.op0_kind() == OpKind::Memory;
    Some(match m {
        M::Wait => Fast::Wait,
        M::Frndint => Fast::Round,
        M::Fscale => Fast::Scale,
        M::Fxam => Fast::Examine,
        M::Fld | M::Fild => Fast::Load(source(i, m == M::Fild)?),
        M::Fst | M::Fstp | M::Fist | M::Fistp if memory => {
            let width = i.memory_size().size() as u8;
            let integer = matches!(m, M::Fist | M::Fistp);
            if !matches!(width, 4 | 8) {
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

fn split(a: f64) -> (f64, f64) {
    let c = 134_217_729.0 * a;
    let high = c - (c - a);
    (high, a - high)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "fma")]
fn fused_error(a: f64, b: f64, p: f64) -> f64 {
    a.mul_add(b, -p)
}

/// Exact `a * b - fl(a * b)`; operands are within `2^±RANGE`.
#[inline]
fn product_error(a: f64, b: f64, p: f64) -> f64 {
    #[cfg(target_arch = "aarch64")]
    return a.mul_add(b, -p);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU supports FMA.
        return unsafe { fused_error(a, b, p) };
    }
    #[allow(unreachable_code)]
    dekker_error(a, b, p)
}

pub fn dekker_error(a: f64, b: f64, p: f64) -> f64 {
    let (ah, al) = split(a);
    let (bh, bl) = split(b);
    ((ah * bh - p) + ah * bl + al * bh) + al * bl
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

fn writable(memory: &Memory, address: u32, len: usize) -> bool {
    memory.direct(address, len, true).is_some()
        || (!memory.shared_range(address, len) && accessible(memory, address, len, true))
}

/// Store to a location `writable` accepted.
fn store(memory: &mut Memory, address: u32, bytes: &[u8]) -> Option<()> {
    if let Some(pointer) = memory.direct(address, bytes.len(), true) {
        // SAFETY: `direct` bounds the range to one owned ordinary page.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
        return Some(());
    }
    memory.write(address, bytes).ok()
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
) -> Option<(f64, Option<u32>)> {
    let x87 = &state.x87;
    if let Source::St(index) = source {
        let slot = x87.physical_index(usize::from(index));
        if x87.physical_tag(slot) == 3 {
            return None;
        }
        return Some((double(x87.regs[slot], range)?, None));
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
    bounded(value, range).then_some((value, Some(at)))
}

fn rounded_up(error: f64, result: f64) -> bool {
    error != 0.0 && error.is_sign_negative() != result.is_sign_negative()
}

/// Precision control of an arithmetic-eligible control word: round to nearest,
/// every exception masked, and PC=24 or PC=53. PC=64 is admitted separately
/// only when a result is exact; rejections retain the Control diagnostic class.
pub fn arithmetic_precision(control: u16) -> Option<u32> {
    match control & 0x0f3f {
        0x023f => Some(53),
        0x003f => Some(24),
        _ => None,
    }
}

/// Probe only rejected PC=64/nearest/masked calls, without changing state or
/// faulting in file-backed memory. Range and zero are separate from candidates.
pub fn pc64_opportunity(
    i: &Instruction,
    fast: Fast,
    state: &CpuState,
    memory: &Memory,
) -> crate::cliffs::X87Pc64 {
    use crate::cliffs::X87Pc64;
    let Fast::Arith {
        op,
        dest,
        source,
        reverse,
        ..
    } = fast
    else {
        return X87Pc64::Other;
    };
    // Keep division in Other for compatibility with this legacy add/sub/mul
    // census. The opt-in form census reports division rejections separately.
    if op == Op::Div {
        return X87Pc64::Other;
    }
    let target = state.x87.physical_index(usize::from(dest));
    if state.x87.physical_tag(target) == 3 {
        return X87Pc64::Operand;
    }
    let Some(mut a) = double(state.x87.regs[target], i32::MAX) else {
        return X87Pc64::Operand;
    };
    let Some((mut b, _)) = load(source, i, state, memory, i32::MAX) else {
        return X87Pc64::Operand;
    };
    if !bounded(a, RANGE) || !bounded(b, RANGE) {
        return X87Pc64::Range;
    }
    if reverse {
        std::mem::swap(&mut a, &mut b);
    }
    // Every nonzero input is >= 2^-400 and a multiple of 2^-452.
    // Products and their residuals are multiples of 2^-904; sums and their
    // residuals are multiples of 2^-452. No nonzero residual can underflow.
    // The largest product is < 2^802; Dekker's split cannot overflow either.
    // Thus TwoSum/TwoProduct gives exact = rounded + error, so error == 0
    // proves an exactly representable result, also at extended precision.
    let (result, error) = arithmetic(op, a, b);
    if error != 0.0 {
        X87Pc64::Inexact
    } else if result == 0.0 {
        X87Pc64::Zero
    } else {
        debug_assert!(result.is_normal());
        X87Pc64::Exact
    }
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

/// Run `op` if it qualifies; `None` leaves every piece of state untouched.
pub fn execute(
    op: &NativeX87,
    fast: Fast,
    state: &mut CpuState,
    memory: &mut Memory,
) -> Option<u32> {
    let i = &op.instruction;
    let x87 = &state.x87;
    if x87.status & 0x80 != 0 || state.mmx_mode != 0 {
        return None;
    }
    let control = x87.control;
    let mut status = x87.status;
    let mut top = x87.top();
    // (logical index relative to the final top, value) writes, applied last.
    let mut writes: [(usize, Ext); 2] = [(usize::MAX, Ext::default()); 2];
    let mut tags_freed = [usize::MAX; 2];
    let mut dp = None;
    let mut metadata = true;
    let mut new_control = None;
    let pc = state.eip;
    let epoch = memory.epoch;
    let slot = |top: usize, logical: usize| (top + logical) & 7;
    let empty = |physical: usize| x87.physical_tag(physical) == 3;
    match fast {
        // ES and deferred MMX were checked above. WAIT changes neither C1 nor
        // the last x87 instruction/data metadata, regardless of the control word.
        Fast::Round | Fast::Scale | Fast::Examine => return None,
        Fast::Wait => metadata = false,
        Fast::Load(source) => {
            let (value, at) = raw::source(source, i, state, memory)?;
            let target = top.wrapping_sub(1) & 7;
            if !empty(target) {
                return None;
            }
            top = target;
            writes[0] = (target, value);
            dp = at;
            status &= !C1;
        }
        Fast::Constant(one) => {
            let target = top.wrapping_sub(1) & 7;
            if !empty(target) {
                return None;
            }
            top = target;
            writes[0] = (target, extended(if one { 1.0 } else { 0.0 }));
            status &= !C1;
        }
        Fast::Store {
            width,
            integer,
            pop,
        } => {
            let at = address(i, state)?;
            if !writable(memory, at, usize::from(width)) {
                return None;
            }
            let physical = slot(top, 0);
            if empty(physical) {
                return None;
            }
            let (bits, exact, up) = raw::store_bits(x87.regs[physical], width, integer, control)?;
            if !exact && control & PE == 0 {
                return None;
            }
            status &= !C1;
            if !exact {
                status |= PE | if up { C1 } else { 0 };
            }
            store(memory, at, &bits.to_le_bytes()[..usize::from(width)])?;
            dp = Some(at);
            if pop {
                tags_freed[0] = physical;
                top = (top + 1) & 7;
            }
        }
        Fast::StoreSt { index, pop } => {
            let from = slot(top, 0);
            if empty(from) {
                return None;
            }
            // Register copies keep any valid encoding the stack already holds.
            raw::ordinary(x87.regs[from])?;
            writes[0] = (slot(top, usize::from(index)), x87.regs[from]);
            status &= !C1;
            if pop {
                tags_freed[0] = from;
                top = (top + 1) & 7;
            }
        }
        Fast::Arith {
            op,
            dest,
            source,
            reverse,
            pop,
        } => {
            let precision = if control & 0x0f3f == 0x033f {
                64
            } else {
                arithmetic_precision(control)?
            };
            let target = slot(top, usize::from(dest));
            if empty(target) {
                return None;
            }
            let mut a = double(x87.regs[target], RANGE)?;
            let (mut b, at) = load(source, i, state, memory, RANGE)?;
            if reverse {
                std::mem::swap(&mut a, &mut b);
            }
            if op == Op::Div && b == 0.0 {
                return None;
            }
            if precision == 24 && (a.to_bits() | b.to_bits()) & SINGLE_TAIL != 0 {
                return None;
            }
            let (mut result, error) = arithmetic(op, a, b);
            // With q and b within 2^+-400, q*b and its nonzero residual
            // lie on the 2^-904 grid; Dekker splitting cannot overflow.
            // a and fl(q*b) are adjacent enough for exact subtraction. Thus
            // residual == 0 proves q*b == a, including for signed zero a.
            if precision == 64 && op == Op::Div && !bounded(result, RANGE) {
                return None;
            }
            // The range proof in pc64_opportunity rules out lost residuals.
            // Nearest cancellation gives +0 (except -0 + -0); multiplication
            // preserves sign XOR. These zero rules match the extended model.
            if precision == 64 && error != 0.0 {
                return None;
            }
            let mut inexact = error != 0.0;
            let mut up = rounded_up(error, result);
            if precision == 24 {
                let (single, grew) = round_single_precision(result);
                // No 24-bit value lies strictly between a changed result and
                // the exact one, so the binary64 rounding decides C1 only when
                // the 24-bit rounding left it unchanged.
                if single != result {
                    (result, inexact, up) = (single, true, grew);
                }
            }
            status &= !C1;
            if inexact {
                status |= PE | if up { C1 } else { 0 };
            }
            writes[0] = (target, extended(result));
            dp = at.or(dp);
            if pop {
                tags_freed[0] = slot(top, 0);
                top = (top + 1) & 7;
            }
        }
        Fast::Compare { source, pops } => {
            let first = slot(top, 0);
            if empty(first) {
                return None;
            }
            let a = raw::ordinary(x87.regs[first])?;
            let (b, at) = raw::source(source, i, state, memory)?;
            let comparison = raw::compare(a, b);
            status = (status & !(C0 | C1 | C2 | C3))
                | if comparison.is_eq() {
                    C3
                } else if comparison.is_lt() {
                    C0
                } else {
                    0
                };
            dp = at;
            for (n, freed) in tags_freed.iter_mut().take(usize::from(pops)).enumerate() {
                *freed = slot(top, n);
            }
            top = (top + usize::from(pops)) & 7;
        }
        Fast::Exchange(index) => {
            let (first, second) = (slot(top, 0), slot(top, usize::from(index)));
            if empty(first) || empty(second) {
                return None;
            }
            raw::ordinary(x87.regs[first])?;
            raw::ordinary(x87.regs[second])?;
            writes = [(first, x87.regs[second]), (second, x87.regs[first])];
            status &= !C1;
        }
        Fast::Negate | Fast::Absolute => {
            let target = slot(top, 0);
            if empty(target) {
                return None;
            }
            let value = raw::ordinary(x87.regs[target])?;
            writes[0] = (
                target,
                Ext::from_parts(
                    value.significand(),
                    if fast == Fast::Negate {
                        value.sign_exponent() ^ 0x8000
                    } else {
                        value.sign_exponent() & 0x7fff
                    },
                ),
            );
            status &= !C1;
        }
        Fast::LoadControl => {
            let at = address(i, state)?;
            let value = u16::from_le_bytes(read(memory, at)?);
            // Unmasking a pending exception takes the transactional path.
            if status & !value & 0x3f != 0 {
                return None;
            }
            new_control = Some(value);
            status &= !0x8080;
            metadata = false;
        }
        Fast::StoreControl | Fast::StoreStatus => {
            let value = if fast == Fast::StoreControl {
                control
            } else {
                status
            };
            if i.op0_kind() == OpKind::Register {
                state.regs[0] = (state.regs[0] & 0xffff_0000) | u32::from(value);
            } else {
                let at = address(i, state)?;
                if !writable(memory, at, 2) {
                    return None;
                }
                store(memory, at, &value.to_le_bytes())?;
            }
            metadata = false;
        }
    }
    let x87 = &mut state.x87;
    for &(physical, value) in &writes {
        if physical != usize::MAX {
            x87.regs[physical] = value;
            x87.set_physical_tag(physical, value.tag());
        }
    }
    for &physical in &tags_freed {
        if physical != usize::MAX {
            x87.set_physical_tag(physical, 3);
        }
    }
    x87.status = status;
    x87.set_top(top);
    if let Some(control) = new_control {
        x87.control = control;
    }
    if metadata {
        x87.ip = pc;
        if let Some(opcode) = op.opcode {
            x87.opcode = opcode;
        }
    }
    if let Some(at) = dp {
        x87.dp = at;
    }
    state.mmx = state.x87.regs.map(Ext::significand);
    state.eip = i.next_ip32();
    Some(if memory.epoch != epoch {
        X87_EXIT
    } else {
        X87_DONE
    })
}
