//! Executes a `Func` as the reference semantics of the IR. Front-end tests
//! compare it with the interpreter, and back-end tests compare emitted code
//! with it. Integer flags come from the interpreter's own functions. Floating
//! point uses the default FPCR semantics and checks operand and result values
//! at each operation, recovering to its region start on failure.

use adv_cpu_core::Memory;
use adv_cpu_core::integer::{self, CF, OF, PF, ZF};
use iced_x86::Mnemonic as M;

use super::ir::{
    Block, End, FlagKind, FpOp, FpUnary, Func, IntOp, IntUnary, Op, RecordId, Type, Value, VecOp,
    VecToInt, VecUnary,
};
use super::recovery::{Location, Recipe};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Exit(RecordId),
    /// A failure, continuing at the region start this record describes.
    Fail(RecordId),
    /// The step limit ran out.
    Limit,
}

pub(crate) struct Run {
    pub outcome: Outcome,
    /// Every value's bits, zero-extended; values never defined are 0.
    pub values: Vec<u128>,
    /// Instructions charged to the countdown: block 0's refund at entry,
    /// then each taken edge's charge. Less the outcome record's refund, the
    /// instructions retired.
    pub charged: i64,
}

/// Runs `func` from block 0 with `params`, for at most `limit` instructions.
pub(crate) fn run(func: &Func, params: &[u128], memory: &mut Memory, limit: usize) -> Run {
    let mut values = vec![0u128; func.types.len()];
    let mut block = Block(0);
    let mut args = params.to_vec();
    let mut region = None;
    let mut steps = 0usize;
    let first = func.blocks[0].head.expect("block 0 is a head").record;
    let mut charged = i64::from(func.records[first.0 as usize].refund);
    let outcome = 'run: loop {
        let data = &func.blocks[block.0 as usize];
        for (&p, &v) in data.params.iter().zip(&args) {
            values[p.0 as usize] = v;
        }
        if let Some(head) = data.head {
            region = Some(head.record);
        }
        for (result, op) in &data.insts {
            steps += 1;
            if steps > limit {
                break 'run Outcome::Limit;
            }
            match step(func, op, &values, memory) {
                Some(bits) => {
                    if let Some(r) = result {
                        values[r.0 as usize] = bits;
                    }
                }
                None => break 'run Outcome::Fail(region.expect("block 0 is a head")),
            }
            if let Op::Store { after, .. } = *op {
                region = Some(after);
            }
        }
        let get = |v: Value| values[v.0 as usize];
        let target = match &data.end {
            End::Jump(target) => target,
            End::Branch { cond, taken, fall } => {
                if get(*cond) != 0 {
                    taken
                } else {
                    fall
                }
            }
            End::Exit(record) => break Outcome::Exit(*record),
            End::Unset => unreachable!("verified"),
        };
        args = target.args.iter().map(|&v| get(v)).collect();
        charged += i64::from(target.charge);
        block = target.block;
    };
    Run {
        outcome,
        values,
        charged,
    }
}

