//! Executes a `Func` as the reference semantics of the IR. Front-end tests
//! compare it with the interpreter, and back-end tests compare emitted code
//! with it. Integer flags come from the interpreter's own functions. Floating
//! point follows AArch64 with `FPCR.FZ` set and `FPCR.DN` clear: denormal
//! inputs flush to zero with IDC, and tiny results flush to zero with UFC.

use adv_cpu_core::Memory;
use adv_cpu_core::integer::{self, CF, OF, PF, ZF};
use iced_x86::Mnemonic as M;

use super::ir::{
    Block, End, FlagKind, FpOp, FpUnary, Func, IntOp, IntUnary, Op, RecordId, Type, Value, VecOp,
};
use super::recovery::{Location, Recipe};

pub(crate) const IOC: u32 = 1;
pub(crate) const DZC: u32 = 1 << 1;
pub(crate) const OFC: u32 = 1 << 2;
pub(crate) const UFC: u32 = 1 << 3;
pub(crate) const IDC: u32 = 1 << 7;
/// The cumulative FPSR flags a validation checks.
pub(crate) const CHECKED: u32 = IOC | DZC | OFC | UFC | IDC;

const DEFAULT_NAN: u64 = 0x7ff8_0000_0000_0000;

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
}

/// Runs `func` from block 0 with `params`, for at most `limit` instructions.
pub(crate) fn run(func: &Func, params: &[u128], memory: &mut Memory, limit: usize) -> Run {
    let mut values = vec![0u128; func.types.len()];
    let mut fpsr = 0u32;
    let mut block = Block(0);
    let mut args = params.to_vec();
    let mut region = None;
    let mut steps = 0usize;
    let outcome = 'run: loop {
        let data = &func.blocks[block.0 as usize];
        for (&p, &v) in data.params.iter().zip(&args) {
            values[p.0 as usize] = v;
        }
        if let Some(head) = data.head {
            region = Some(head.record);
        }
        let fail = region.expect("block 0 is a head");
        for (result, op) in &data.insts {
            steps += 1;
            if steps > limit {
                break 'run Outcome::Limit;
            }
            if let Op::Store { after, .. } = *op {
                region = Some(after);
            }
            match step(func, op, &values, &mut fpsr, memory) {
                Some(bits) => {
                    if let Some(r) = result {
                        values[r.0 as usize] = bits;
                    }
                }
                None => break 'run Outcome::Fail(fail),
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
        block = target.block;
    };
    Run { outcome, values }
}

/// One instruction's result bits, or `None` when it fails.
fn step(
    func: &Func,
    op: &Op,
    values: &[u128],
    fpsr: &mut u32,
    memory: &mut Memory,
) -> Option<u128> {
    let get = |v: Value| values[v.0 as usize];
    let wide = |v: Value| func.ty(v) == Type::I64;
    let f = |v: Value| f64::from_bits(get(v) as u64);
    let float = |x: f64| Some(u128::from(x.to_bits()));
    Some(match *op {
        Op::Constant(_, bits) => bits,
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
            }
        }
        Op::Div {
            signed,
            remainder,
            dividend,
            divisor,
            ..
        } => {
            let (n, d) = (get(dividend) as u64, get(divisor) as u32);
            if d == 0 {
                return None;
            }
            let (q, r) = if signed {
                let (n, d) = (n as i64, i64::from(d as i32));
                let q = n.checked_div(d)?;
                i32::try_from(q).ok()?;
                (q as u64, (n % d) as u64)
            } else {
                let q = n / u64::from(d);
                u32::try_from(q).ok()?;
                (q, n % u64::from(d))
            };
            u128::from(if remainder { r } else { q } as u32)
        }
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
            fpsr,
        )),
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
        Op::Fp(op, a, b) => float(arithmetic(op, f(a), f(b), fpsr))?,
        Op::FpUnary(op, a) => {
            let x = f(a);
            match op {
                FpUnary::Neg => float(-x)?,
                FpUnary::Abs => float(x.abs())?,
                FpUnary::RoundZero | FpUnary::RoundDown => {
                    let x = input(x, fpsr);
                    if x.is_nan() {
                        float(quiet(x, fpsr))?
                    } else if op == FpUnary::RoundZero {
                        float(x.trunc())?
                    } else {
                        float(x.floor())?
                    }
                }
            }
        }
        Op::FromInt(a) => {
            let n = if wide(a) {
                get(a) as u64 as i64
            } else {
                i64::from(get(a) as u32 as i32)
            };
            let x = n as f64;
            if x as i128 != i128::from(n) {
                return None;
            }
            float(x)?
        }
        Op::ToInt { value, wide } => {
            let x = input(f(value), fpsr);
            let (lo, hi) = if wide {
                (i64::MIN as f64, -(i64::MIN as f64))
            } else {
                (f64::from(i32::MIN), -f64::from(i32::MIN))
            };
            let t = x.trunc();
            let n = if x.is_nan() || t < lo || t >= hi {
                *fpsr |= IOC;
                if x.is_nan() {
                    0
                } else if t < lo {
                    lo as i64
                } else {
                    hi as i64 - 1
                }
            } else {
                t as i64
            };
            if wide {
                u128::from(n as u64)
            } else {
                u128::from(n as u32)
            }
        }
        Op::Vec(op, a, b) => vector(op, get(a), get(b), fpsr),
        Op::Load { width, addr, .. } => {
            let mut bytes = [0u8; 16];
            memory
                .read_into(get(addr) as u32, &mut bytes[..usize::from(width)])
                .ok()?;
            u128::from_le_bytes(bytes)
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
        Op::X87Load { bits, denormal } => {
            let x = f(bits);
            let tiny = x != 0.0 && x.abs() < f64::MIN_POSITIVE;
            if x.is_nan() || denormal && tiny {
                return None;
            }
            float(x)?
        }
        Op::Validate => {
            if *fpsr & CHECKED != 0 {
                return None;
            }
            0
        }
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

fn flags(kind: FlagKind, width: u8, a: u128, b: u128, prior: u32, fpsr: &mut u32) -> u32 {
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
        FlagKind::MulSigned => {
            let product = integer::signed(x, width) * integer::signed(y, width);
            let low = product as u32 & integer::mask(width);
            let overflow = product != integer::signed(low, width);
            out = out & !(CF | OF) | if overflow { CF | OF } else { 0 };
        }
        FlagKind::UnorderedCompare => {
            let p = input(f64::from_bits(a as u64), fpsr);
            let q = input(f64::from_bits(b as u64), fpsr);
            if signaling(p) || signaling(q) {
                *fpsr |= IOC;
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
    out
}

fn signaling(x: f64) -> bool {
    x.is_nan() && x.to_bits() & 1 << 51 == 0
}

/// An operand after FZ: nonzero denormals become zero and set IDC.
fn input(x: f64, fpsr: &mut u32) -> f64 {
    if x != 0.0 && x.abs() < f64::MIN_POSITIVE {
        *fpsr |= IDC;
        0.0f64.copysign(x)
    } else {
        x
    }
}

/// A NaN operand's result: quieted, with IOC for a signaling one.
fn quiet(x: f64, fpsr: &mut u32) -> f64 {
    if signaling(x) {
        *fpsr |= IOC;
    }
    f64::from_bits(x.to_bits() | 1 << 51)
}

fn arithmetic(op: FpOp, a: f64, b: f64, fpsr: &mut u32) -> f64 {
    let (a, b) = (input(a, fpsr), input(b, fpsr));
    if a.is_nan() || b.is_nan() {
        // AArch64 returns the first signaling NaN, else the first quiet one.
        let first = if signaling(a) || !signaling(b) && a.is_nan() {
            a
        } else {
            b
        };
        if signaling(a) || signaling(b) {
            *fpsr |= IOC;
        }
        return quiet(first, &mut 0);
    }
    let r = match op {
        FpOp::Add => a + b,
        FpOp::Sub => a - b,
        FpOp::Mul => a * b,
        FpOp::Div => a / b,
    };
    if r.is_nan() {
        *fpsr |= IOC;
        return f64::from_bits(DEFAULT_NAN);
    }
    if r.is_infinite() {
        if a.is_finite() && b.is_finite() {
            *fpsr |= if op == FpOp::Div && b == 0.0 {
                DZC
            } else {
                OFC
            };
        }
        return r;
    }
    // Whether the exact result is nonzero, its sign, and the sign of the
    // exact result minus `r`, which tells a tiny value that rounded up to
    // the smallest normal. A tiny exact sum is exact (Hauser).
    let (nonzero, sign, error) = match op {
        FpOp::Add | FpOp::Sub => (r != 0.0, r, 0.0),
        FpOp::Mul => (
            a != 0.0 && b != 0.0,
            a.signum() * b.signum(),
            a.mul_add(b, -r),
        ),
        FpOp::Div => (
            a != 0.0 && b.is_finite(),
            a.signum() * b.signum(),
            (-r).mul_add(b, a) * b.signum(),
        ),
    };
    let tiny = nonzero
        && (r.abs() < f64::MIN_POSITIVE
            || r.abs() == f64::MIN_POSITIVE && error != 0.0 && error.signum() != r.signum());
    if tiny {
        *fpsr |= UFC;
        return 0.0f64.copysign(sign);
    }
    r
}

fn vector(op: VecOp, a: u128, b: u128, fpsr: &mut u32) -> u128 {
    let lanes64 = |f: &mut dyn FnMut(u64, u64) -> u64| {
        let lo = f(a as u64, b as u64);
        let hi = f((a >> 64) as u64, (b >> 64) as u64);
        u128::from(lo) | u128::from(hi) << 64
    };
    let count = b as u64;
    match op {
        VecOp::And => a & b,
        VecOp::AndNot => !a & b,
        VecOp::Or => a | b,
        VecOp::Xor => a ^ b,
        VecOp::SubI32 => (0..4).fold(0, |acc, i| {
            let lane = ((a >> (32 * i)) as u32).wrapping_sub((b >> (32 * i)) as u32);
            acc | u128::from(lane) << (32 * i)
        }),
        VecOp::ShlI64 => lanes64(&mut |x, _| if count > 63 { 0 } else { x << count }),
        VecOp::ShrI64 => lanes64(&mut |x, _| if count > 63 { 0 } else { x >> count }),
        VecOp::LessF64 => lanes64(&mut |x, y| {
            let (p, q) = (
                input(f64::from_bits(x), fpsr),
                input(f64::from_bits(y), fpsr),
            );
            if p.is_nan() || q.is_nan() {
                *fpsr |= IOC;
            }
            if p < q { u64::MAX } else { 0 }
        }),
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
fn rounded_up(op: FpOp, a: f64, b: f64) -> bool {
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
    use super::super::recovery::{IrRecord, X87};
    use super::*;

    fn record() -> IrRecord {
        IrRecord {
            eip: 0x1000,
            refund: 0,
            regs: [None; 8],
            eflags: None,
            x87: X87 {
                top: 0,
                tags: 0xffff,
                stack: [None; 8],
                status: 0,
                c1: None,
                ip: 0,
                opcode: 0,
            },
            xmm: [None; 8],
            frame: Vec::new(),
        }
    }

    /// `x * y`, validated, stored at `addr`, then an exit.
    fn store_product() -> (Func, RecordId, RecordId) {
        let mut f = Func::default();
        let entry = f.record(record());
        let after = f.record(record());
        let exit = f.record(record());
        let b = f.block(Some(Head {
            prepaid: 1,
            record: entry,
        }));
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

    #[test]
    fn x87_loads_refuse_nan_and_optionally_denormals() {
        let mut f = Func::default();
        let entry = f.record(record());
        let exit = f.record(record());
        let b = f.block(Some(Head {
            prepaid: 1,
            record: entry,
        }));
        let raw = f.param(b, Type::I64);
        f.push(
            b,
            Op::X87Load {
                bits: raw,
                denormal: true,
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
        let entry = f.record(record());
        let mut full = record();
        full.regs = [Some(Location::Constant(0)); 8];
        full.eflags = Some(Location::Constant(0));
        full.x87.c1 = Some(Location::Constant(0));
        full.xmm = [Some(Location::Constant(0)); 8];
        let exit = f.record(full);
        let head = f.block(Some(Head {
            prepaid: 2,
            record: entry,
        }));
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
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                },
            },
        );
        f.end(
            body,
            End::Jump(Target {
                block: head,
                args: vec![next, total],
            }),
        );
        f.end(done, End::Exit(exit));
        let run = run(&f, &[5, 0], &mut memory(), 1000);
        assert_eq!(run.outcome, Outcome::Exit(exit));
        assert_eq!(run.values[total.0 as usize], 15);
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
