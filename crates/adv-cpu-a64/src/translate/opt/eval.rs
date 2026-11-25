//! Executes a `Func` as the reference semantics of the IR. Front-end tests
//! compare it with the interpreter, and back-end tests compare emitted code
//! with it. Integer flags come from the interpreter's own functions. Floating
//! point uses the default FPCR semantics and checks operand and result values
//! at each operation, recovering to its region start on failure.

use crate::translate::front::tags::Mnemonic as M;
use adv_cpu_core::Memory;
use adv_cpu_core::integer::{self, CF, OF, PF, ZF};

use super::ir::{
    Block, DomainGuardKind, End, FlagKind, FmaOp, FpOp, FpUnary, Func, IntOp, IntUnary, Op,
    RecordId, SINGLE_EXACT, SINGLE_RESULT, SseConvertKind, Type, Value, VecOp, VecToInt, VecUnary,
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
            match step(func, *result, op, &values, memory) {
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
fn step(
    func: &Func,
    result: Option<Value>,
    op: &Op,
    values: &[u128],
    memory: &mut Memory,
) -> Option<u128> {
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
        Op::Fp(op, a, b, _) => {
            if let Some(r) = result
                && func.deferred_fp.contains(&r)
                && matches!(op, FpOp::Add | FpOp::Sub)
            {
                let (x, y) = (f(a), f(b));
                let res = match op {
                    FpOp::Add => x + y,
                    FpOp::Sub => x - y,
                    _ => unreachable!(),
                };
                float(res)?
            } else {
                float(arithmetic(op, f(a), f(b))?)?
            }
        }
        Op::SseFp64(op, a, b) => float(arithmetic(op, f(a), f(b))?)?,
        Op::X87Single(op, a, b, proved) => {
            let (a, b) = (f(a), f(b));
            let (sa, sb) = (a as f32, b as f32);
            if f64::from(sa) != a || f64::from(sb) != b {
                return None;
            }
            let r = match op {
                FpOp::Add => sa + sb,
                FpOp::Sub => sa - sb,
                FpOp::Mul => sa * sb,
                FpOp::Div => sa / sb,
            };
            if r == 0.0 {
                if !(matches!(op, FpOp::Add | FpOp::Sub) || a == 0.0 || op == FpOp::Mul && b == 0.0)
                {
                    return None;
                }
            } else if !r.is_normal() {
                return None;
            } else if proved & (SINGLE_RESULT | SINGLE_EXACT) == 0 {
                // Unproved backend excludes the entire exp == 1 binade (< 2^-125).
                if r.abs() < f32::from_bits(0x0100_0000) {
                    return None;
                }
            } else if r.abs() == f32::MIN_POSITIVE {
                return None;
            }
            let widened = f64::from(r);
            if proved & SINGLE_EXACT != 0 {
                let d = match op {
                    FpOp::Add => a + b,
                    FpOp::Sub => a - b,
                    FpOp::Mul => a * b,
                    FpOp::Div => a / b,
                };
                if d.to_bits() != widened.to_bits() {
                    return None;
                }
            }
            float(widened)?
        }
        Op::Fma {
            op,
            a,
            b,
            c,
            single,
            proved,
        } => {
            let (a, b, c) = (f(a), f(b), f(c));
            if single {
                let (sa, sb, sc) = (a as f32, b as f32, c as f32);
                if f64::from(sa) != a || f64::from(sb) != b || f64::from(sc) != c {
                    return None;
                }
                let r = match op {
                    FmaOp::Add => sa.mul_add(sb, sc),
                    FmaOp::Sub => sa.mul_add(sb, -sc),
                    FmaOp::RevSub => (-sa).mul_add(sb, sc),
                };
                if r != 0.0 {
                    if !r.is_normal() {
                        return None;
                    }
                    if proved & (SINGLE_RESULT | SINGLE_EXACT) == 0 {
                        if r.abs() < f32::from_bits(0x0100_0000) {
                            return None;
                        }
                    } else if r.abs() == f32::MIN_POSITIVE {
                        return None;
                    }
                }
                let widened = f64::from(r);
                if proved & SINGLE_EXACT != 0 {
                    let d = match op {
                        FmaOp::Add => a.mul_add(b, c),
                        FmaOp::Sub => a.mul_add(b, -c),
                        FmaOp::RevSub => (-a).mul_add(b, c),
                    };
                    if d.to_bits() != widened.to_bits() {
                        return None;
                    }
                }
                float(widened)?
            } else {
                let r = match op {
                    FmaOp::Add => a.mul_add(b, c),
                    FmaOp::Sub => a.mul_add(b, -c),
                    FmaOp::RevSub => (-a).mul_add(b, c),
                };
                if !r.is_finite() || r.is_subnormal() {
                    return None;
                }
                float(r)?
            }
        }
        Op::X87Arith {
            op,
            a,
            b,
            pc,
            rounding,
            ..
        } => {
            use adv_cpu_core::floating::softfloat;
            let (a, b) = (f(a), f(b));
            if a.is_nan() || b.is_nan() {
                return None;
            }
            softfloat::configure_sse(0x1f80 | (u32::from(rounding) << 13));
            match pc {
                0 => {
                    let (sa, sb) = (a as f32, b as f32);
                    if f64::from(sa) != a || f64::from(sb) != b {
                        return None;
                    }
                    let bits = softfloat::sse_binary(
                        u64::from(sa.to_bits()),
                        u64::from(sb.to_bits()),
                        op as u8,
                        4,
                    );
                    let r = f32::from_bits(bits as u32);
                    if r == 0.0 {
                        if !(matches!(op, FpOp::Add | FpOp::Sub)
                            || a == 0.0
                            || op == FpOp::Mul && b == 0.0)
                        {
                            return None;
                        }
                    } else if !r.is_normal() || r.abs() < f32::from_bits(0x0100_0000) {
                        return None;
                    }
                    float(f64::from(r))?
                }
                3 => {
                    // PC64: exact binary64 discrimination (zero rounding error).
                    let bits = softfloat::sse_binary(a.to_bits(), b.to_bits(), op as u8, 8);
                    let fl = softfloat::flags();
                    let r = f64::from_bits(bits);
                    if fl & 0x3f != 0 || !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    if matches!(op, FpOp::Mul | FpOp::Div)
                        && (r.abs() == f64::MIN_POSITIVE
                            || r == 0.0 && a != 0.0 && (op != FpOp::Mul || b != 0.0))
                    {
                        return None;
                    }
                    float(r)?
                }
                _ => {
                    // PC53 with directed rounding.
                    let bits = softfloat::sse_binary(a.to_bits(), b.to_bits(), op as u8, 8);
                    let r = f64::from_bits(bits);
                    if r.is_nan()
                        || r.is_infinite() && a.is_finite() && b.is_finite()
                        || r.is_subnormal()
                    {
                        return None;
                    }
                    if matches!(op, FpOp::Mul | FpOp::Div)
                        && (r.abs() == f64::MIN_POSITIVE
                            || r == 0.0 && a != 0.0 && (op != FpOp::Mul || b != 0.0))
                    {
                        return None;
                    }
                    float(r)?
                }
            }
        }
        Op::X87Sqrt {
            value,
            pc,
            rounding,
            ..
        } => {
            use adv_cpu_core::floating::softfloat;
            let a = f(value);
            if a.is_nan() || (a < 0.0) || a.is_subnormal() {
                return None;
            }
            if a == 0.0 {
                return float(a);
            }
            if a.is_infinite() {
                return float(a);
            }
            softfloat::configure_sse(0x1f80 | (u32::from(rounding) << 13));
            match pc {
                0 => {
                    let sa = a as f32;
                    if f64::from(sa) != a || sa.is_subnormal() {
                        return None;
                    }
                    let bits = softfloat::sse_binary(0, u64::from(sa.to_bits()), 4, 4);
                    let r = f32::from_bits(bits as u32);
                    if !r.is_normal() || r.abs() < f32::from_bits(0x0100_0000) {
                        return None;
                    }
                    float(f64::from(r))?
                }
                3 => {
                    let r = a.sqrt();
                    if !r.is_normal() || r * r != a {
                        return None;
                    }
                    float(r)?
                }
                _ => {
                    let bits = softfloat::sse_binary(0, a.to_bits(), 4, 8);
                    let r = f64::from_bits(bits);
                    if !r.is_normal() {
                        return None;
                    }
                    float(r)?
                }
            }
        }
        Op::X87Round {
            value,
            rounding,
            denormal,
            ..
        } => {
            let a = f(value);
            if a.is_nan() || (denormal && a.is_subnormal()) {
                return None;
            }
            let r = match rounding {
                0 => a.round_ties_even(),
                1 => a.floor(),
                2 => a.ceil(),
                _ => a.trunc(),
            };
            float(r)?
        }
        Op::X87Compare { a, b, ordered } => {
            let (a, b) = (f(a), f(b));
            if signaling(a) || signaling(b) || (ordered && (a.is_nan() || b.is_nan())) {
                return None;
            }
            let cc = if a.is_nan() || b.is_nan() {
                0x4500u32
            } else if a < b {
                0x0100
            } else if a == b {
                0x4000
            } else {
                0x0000
            };
            u128::from(cc)
        }
        Op::X87Exam(value) => {
            let x = f(value);
            let sign = u32::from(x.to_bits() >> 63 != 0) << 9;
            let cc = if x.is_nan() {
                0x0100u32
            } else if x.is_infinite() {
                0x0500
            } else if x == 0.0 {
                0x4000
            } else if x.is_subnormal() {
                0x4400
            } else {
                0x0400
            };
            u128::from(cc | sign)
        }
        Op::X87RoundC1 {
            op,
            a,
            b,
            pc,
            rounding,
        } => {
            let (a, b) = (f(a), f(b));
            let up = if rounding == 0 && pc != 3 {
                rounded_up(op, a, b, pc == 0)
            } else if pc == 3 || rounding == 3 {
                false
            } else {
                use adv_cpu_core::floating::softfloat;
                softfloat::configure_sse(0x1f80 | (u32::from(rounding) << 13));
                let (bits, res) = if pc == 0 {
                    let bits = softfloat::sse_binary(
                        u64::from((a as f32).to_bits()),
                        u64::from((b as f32).to_bits()),
                        op as u8,
                        4,
                    );
                    (bits, f64::from(f32::from_bits(bits as u32)))
                } else {
                    let bits = softfloat::sse_binary(a.to_bits(), b.to_bits(), op as u8, 8);
                    (bits, f64::from_bits(bits))
                };
                let _ = bits;
                let inexact = softfloat::flags() & 0x20 != 0;
                inexact && ((rounding == 1 && res < 0.0) || (rounding == 2 && res > 0.0))
            };
            u128::from(up)
        }
        Op::X87SqrtC1 { input, result } => {
            let (a, r) = (f(input), f(result));
            u128::from(r.mul_add(r, -a) > 0.0)
        }
        Op::SsePackedFp {
            op, double, a, b, ..
        } => {
            let (av, bv) = (get(a), get(b));
            if double {
                let mut out = 0u128;
                for shift in [0usize, 64] {
                    let x = f64::from_bits((av >> shift) as u64);
                    let y = f64::from_bits((bv >> shift) as u64);
                    let r = arithmetic(op, x, y)?;
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            } else {
                let mut out = 0u128;
                let sum = matches!(op, FpOp::Add | FpOp::Sub);
                for lane in 0..4usize {
                    let shift = lane * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    let y = f32::from_bits((bv >> shift) as u32);
                    let r = match op {
                        FpOp::Add => x + y,
                        FpOp::Sub => x - y,
                        FpOp::Mul => x * y,
                        FpOp::Div => x / y,
                    };
                    if !r.is_finite() || (!sum && r.abs() <= f32::MIN_POSITIVE) {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            }
        }
        Op::SseMinMax {
            max,
            scalar,
            double,
            a,
            b,
            ..
        } => {
            let (av, bv) = (get(a), get(b));
            let lanes = if scalar {
                1
            } else if double {
                2
            } else {
                4
            };
            let mut out = 0u128;
            if double {
                for lane in 0..lanes {
                    let shift = lane * 64;
                    let x = f64::from_bits((av >> shift) as u64);
                    let y = f64::from_bits((bv >> shift) as u64);
                    if x.is_nan() || y.is_nan() {
                        return None;
                    }
                    let take_x = if max { x > y } else { x < y };
                    let r = if take_x { x } else { y };
                    out |= u128::from(r.to_bits()) << shift;
                }
            } else {
                for lane in 0..lanes {
                    let shift = lane * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    let y = f32::from_bits((bv >> shift) as u32);
                    if x.is_nan() || y.is_nan() {
                        return None;
                    }
                    let take_x = if max { x > y } else { x < y };
                    let r = if take_x { x } else { y };
                    out |= u128::from(r.to_bits()) << shift;
                }
            }
            out
        }
        Op::SseSqrt {
            scalar, double, a, ..
        } => {
            let av = get(a);
            let lanes = if scalar {
                1
            } else if double {
                2
            } else {
                4
            };
            let mut out = 0u128;
            if double {
                for lane in 0..lanes {
                    let shift = lane * 64;
                    let x = f64::from_bits((av >> shift) as u64);
                    if x.is_nan() || x < 0.0 {
                        return None;
                    }
                    let r = x.sqrt();
                    if r.is_subnormal() || (r != 0.0 && r.abs() <= f64::MIN_POSITIVE) {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
            } else {
                for lane in 0..lanes {
                    let shift = lane * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    if x.is_nan() || x < 0.0 {
                        return None;
                    }
                    let r = x.sqrt();
                    if r.is_subnormal() || (r != 0.0 && r.abs() <= f32::MIN_POSITIVE) {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
            }
            out
        }
        Op::SseConvert { kind, a, .. } => {
            let av = get(a);
            match kind {
                SseConvertKind::WidenScalar => {
                    let x = f32::from_bits(av as u32);
                    if x.is_nan() {
                        return None;
                    }
                    u128::from(f64::from(x).to_bits())
                }
                SseConvertKind::WidenPacked => {
                    let mut out = 0u128;
                    for lane in 0..2usize {
                        let x = f32::from_bits((av >> (lane * 32)) as u32);
                        if x.is_nan() {
                            return None;
                        }
                        out |= u128::from(f64::from(x).to_bits()) << (lane * 64);
                    }
                    out
                }
                SseConvertKind::NarrowScalar => {
                    let x = f64::from_bits(av as u64);
                    let r = x as f32;
                    if !r.is_finite() || (x != 0.0 && r.abs() < f32::from_bits(0x0100_0000)) {
                        return None;
                    }
                    u128::from(r.to_bits())
                }
                SseConvertKind::NarrowPacked => {
                    let mut out = 0u128;
                    for lane in 0..2usize {
                        let x = f64::from_bits((av >> (lane * 64)) as u64);
                        let r = x as f32;
                        if !r.is_finite() || (x != 0.0 && r.abs() < f32::from_bits(0x0100_0000)) {
                            return None;
                        }
                        out |= u128::from(r.to_bits()) << (lane * 32);
                    }
                    out
                }
            }
        }
        Op::SseScalarToInt {
            value,
            double,
            truncate,
        } => {
            let av = get(value);
            let x = if double {
                f64::from_bits(av as u64)
            } else {
                f64::from(f32::from_bits(av as u32))
            };
            let rounded = if truncate {
                x.trunc()
            } else {
                x.round_ties_even()
            };
            if x.is_nan() || rounded < f64::from(i32::MIN) || rounded >= -f64::from(i32::MIN) {
                return None;
            }
            u128::from((rounded as i32) as u32)
        }
        Op::FpValidate { values, strict } => {
            for (idx, v) in values.into_iter().enumerate() {
                let Some(v) = v else { continue };
                let x = f(v);
                if !x.is_finite() || x.is_subnormal() {
                    return None;
                }
                // Non-strict lanes (`Add`/`Sub`) accept `±0.0`; strict lanes (`Mul`)
                // already verified exact `0.0` at the op or via FPSR, so only reject
                // subnormal / smallest normal binade / non-finite here.
                if (strict >> idx) & 1 != 0
                    && x != 0.0
                    && x.abs() < f64::from_bits(0x0020_0000_0000_0000)
                {
                    return None;
                }
            }
            0
        }
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
        Op::SsePackedToInt {
            value,
            double,
            truncate,
        } => {
            let v = get(value);
            if double {
                let hi_limit = if truncate {
                    0x41e0_0000u32
                } else {
                    0x41df_0000u32
                };
                let mut out = 0u128;
                for lane in 0..2usize {
                    let bits = (v >> (lane * 64)) as u64;
                    if ((bits >> 32) & 0x7fff_ffff) as u32 >= hi_limit {
                        return None;
                    }
                    let x = f64::from_bits(bits);
                    let r = if truncate {
                        x.trunc()
                    } else {
                        x.round_ties_even()
                    };
                    out |= u128::from((r as i32) as u32) << (lane * 32);
                }
                out
            } else {
                let mut out = 0u128;
                for lane in 0..4usize {
                    let bits = (v >> (lane * 32)) as u32;
                    if (bits << 1) >= 0x9e00_0000 {
                        return None;
                    }
                    let x = f32::from_bits(bits);
                    let r = if truncate {
                        x.trunc()
                    } else {
                        x.round_ties_even()
                    };
                    out |= u128::from((r as i32) as u32) << (lane * 32);
                }
                out
            }
        }
        Op::X87Store {
            value,
            width,
            rounding,
            integer,
            proved,
        } => {
            let value = f(value);
            if integer {
                let rounded = match rounding {
                    0 => value.round_ties_even(),
                    1 => value.floor(),
                    2 => value.ceil(),
                    _ => value.trunc(),
                };
                let limit = (1u64 << (width * 8 - 1)) as f64;
                let out_of_range = if proved {
                    !rounded.is_finite() || rounded < -limit || rounded >= limit
                } else {
                    !rounded.is_finite() || rounded.abs() >= limit
                };
                if out_of_range {
                    return None;
                }
                let n = rounded as i64;
                if width == 8 {
                    n as u64 as u128
                } else {
                    n as u32 as u128
                }
            } else {
                let rounded = round_f64_to_f32(value, rounding);
                let bad = if proved {
                    value != 0.0
                        && (!rounded.is_normal()
                            || rounded.abs() == f32::MIN_POSITIVE && f64::from(rounded) != value)
                } else {
                    // Unproved backend checks exp in 2..=254, excluding exp == 1 (< 2^-125).
                    value != 0.0
                        && (!rounded.is_normal() || rounded.abs() < f32::from_bits(0x0100_0000))
                };
                if bad {
                    return None;
                }
                u128::from(rounded.to_bits())
            }
        }
        Op::X87StoreC1 {
            value,
            stored,
            integer,
        } => {
            let rounded = if !integer {
                f64::from(f32::from_bits(get(stored) as u32))
            } else if wide(stored) {
                get(stored) as u64 as i64 as f64
            } else {
                f64::from(get(stored) as u32 as i32)
            };
            u128::from(rounded.abs() > f(value).abs())
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
        Op::VecFp { op, double, a, b } => {
            let (av, bv) = (get(a), get(b));
            if double {
                let mut out = 0u128;
                for shift in [0usize, 64] {
                    let x = f64::from_bits((av >> shift) as u64);
                    let y = f64::from_bits((bv >> shift) as u64);
                    let r = match op {
                        FpOp::Add => x + y,
                        FpOp::Sub => x - y,
                        FpOp::Mul => x * y,
                        FpOp::Div => x / y,
                    };
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            } else {
                let mut out = 0u128;
                for lane in 0..4usize {
                    let shift = lane * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    let y = f32::from_bits((bv >> shift) as u32);
                    let r = match op {
                        FpOp::Add => x + y,
                        FpOp::Sub => x - y,
                        FpOp::Mul => x * y,
                        FpOp::Div => x / y,
                    };
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            }
        }
        Op::VecFpMulElem { double, a, b, lane } => {
            let (av, bv) = (get(a), get(b));
            if double {
                let s = f64::from_bits((bv >> (usize::from(lane) * 64)) as u64);
                let mut out = 0u128;
                for shift in [0usize, 64] {
                    let x = f64::from_bits((av >> shift) as u64);
                    let r = x * s;
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            } else {
                let s = f32::from_bits((bv >> (usize::from(lane) * 32)) as u32);
                let mut out = 0u128;
                for l in 0..4usize {
                    let shift = l * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    let r = x * s;
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            }
        }
        Op::VecDeintLoad {
            addr,
            channels,
            channel,
            double,
            ..
        } => {
            let base = get(addr) as u32;
            let elem = if double { 8u32 } else { 4u32 };
            let stride = elem * u32::from(channels);
            let lanes = if double { 2u32 } else { 4u32 };
            let mut out = 0u128;
            for l in 0..lanes {
                let ea = base
                    .wrapping_add(l.wrapping_mul(stride))
                    .wrapping_add(u32::from(channel).wrapping_mul(elem));
                let mut bytes = [0u8; 16];
                memory.read_into(ea, &mut bytes[..elem as usize]).ok()?;
                let raw = u128::from_le_bytes(bytes);
                out |= raw << (l * elem * 8);
            }
            out
        }
        Op::VecLoadBcast { addr, double, .. } => {
            let base = get(addr) as u32;
            let mut bytes = [0u8; 16];
            if double {
                memory.read_into(base, &mut bytes[..8]).ok()?;
                let raw = u128::from_le_bytes(bytes);
                raw | (raw << 64)
            } else {
                memory.read_into(base, &mut bytes[..4]).ok()?;
                let raw = u128::from_le_bytes(bytes);
                raw | (raw << 32) | (raw << 64) | (raw << 96)
            }
        }
        Op::VecFma {
            sub,
            double,
            a,
            b,
            c,
        } => {
            let (av, bv, cv) = (get(a), get(b), get(c));
            if double {
                let mut out = 0u128;
                for shift in [0usize, 64] {
                    let x = f64::from_bits((av >> shift) as u64);
                    let y = f64::from_bits((bv >> shift) as u64);
                    let z = f64::from_bits((cv >> shift) as u64);
                    let r = if sub {
                        (-x).mul_add(y, z)
                    } else {
                        x.mul_add(y, z)
                    };
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            } else {
                let mut out = 0u128;
                for lane in 0..4usize {
                    let shift = lane * 32;
                    let x = f32::from_bits((av >> shift) as u32);
                    let y = f32::from_bits((bv >> shift) as u32);
                    let z = f32::from_bits((cv >> shift) as u32);
                    let r = if sub {
                        (-x).mul_add(y, z)
                    } else {
                        x.mul_add(y, z)
                    };
                    if !r.is_finite() || r.is_subnormal() {
                        return None;
                    }
                    out |= u128::from(r.to_bits()) << shift;
                }
                out
            }
        }
        Op::VecFpToInt {
            value,
            double,
            rounding,
        } => {
            let v = get(value);
            if double {
                let mut out = 0u128;
                for shift in [0usize, 64] {
                    let x = f64::from_bits((v >> shift) as u64);
                    if !x.is_finite() {
                        return None;
                    }
                    let rounded = match rounding {
                        0 => x.round_ties_even(),
                        1 => x.floor(),
                        2 => x.ceil(),
                        _ => x.trunc(),
                    };
                    if rounded < -(2.0f64.powi(63)) || rounded >= 2.0f64.powi(63) {
                        return None;
                    }
                    out |= u128::from((rounded as i64) as u64) << shift;
                }
                out
            } else {
                let mut out = 0u128;
                for lane in 0..4usize {
                    let shift = lane * 32;
                    let x = f32::from_bits((v >> shift) as u32);
                    if !x.is_finite() {
                        return None;
                    }
                    let rounded = match rounding {
                        0 => x.round_ties_even(),
                        1 => x.floor(),
                        2 => x.ceil(),
                        _ => x.trunc(),
                    };
                    if rounded < -(2.0f32.powi(31)) || rounded >= 2.0f32.powi(31) {
                        return None;
                    }
                    out |= u128::from((rounded as i32) as u32) << shift;
                }
                out
            }
        }
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
            ..
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
                    if packed {
                        (0..4)
                            .any(|lane| f32::from_bits((bits >> (lane * 32)) as u32).is_subnormal())
                    } else {
                        f32::from_bits(bits as u32).is_subnormal()
                    }
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
        Op::DomainGuard { kind, .. } => {
            let ok = match kind {
                DomainGuardKind::FpWindow {
                    value,
                    min_exp,
                    max_exp,
                    exact_f32,
                } => {
                    let x = f(value);
                    let bits = x.to_bits();
                    let exp = ((bits >> 52) & 0x7ff) as i16 - 1023;
                    x.is_finite()
                        && (x == 0.0 || (exp >= min_exp && exp <= max_exp))
                        && (!exact_f32 || f64::from(x as f32) == x)
                }
                DomainGuardKind::SameSign { a, b } => {
                    let (a, b) = (f(a), f(b));
                    a == 0.0 || b == 0.0 || a.is_sign_positive() == b.is_sign_positive()
                }
                DomainGuardKind::FpLe { value, bound_bits } => {
                    f(value) <= f64::from_bits(bound_bits)
                }
                DomainGuardKind::U32Le { value, max } => (get(value) as u32) <= max,
                DomainGuardKind::ExactF32 { value } => {
                    let x = f(value);
                    f64::from(x as f32) == x
                }
            };
            if !ok {
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
        Op::Validate | Op::ScopeWritten(_) => 0,
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
            let ordered = kind == FlagKind::OrderedCompare;
            let (p, q) = if width == 32 {
                let pf = f32::from_bits(a as u32);
                let qf = f32::from_bits(b as u32);
                if signaling32(pf) || signaling32(qf) || ordered && (pf.is_nan() || qf.is_nan()) {
                    return None;
                }
                (f64::from(pf), f64::from(qf))
            } else {
                let pd = f64::from_bits(a as u64);
                let qd = f64::from_bits(b as u64);
                if signaling(pd) || signaling(qd) || ordered && (pd.is_nan() || qd.is_nan()) {
                    return None;
                }
                (pd, qd)
            };
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
        FlagKind::Sahf => {
            let ah = x & 0xff;
            let mask = integer::SF | ZF | integer::AF | PF | CF;
            out = (out & !mask) | (ah & mask);
        }
        FlagKind::X87CompareFold => {
            let p = f64::from_bits(a as u64);
            let q = f64::from_bits(b as u64);
            let ordered = width & 2 != 0;
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
            let af = if width & 1 != 0 { integer::AF } else { 0 };
            out = out & !(ZF | PF | CF | integer::SF | integer::AF) | set | af;
        }
    }
    Some(out)
}

fn signaling(x: f64) -> bool {
    x.is_nan() && x.to_bits() & 1 << 51 == 0
}

fn signaling32(x: f32) -> bool {
    x.is_nan() && x.to_bits() & 1 << 22 == 0
}

fn round_f64_to_f32(x: f64, rounding: u8) -> f32 {
    let s = x as f32;
    if !x.is_finite() || f64::from(s) == x {
        return s;
    }
    let w = f64::from(s);
    match rounding {
        0 => s,
        1 => {
            if w > x {
                s.next_down()
            } else {
                s
            }
        }
        2 => {
            if w < x {
                s.next_up()
            } else {
                s
            }
        }
        _ => {
            if w.abs() > x.abs() {
                if x < 0.0 { s.next_up() } else { s.next_down() }
            } else {
                s
            }
        }
    }
}

fn binary80_eval(bits: u64) -> adv_cpu_core::state::X87Register {
    let sign = ((bits >> 48) as u16) & 0x8000;
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1 << 52) - 1);
    let (significand, exponent) = match exponent {
        0 if fraction == 0 => (0, 0),
        0 => {
            let shift = fraction.leading_zeros();
            (fraction << shift, (16383 - 1074 + 63 - shift) as u16)
        }
        0x7ff => ((1 << 63) | fraction << 11, 0x7fff),
        _ => ((1 << 63) | fraction << 11, (exponent + 16383 - 1023) as u16),
    };
    adv_cpu_core::state::X87Register::from_parts(significand, sign | exponent)
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
        VecUnary::WidenScalar => u128::from(f64::from(f32::from_bits(a as u32)).to_bits()),
        VecUnary::FromInt4s => {
            let mut out = 0u128;
            for lane in 0..4usize {
                let n = ((a >> (lane * 32)) as u32) as i32;
                out |= u128::from((n as f32).to_bits()) << (lane * 32);
            }
            out
        }
        VecUnary::FromInt2d => {
            let n0 = (a as u32) as i32;
            let n1 = ((a >> 32) as u32) as i32;
            u128::from((f64::from(n0)).to_bits()) | (u128::from((f64::from(n1)).to_bits()) << 64)
        }
        VecUnary::Dup64 => {
            let lo = a & u128::from(u64::MAX);
            lo | (lo << 64)
        }
        VecUnary::Dup32 => {
            let lo = a & u128::from(u32::MAX);
            lo | (lo << 32) | (lo << 64) | (lo << 96)
        }
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
        VecToInt::Extract32(lane) => ((a >> (usize::from(lane) * 32)) & 0xffff_ffff) as u32,
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
        VecOp::Insert32(lane) => {
            let val = b & u128::from(u32::MAX);
            let shift = usize::from(lane) * 32;
            (a & !(u128::from(u32::MAX) << shift)) | (val << shift)
        }
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
        Location::Recipe(Recipe::C1 { op, a, b, single }) => u128::from(rounded_up(
            op,
            f64::from_bits(get(a) as u64),
            f64::from_bits(get(b) as u64),
            single,
        )),
        Location::Recipe(Recipe::C1Fma {
            op,
            a,
            b,
            c,
            single,
        }) => {
            let prod = f64::from_bits(get(a) as u64) * f64::from_bits(get(b) as u64);
            let cv = f64::from_bits(get(c) as u64);
            let (fp_op, lhs, rhs) = match op {
                FmaOp::Add => (FpOp::Add, prod, cv),
                FmaOp::Sub => (FpOp::Sub, prod, cv),
                FmaOp::RevSub => (FpOp::Sub, cv, prod),
            };
            u128::from(rounded_up(fp_op, lhs, rhs, single))
        }
        Location::Recipe(Recipe::VecLane { vec, lane, double }) => {
            let bits = get(vec);
            if double {
                u128::from((bits >> (u32::from(lane) * 64)) as u64)
            } else {
                let f = f32::from_bits((bits >> (u32::from(lane) * 32)) as u32);
                u128::from(f64::from(f).to_bits())
            }
        }
        Location::Recipe(Recipe::C1Vec {
            op,
            a,
            b,
            lane,
            single,
        }) => {
            let lane_f64 = |v: Value| {
                let bits = get(v);
                if single {
                    f64::from(f32::from_bits((bits >> (u32::from(lane) * 32)) as u32))
                } else {
                    f64::from_bits((bits >> (u32::from(lane) * 64)) as u64)
                }
            };
            u128::from(rounded_up(op, lane_f64(a), lane_f64(b), single))
        }
    }
}

/// C1 after a nearest-rounded `a op b` without other exceptions: whether
/// the magnitude was rounded up.
pub(crate) fn rounded_up(op: FpOp, a: f64, b: f64, single: bool) -> bool {
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
    if single {
        if r == 0.0 {
            return false;
        }
        let (rounded, grew) = adv_cpu_core::floating::fast::round_single_precision(r);
        if rounded != r {
            return grew;
        }
    }
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
        let p = f.push(b, Op::Fp(FpOp::Mul, x, y, false)).unwrap();
        f.push(b, Op::Validate);
        f.push(
            b,
            Op::Store {
                scope: None,
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
                scope: None,
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
                proved: false,
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
                    cond: crate::translate::front::tags::ConditionCode::ne,
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
        assert!(rounded_up(FpOp::Div, 1.0, 10.0, false));
        assert!(rounded_up(FpOp::Add, 0.1, 0.2, false));
        assert!(!rounded_up(FpOp::Div, 1.0, 3.0, false));
        assert!(!rounded_up(FpOp::Mul, 1.5, 2.0, false));
        assert!(rounded_up(FpOp::Add, 1.0, f64::EPSILON * 0.75, false));
    }
}