/// One instruction's result bits, or `None` when it fails.
fn step(func: &Func, op: &Op, values: &[u128], memory: &mut Memory) -> Option<u128> {
    let get = |v: Value| values[v.0 as usize];
    let wide = |v: Value| func.ty(v) == Type::I64;
    let f = |v: Value| f64::from_bits(get(v) as u64);
    let float = |x: f64| Some(u128::from(x.to_bits()));
    Some(match *op {
        Op::Constant(_, bits) => bits,
        Op::Snapshot(v) => get(v),
        Op::Int(op, a, b) => int(op, get(a) as u64, get(b) as u64, wide(a)),
        Op::IntUnary(op, a) => {
            let x = get(a) as u64;
            let narrow = |x: u64| u128::from(x as u32);
            match op {
                IntUnary::Not if wide(a) => u128::from(!x),
                IntUnary::Not => narrow(!x),
                IntUnary::Neg if wide(a) => u128::from(x.wrapping_neg()),
                IntUnary::Neg => narrow(x.wrapping_neg()),
                IntUnary::ZeroExtend8 => u128::from(x as u8),
                IntUnary::ZeroExtend16 => u128::from(x as u16),
                IntUnary::SignExtend8 => narrow(x as i8 as u64),
                IntUnary::SignExtend16 => narrow(x as i16 as u64),
                IntUnary::Low32 | IntUnary::Widen => narrow(x),
                IntUnary::High32 => narrow(x >> 32),
                IntUnary::ByteSwap => u128::from((x as u32).swap_bytes()),
                IntUnary::SignWiden => u128::from(x as u32 as i32 as i64 as u64),
            }
        }
        Op::Div {
            signed,
            dividend,
            divisor,
            ..
        } => {
            let (n, d) = (get(dividend) as u64, get(divisor) as u32);
            if d == 0 {
                return None;
            }
            let q = if signed {
                let q = (n as i64).checked_div(i64::from(d as i32))?;
                i32::try_from(q).ok()? as u32
            } else {
                u32::try_from(n / u64::from(d)).ok()?
            };
            u128::from(q)
        }
        Op::Remainder {
            quotient,
            dividend,
            divisor,
        } => u128::from(
            (get(dividend) as u32)
                .wrapping_sub((get(quotient) as u32).wrapping_mul(get(divisor) as u32)),
        ),
        Op::Flags {
            kind,
            width,
            a,
            b,
            prior,
        } => u128::from(flags(
            kind,
            width,
            get(a),
            get(b),
            prior.map_or(0, |p| get(p) as u32),
        )?),
        Op::Test { flags, cond } => u128::from(integer::condition_true(cond, get(flags) as u32)),
        Op::Select { cond, a, b } => get(if get(cond) != 0 { a } else { b }),
        Op::Bits { to, value } => {
            let bits = get(value);
            match to {
                Type::I32 => bits & 0xffff_ffff,
                Type::I64 | Type::F64 => bits & u128::from(u64::MAX),
                Type::V128 => bits,
                Type::Flags => unreachable!("no reinterpretation as flags"),
            }
        }
        Op::Fp(op, a, b) | Op::SseFp64(op, a, b) => float(arithmetic(op, f(a), f(b))?)?,
        Op::Fp32(op, a, b) => {
            let a = f32::from_bits(get(a) as u32);
            let b = f32::from_bits(get(b) as u32);
            let sum = matches!(op, FpOp::Add | FpOp::Sub);
            let r = match op {
                FpOp::Add => a + b,
                FpOp::Sub => a - b,
                FpOp::Mul => a * b,
                FpOp::Div => a / b,
            };
            if !r.is_finite() || !sum && r.abs() <= f32::MIN_POSITIVE {
                return None;
            }
            u128::from(r.to_bits())
        }
        Op::FpUnary(op, a) => {
            let x = f(a);
            match op {
                FpUnary::Neg => float(-x)?,
                FpUnary::Abs => float(x.abs())?,
            }
        }
        Op::FromInt(a) => {
            let n = if wide(a) {
                get(a) as u64 as i64
            } else {
                i64::from(get(a) as u32 as i32)
            };
            let magnitude = n.unsigned_abs();
            if 64u32.saturating_sub(magnitude.leading_zeros() + magnitude.trailing_zeros()) > 53 {
                return None;
            }
            float(n as f64)?
        }
        Op::ToInt { value, wide, .. } => {
            let x = f(value);
            let (lo, hi) = if wide {
                (i64::MIN as f64, -(i64::MIN as f64))
            } else {
                (f64::from(i32::MIN), -f64::from(i32::MIN))
            };
            let t = x.trunc();
            if x.is_nan() || t < lo || t >= hi {
                return None;
            }
            let n = t as i64;
            if wide {
                u128::from(n as u64)
            } else {
                u128::from(n as u32)
            }
        }
        Op::Vec(VecOp::LessF64, a, b) => {
            let mut result = 0;
            for shift in [0, 64] {
                let a = f64::from_bits((get(a) >> shift) as u64);
                let b = f64::from_bits((get(b) >> shift) as u64);
                if a.is_nan() || b.is_nan() {
                    return None;
                }
                if a < b {
                    result |= u128::from(u64::MAX) << shift;
                }
            }
            result
        }
        Op::Vec(op, a, b) => vector(op, get(a), get(b)),
        Op::VecUnary(op, a) => vector_unary(op, get(a)),
        Op::VecToInt(op, a) => u128::from(vector_to_int(op, get(a))),
        Op::Load { width, addr, .. } => {
            let mut bytes = [0u8; 16];
            memory
                .read_into(get(addr) as u32, &mut bytes[..usize::from(width)])
                .ok()?;
            u128::from_le_bytes(bytes)
        }
        // The host address of guest memory is the guest address itself.
        Op::Host(addr) => get(addr) & 0xffff_ffff,
        Op::FrameLoad { ty, base, offset } => {
            let mut bytes = [0u8; 16];
            let width = if ty == Type::I32 { 4 } else { 8 };
            memory
                .read_into(
                    (get(base) as u32).wrapping_add_signed(offset),
                    &mut bytes[..width],
                )
                .ok()?;
            u128::from_le_bytes(bytes)
        }
        Op::FrameStore {
            base,
            offset,
            value,
        } => {
            let width = if func.ty(value) == Type::I32 { 4 } else { 8 };
            let bytes = get(value).to_le_bytes();
            memory
                .write(
                    (get(base) as u32).wrapping_add_signed(offset),
                    &bytes[..width],
                )
                .ok()?;
            0
        }
        Op::Store {
            width, addr, value, ..
        } => {
            let bytes = get(value).to_le_bytes();
            memory
                .write(get(addr) as u32, &bytes[..usize::from(width)])
                .ok()?;
            0
        }
        Op::X87Load {
            bits,
            denormal,
            single,
        } => {
            let (x, tiny) = if single {
                let x = f32::from_bits(get(bits) as u32);
                (f64::from(x), denormal && x.is_subnormal())
            } else {
                let x = f(bits);
                (x, denormal && x.is_subnormal())
            };
            if x.is_nan() || tiny {
                return None;
            }
            float(x)?
        }
        Op::SseInput {
            a,
            b,
            packed,
            single,
            ..
        } => {
            for v in [Some(a), b].into_iter().flatten() {
                let bits = get(v);
                if if single {
                    f32::from_bits(bits as u32).is_subnormal()
                } else {
                    f64::from_bits(bits as u64).is_subnormal()
                        || packed && f64::from_bits((bits >> 64) as u64).is_subnormal()
                } {
                    return None;
                }
            }
            0
        }
        Op::PlainPage { addr, bytes, write } => {
            let addr = get(addr) as u32;
            #[cfg(unix)]
            let plain = memory.computation_range(addr, bytes as usize, write);
            #[cfg(not(unix))]
            let plain = {
                use adv_cpu_core::memory::{READ, WRITE};
                memory.accessible(addr, bytes as usize, if write { WRITE } else { READ })
                    && !memory.shared_range(addr, bytes as usize)
            };
            if !plain {
                return None;
            }
            0
        }
        Op::PageCross { addr, width, .. } => {
            use adv_cpu_core::memory::{EXEC, PAGE_SIZE, WRITE};
            let addr = get(addr) as u32;
            let width = usize::from(width);
            if addr as usize % PAGE_SIZE > PAGE_SIZE - width {
                #[cfg(unix)]
                let plain = memory.computation_range(addr, width, true);
                #[cfg(not(unix))]
                let plain =
                    memory.accessible(addr, width, WRITE) && !memory.shared_range(addr, width);
                if !plain {
                    return None;
                }
                let page = (addr - memory.base) as usize / PAGE_SIZE;
                if memory.native_permissions()[page..=page + 1]
                    .iter()
                    .any(|class| class & (WRITE | EXEC) != WRITE)
                {
                    return None;
                }
            }
            0
        }
        Op::FrameDisjoint {
            addr,
            width,
            base,
            lo,
            hi,
            ..
        } => {
            let start = (get(base) as u32).wrapping_add_signed(lo);
            let bytes = (i64::from(hi) - i64::from(lo)) as u32;
            if !super::ir::disjoint_ranges(get(addr) as u32, width.into(), start, bytes) {
                return None;
            }
            0
        }
        Op::Validate => 0,
    })
}

fn int(op: IntOp, a: u64, b: u64, wide: bool) -> u128 {
    let (a, b) = if wide {
        (a, b)
    } else {
        (a as u32 as u64, b as u32 as u64)
    };
    let shift = (b & if wide { 63 } else { 31 }) as u32;
    let sign = |x: u64| {
        if wide {
            x as i64
        } else {
            i64::from(x as u32 as i32)
        }
    };
    let value = match op {
        IntOp::Add => a.wrapping_add(b),
        IntOp::Sub => a.wrapping_sub(b),
        IntOp::And => a & b,
        IntOp::Or => a | b,
        IntOp::Xor => a ^ b,
        IntOp::Shl => a << shift,
        IntOp::Shr => a >> shift,
        IntOp::Sar => (sign(a) >> shift) as u64,
        IntOp::Ror if wide => a.rotate_right(shift),
        IntOp::Ror => u64::from((a as u32).rotate_right(shift)),
        IntOp::Mul => a.wrapping_mul(b),
        IntOp::MulHighSigned if wide => {
            ((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64
        }
        IntOp::MulHighSigned => ((sign(a) * sign(b)) >> 32) as u64,
        IntOp::MulHighUnsigned if wide => ((u128::from(a) * u128::from(b)) >> 64) as u64,
        IntOp::MulHighUnsigned => (a * b) >> 32,
    };
    if wide {
        u128::from(value)
    } else {
        u128::from(value as u32)
    }
}

pub(super) fn flags(kind: FlagKind, width: u8, a: u128, b: u128, prior: u32) -> Option<u32> {
    let (x, y) = (a as u32, b as u32);
    let mut out = prior;
    match kind {
        FlagKind::Add | FlagKind::AddCarry | FlagKind::Sub | FlagKind::SubBorrow => {
            let carry = matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow);
            let subtract = matches!(kind, FlagKind::Sub | FlagKind::SubBorrow);
            let carry = u32::from(carry && prior & CF != 0);
            integer::arithmetic(&mut out, x, y, carry, width, subtract);
        }
        FlagKind::Logic => {
            integer::binary(&mut out, M::Or, x, 0, width);
        }
        FlagKind::Increment => {
            integer::unary(&mut out, M::Inc, x, width);
        }
        FlagKind::Decrement => {
            integer::unary(&mut out, M::Dec, x, width);
        }
        FlagKind::Negate => {
            integer::unary(&mut out, M::Neg, x, width);
        }
        FlagKind::Shl | FlagKind::Shr | FlagKind::Sar => {
            let count = y & 31;
            if count != 0 {
                let kind = match kind {
                    FlagKind::Shl => M::Shl,
                    FlagKind::Shr => M::Shr,
                    _ => M::Sar,
                };
                integer::shift(&mut out, kind, x, count, width);
            }
        }
        FlagKind::CarryBit => out = out & !CF | (x & 1),
        FlagKind::MulUnsigned => {
            out = out & !(CF | OF) | if x != 0 { CF | OF } else { 0 };
        }
        FlagKind::Rol | FlagKind::Ror => {
            let count = y & 31;
            if count != 0 {
                let left = kind == FlagKind::Rol;
                let carry = (x >> if left { 0 } else { 31 }) & 1;
                out = out & !CF | carry;
                if count == 1 {
                    let overflow = (x >> 31) ^ if left { carry } else { (x >> 30) & 1 };
                    out = out & !OF | (overflow << 11);
                }
            }
        }
        FlagKind::MulSigned => {
            let product = integer::signed(x, width) * integer::signed(y, width);
            let low = product as u32 & integer::mask(width);
            let overflow = product != integer::signed(low, width);
            out = out & !(CF | OF) | if overflow { CF | OF } else { 0 };
        }
        FlagKind::UnorderedCompare | FlagKind::OrderedCompare => {
            let p = f64::from_bits(a as u64);
            let q = f64::from_bits(b as u64);
            let ordered = kind == FlagKind::OrderedCompare;
            if signaling(p) || signaling(q) || ordered && (p.is_nan() || q.is_nan()) {
                return None;
            }
            let set = if p.is_nan() || q.is_nan() {
                ZF | PF | CF
            } else if p < q {
                CF
            } else if p == q {
                ZF
            } else {
                0
            };
            out = out & !(ZF | PF | CF | OF | integer::SF | integer::AF) | set;
        }
    }
    Some(out)
}

fn signaling(x: f64) -> bool {
    x.is_nan() && x.to_bits() & 1 << 51 == 0
}

fn arithmetic(op: FpOp, a: f64, b: f64) -> Option<f64> {
    if signaling(a) || signaling(b) {
        return None;
    }
    if a.is_nan() || b.is_nan() {
        return Some(if a.is_nan() { a } else { b });
    }
    let r = match op {
        FpOp::Add => a + b,
        FpOp::Sub => a - b,
        FpOp::Mul => a * b,
        FpOp::Div => a / b,
    };
    if r.is_nan() || r.is_infinite() && a.is_finite() && b.is_finite() || r.is_subnormal() {
        return None;
    }
    if matches!(op, FpOp::Mul | FpOp::Div)
        && (r.abs() == f64::MIN_POSITIVE || r == 0.0 && a != 0.0 && (op != FpOp::Mul || b != 0.0))
    {
        return None;
    }
    Some(r)
}

fn vec_signed(value: u64, bits: usize) -> i64 {
    ((value << (64 - bits)) as i64) >> (64 - bits)
}

fn vec_shift(op: crate::encode::ShiftImm, log2: u8, full: bool, a: u128, count: u64) -> u128 {
    use crate::encode::ShiftImm as S;
    let bits = 8usize << log2;
    let mask = u64::MAX >> (64 - bits);
    let lanes = if full { 128 } else { 64 } / bits;
    let mut out = 0u128;
    for i in 0..lanes {
        let shift = i * bits;
        let lane = ((a >> shift) as u64) & mask;
        let val = match op {
            S::Shl => {
                if count >= bits as u64 {
                    0
                } else {
                    (lane << count) & mask
                }
            }
            S::Ushr => {
                if count >= bits as u64 {
                    0
                } else {
                    lane >> count
                }
            }
            S::Sshr => {
                let s = vec_signed(lane, bits);
                (s >> count.min(bits as u64 - 1)) as u64 & mask
            }
        };
        out |= u128::from(val) << shift;
    }
    out
}

fn vector_unary(op: VecUnary, a: u128) -> u128 {
    match op {
        VecUnary::ShiftImm(op, log2, count, full) => vec_shift(op, log2, full, a, u64::from(count)),
        VecUnary::NarrowScalar => u128::from((f64::from_bits(a as u64) as f32).to_bits()),
        VecUnary::Low64 => a & u128::from(u64::MAX),
        VecUnary::High64 => a >> 64,
        VecUnary::Shuffle(log2, imm, full) => {
            let bits = 8usize << log2;
            let mask = u128::MAX >> (128 - bits);
            let mut out = 0u128;
            for lane in 0..4 {
                let from = usize::from((imm >> (2 * lane)) & 3);
                let val = (a >> (from * bits)) & mask;
                out |= val << (lane * bits);
            }
            if full {
                out
            } else {
                out & u128::from(u64::MAX)
            }
        }
        VecUnary::ShuffleWords(high, imm) => {
            let base = if high { 4 } else { 0 };
            let mut out = a & !(u128::from(u64::MAX) << (base * 16));
            for lane in 0..4 {
                let from = usize::from((imm >> (2 * lane)) & 3);
                let val = (a >> ((base + from) * 16)) & 0xffff;
                out |= val << ((base + lane) * 16);
            }
            out
        }
        VecUnary::ShiftBytes(left, count) => {
            if count >= 16 {
                0
            } else if left {
                a << (usize::from(count) * 8)
            } else {
                a >> (usize::from(count) * 8)
            }
        }
    }
}

fn vector_to_int(op: VecToInt, a: u128) -> u32 {
    match op {
        VecToInt::ExtractWord(lane) => ((a >> (usize::from(lane) * 16)) & 0xffff) as u32,
        VecToInt::ByteMask(full) => {
            let bytes = if full { 16 } else { 8 };
            let mut out = 0u32;
            for i in 0..bytes {
                if (a >> (i * 8 + 7)) & 1 != 0 {
                    out |= 1 << i;
                }
            }
            out
        }
        VecToInt::SignMask(double) => {
            let (lanes, shift) = if double { (2, 63) } else { (4, 31) };
            let mut out = 0u32;
            for i in 0..lanes {
                let width = if double { 64 } else { 32 };
                if (a >> (i * width + shift)) & 1 != 0 {
                    out |= 1 << i;
                }
            }
            out
        }
    }
}

fn vector(op: VecOp, a: u128, b: u128) -> u128 {
    use crate::encode::{Narrowing, Permute, Same};
    let mask_full = |v: u128, full: bool| {
        if full { v } else { v & u128::from(u64::MAX) }
    };
    match op {
        VecOp::And(full) => mask_full(a & b, full),
        VecOp::AndNot(full) => mask_full(!a & b, full),
        VecOp::Or(full) => mask_full(a | b, full),
        VecOp::Xor(full) => mask_full(a ^ b, full),
        VecOp::Same(op, log2, full) => {
            let bits = 8usize << log2;
            let mask = u64::MAX >> (64 - bits);
            let lanes = if full { 128 } else { 64 } / bits;
            let mut out = 0u128;
            for i in 0..lanes {
                let shift = i * bits;
                let x = ((a >> shift) as u64) & mask;
                let y = ((b >> shift) as u64) & mask;
                let val = match op {
                    Same::Add => x.wrapping_add(y) & mask,
                    Same::Sub => x.wrapping_sub(y) & mask,
                    Same::Sqadd => {
                        let lo = -(1i64 << (bits - 1));
                        let hi = (1i64 << (bits - 1)) - 1;
                        (vec_signed(x, bits) + vec_signed(y, bits)).clamp(lo, hi) as u64 & mask
                    }
                    Same::Uqadd => (x + y).min(mask),
                    Same::Sqsub => {
                        let lo = -(1i64 << (bits - 1));
                        let hi = (1i64 << (bits - 1)) - 1;
                        (vec_signed(x, bits) - vec_signed(y, bits)).clamp(lo, hi) as u64 & mask
                    }
                    Same::Uqsub => x.saturating_sub(y),
                    Same::Cmeq => {
                        if x == y {
                            mask
                        } else {
                            0
                        }
                    }
                    Same::Cmgt => {
                        if vec_signed(x, bits) > vec_signed(y, bits) {
                            mask
                        } else {
                            0
                        }
                    }
                    Same::Mul => x.wrapping_mul(y) & mask,
                    Same::Umin => x.min(y),
                    Same::Umax => x.max(y),
                    Same::Smin => {
                        if vec_signed(x, bits) < vec_signed(y, bits) {
                            x
                        } else {
                            y
                        }
                    }
                    Same::Smax => {
                        if vec_signed(x, bits) > vec_signed(y, bits) {
                            x
                        } else {
                            y
                        }
                    }
                    Same::Urhadd => ((x + y + 1) >> 1) & mask,
                    _ => unreachable!("unsupported Same op"),
                };
                out |= u128::from(val) << shift;
            }
            out
        }
        VecOp::Unpack(perm, log2, full) => {
            let width = 1usize << log2;
            let total = if full { 16 } else { 8 };
            let base = if perm == Permute::Zip2 { total / 2 } else { 0 };
            let left = a.to_le_bytes();
            let right = b.to_le_bytes();
            let mut out = [0u8; 16];
            for n in 0..total / (2 * width) {
                let dst = n * 2 * width;
                let src = base + n * width;
                out[dst..dst + width].copy_from_slice(&left[src..src + width]);
                out[dst + width..dst + 2 * width].copy_from_slice(&right[src..src + width]);
            }
            u128::from_le_bytes(out)
        }
        VecOp::Pack(narrow, log2, full) => {
            let out_w = 1usize << log2;
            let in_w = out_w * 2;
            let total = if full { 16 } else { 8 };
            let left = a.to_le_bytes();
            let right = b.to_le_bytes();
            let mut out = [0u8; 16];
            for (n, chunk) in left[..total]
                .chunks_exact(in_w)
                .chain(right[..total].chunks_exact(in_w))
                .enumerate()
            {
                let mut val = 0u64;
                for (k, &byte) in chunk.iter().enumerate() {
                    val |= u64::from(byte) << (k * 8);
                }
                let s = vec_signed(val, in_w * 8);
                let clamped = match narrow {
                    Narrowing::Sqxtun => s.clamp(0, 255),
                    Narrowing::Sqxtn if out_w == 1 => s.clamp(-128, 127),
                    Narrowing::Sqxtn => s.clamp(-32768, 32767),
                    Narrowing::Xtn => unreachable!(),
                };
                out[n * out_w..(n + 1) * out_w].copy_from_slice(&clamped.to_le_bytes()[..out_w]);
            }
            u128::from_le_bytes(out)
        }
        VecOp::MulHigh(signed, full) => {
            let lanes = if full { 8 } else { 4 };
            let mut out = 0u128;
            for i in 0..lanes {
                let x = (a >> (i * 16)) as u16;
                let y = (b >> (i * 16)) as u16;
                let hi = if signed {
                    ((i32::from(x as i16) * i32::from(y as i16)) >> 16) as u16
                } else {
                    ((u32::from(x) * u32::from(y)) >> 16) as u16
                };
                out |= u128::from(hi) << (i * 16);
            }
            out
        }
        VecOp::MulAdd(full) => {
            let pairs = if full { 4 } else { 2 };
            let mut out = 0u128;
            for i in 0..pairs {
                let x0 = i32::from((a >> (i * 32)) as i16);
                let y0 = i32::from((b >> (i * 32)) as i16);
                let x1 = i32::from((a >> (i * 32 + 16)) as i16);
                let y1 = i32::from((b >> (i * 32 + 16)) as i16);
                let sum = (x0 * y0).wrapping_add(x1 * y1) as u32;
                out |= u128::from(sum) << (i * 32);
            }
            out
        }
        VecOp::Shift(op, log2, full) => vec_shift(op, log2, full, a, b as u64),
        VecOp::Shufps(imm) => {
            let mut out = 0u128;
            for lane in 0..4 {
                let src = if lane < 2 { a } else { b };
                let from = usize::from((imm >> (2 * lane)) & 3);
                out |= ((src >> (from * 32)) & u128::from(u32::MAX)) << (lane * 32);
            }
            out
        }
        VecOp::Shufpd(imm) => {
            let lo = (a >> (usize::from(imm & 1) * 64)) as u64;
            let hi = (b >> (usize::from((imm >> 1) & 1) * 64)) as u64;
            u128::from(lo) | u128::from(hi) << 64
        }
        VecOp::MulDwords(full) => {
            let lo = u64::from(a as u32) * u64::from(b as u32);
            if full {
                let hi = u64::from((a >> 64) as u32) * u64::from((b >> 64) as u32);
                u128::from(lo) | u128::from(hi) << 64
            } else {
                u128::from(lo)
            }
        }
        VecOp::SadBytes(full) => {
            let sum = |offset: usize| -> u64 {
                (0..8)
                    .map(|i| {
                        let x = (a >> ((offset + i) * 8)) as u8;
                        let y = (b >> ((offset + i) * 8)) as u8;
                        u64::from(x.abs_diff(y))
                    })
                    .sum()
            };
            let lo = sum(0);
            if full {
                u128::from(lo) | u128::from(sum(8)) << 64
            } else {
                u128::from(lo)
            }
        }
        VecOp::MoveLow32 => (a & !u128::from(u32::MAX)) | (b & u128::from(u32::MAX)),
        VecOp::Insert64(dst_lane, src_lane) => {
            let val = u128::from((b >> (if src_lane { 64 } else { 0 })) as u64);
            let shift = if dst_lane { 64 } else { 0 };
            (a & !(u128::from(u64::MAX) << shift)) | (val << shift)
        }
        VecOp::InsertWord(lane, full) => {
            let word = b & 0xffff;
            let shift = usize::from(lane) * 16;
            let out = (a & !(0xffffu128 << shift)) | (word << shift);
            mask_full(out, full)
        }
        VecOp::LessF64 => unreachable!("handled by step"),
    }
}

/// The bits a record location gives, with recipes evaluated.
pub(crate) fn locate(location: &Location<Value>, values: &[u128]) -> u128 {
    let get = |v: Value| values[v.0 as usize];
    match *location {
        Location::Constant(bits) => u128::from(bits),
        Location::At(v) | Location::Recipe(Recipe::Eflags(v)) => get(v),
        Location::Recipe(Recipe::C1 { op, a, b }) => u128::from(rounded_up(
            op,
            f64::from_bits(get(a) as u64),
            f64::from_bits(get(b) as u64),
        )),
    }
}

/// C1 after a nearest-rounded `a op b` without other exceptions: whether
/// the magnitude was rounded up.
pub(crate) fn rounded_up(op: FpOp, a: f64, b: f64) -> bool {
    let (r, error) = match op {
        FpOp::Add | FpOp::Sub => {
            let b = if op == FpOp::Sub { -b } else { b };
            let s = a + b;
            let bb = s - a;
            (s, (a - (s - bb)) + (b - bb))
        }
        FpOp::Mul => {
            let p = a * b;
            (p, a.mul_add(b, -p))
        }
        FpOp::Div => {
            let q = a / b;
            (q, (-q).mul_add(b, a) * b.signum())
        }
    };
    error != 0.0 && error.signum() != r.signum()
}

#[cfg(test)]
mod tests {
    use super::super::ir::{Head, Source, Target};
    use super::super::recovery::IrRecord;
    use super::*;

    fn record() -> IrRecord {
        IrRecord {
            eip: 0x1000,
            refund: 0,
            regs: [None; 8],
            eflags: None,
            flag_groups: Vec::new(),
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: Vec::new(),
        }
    }

    /// `x * y`, validated, stored at `addr`, then an exit.
    fn store_product() -> (Func, RecordId, RecordId) {
        let mut f = Func::default();
        let entry = f.record(record());
        let after = f.record(record());
        let exit = f.record(record());
        let b = f.block(Some(Head { record: entry }));
        let x = f.param(b, Type::F64);
        let y = f.param(b, Type::F64);
        let addr = f.param(b, Type::I32);
        f.entry = vec![Source::X87(0), Source::X87(1), Source::Reg(7)];
        let p = f.push(b, Op::Fp(FpOp::Mul, x, y)).unwrap();
        f.push(b, Op::Validate);
        f.push(
            b,
            Op::Store {
                width: 8,
                addr,
                value: p,
                after,
                checked: false,
                pc: 0,
            },
        );
        f.end(b, End::Exit(exit));
        (f, entry, exit)
    }

    fn memory() -> Memory {
        let mut memory = Memory::new(0x10000, 0x10000).unwrap();
        memory.protect(0x10000, 0x10000, 3).unwrap();
        memory
    }

    fn bits(x: f64) -> u128 {
        u128::from(x.to_bits())
    }

    #[test]
    fn validation_fails_on_overflow_tiny_results_and_denormal_inputs() {
        let (f, entry, exit) = store_product();
        let mut m = memory();
        let ok = run(&f, &[bits(1.5), bits(-2.0), 0x10000], &mut m, 100);
        assert_eq!(ok.outcome, Outcome::Exit(exit));
        assert_eq!(m.read(0x10000, 8).unwrap(), (-3.0f64).to_le_bytes());
        let tiny = f64::from_bits(1);
        for (x, y) in [
            (1e300, 1e300),
            (1e-200, 1e-200),
            (tiny, 1.0),
            (f64::MIN_POSITIVE, 0.5),
        ] {
            let run = run(&f, &[bits(x), bits(y), 0x10008], &mut m, 100);
            assert_eq!(run.outcome, Outcome::Fail(entry), "{x:e} * {y:e}");
        }
        assert_eq!(m.read(0x10008, 8).unwrap(), [0; 8]);
    }

    #[test]
    fn a_store_outside_memory_fails_to_the_region_start() {
        let (f, entry, _) = store_product();
        let run = run(&f, &[bits(1.0), bits(1.0), 0x30000], &mut memory(), 100);
        assert_eq!(run.outcome, Outcome::Fail(entry));
    }

    // spec: A64-OPT-6
    #[test]
    fn a_failure_after_a_store_returns_to_the_store_boundary() {
        let (mut f, _, exit) = store_product();
        let b = Block(0);
        let after = f.record(record());
        let Op::Store { after: first, .. } = f.blocks[0].insts[2].1 else {
            unreachable!("built with a store")
        };
        let unmapped = f.push(b, Op::Constant(Type::I32, 0x30000)).unwrap();
        f.push(
            b,
            Op::Store {
                width: 4,
                addr: unmapped,
                value: unmapped,
                after,
                checked: false,
                pc: 0,
            },
        );
        f.end(b, End::Exit(exit));
        let run = run(&f, &[bits(1.0), bits(1.0), 0x10000], &mut memory(), 100);
        assert_eq!(run.outcome, Outcome::Fail(first));
    }

    #[test]
    fn x87_loads_refuse_nan_and_optionally_denormals() {
        let mut f = Func::default();
        let entry = f.record(record());
        let exit = f.record(record());
        let b = f.block(Some(Head { record: entry }));
        let raw = f.param(b, Type::I64);
        f.push(
            b,
            Op::X87Load {
                bits: raw,
                denormal: true,
                single: false,
            },
        );
        f.end(b, End::Exit(exit));
        let mut m = memory();
        for (x, out) in [
            (2.5, Outcome::Exit(exit)),
            (-0.0, Outcome::Exit(exit)),
            (f64::INFINITY, Outcome::Exit(exit)),
            (f64::NAN, Outcome::Fail(entry)),
            (f64::from_bits(3), Outcome::Fail(entry)),
        ] {
            assert_eq!(run(&f, &[bits(x)], &mut m, 10).outcome, out, "{x:e}");
        }
    }

    #[test]
    fn a_counted_loop_carries_block_parameters() {
        let mut f = Func::default();
        let mut full = record();
        full.regs = [Some(Location::Constant(0)); 8];
        full.eflags = Some(Location::Constant(0));
        full.xmm = [Some(Location::Constant(0)); 8];
        full.mmx = [Some(Location::Constant(0)); 8];
        let entry = f.record(IrRecord {
            refund: 3,
            ..full.clone()
        });
        let exit = f.record(full);
        let head = f.block(Some(Head { record: entry }));
        let n = f.param(head, Type::I32);
        let sum = f.param(head, Type::I32);
        f.entry = vec![Source::Reg(1), Source::Reg(0)];
        let body = f.block(None);
        let done = f.block(None);
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let flags = f
            .push(
                head,
                Op::Flags {
                    kind: FlagKind::Decrement,
                    width: 4,
                    a: n,
                    b: n,
                    prior: None,
                },
            )
            .unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, n, one)).unwrap();
        let more = f
            .push(
                head,
                Op::Test {
                    flags,
                    cond: iced_x86::ConditionCode::ne,
                },
            )
            .unwrap();
        let total = f.push(head, Op::Int(IntOp::Add, sum, n)).unwrap();
        f.end(
            head,
            End::Branch {
                cond: more,
                taken: Target {
                    block: body,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        f.end(
            body,
            End::Jump(Target {
                block: head,
                args: vec![next, total],
                charge: 2,
            }),
        );
        f.end(done, End::Exit(exit));
        let run = run(&f, &[5, 0], &mut memory(), 1000);
        assert_eq!(run.outcome, Outcome::Exit(exit));
        assert_eq!(run.values[total.0 as usize], 15);
        assert_eq!(run.charged, 3 + 4 * 2);
        assert_eq!(super::super::verify::verify(&f), Ok(()));
    }

    #[test]
    fn c1_recipes_report_upward_rounding() {
        assert!(rounded_up(FpOp::Div, 1.0, 10.0));
        assert!(rounded_up(FpOp::Add, 0.1, 0.2));
        assert!(!rounded_up(FpOp::Div, 1.0, 3.0));
        assert!(!rounded_up(FpOp::Mul, 1.5, 2.0));
        assert!(rounded_up(FpOp::Add, 1.0, f64::EPSILON * 0.75));
    }
}
