//! A bounded integer proof rule for Paeth's straight-line predictor. Linear
//! combinations of three byte inputs stay in signed-32 range; abs/select
//! rewrites retain compare signedness, truncation and strict tie ordering.
//! This is preparation only, with no solver or execution-time evaluator.
use iced_x86::{ConditionCode as Cc, Mnemonic as M};

use super::expression::Kind;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Linear {
    terms: [i64; 3],
    constant: i64,
}

impl Linear {
    fn constant(value: i64) -> Self {
        Self {
            terms: [0; 3],
            constant: value,
        }
    }
    fn input(index: usize) -> Self {
        let mut value = Self::constant(0);
        value.terms[index] = 1;
        value
    }
    fn range(self) -> (i64, i64) {
        let mut lo = self.constant;
        let mut hi = lo;
        for term in self.terms {
            lo += term.min(0) * 255;
            hi += term.max(0) * 255;
        }
        (lo, hi)
    }
    fn combine(self, other: Self, subtract: bool) -> Option<Self> {
        let sign = if subtract { -1 } else { 1 };
        let mut value = self;
        value.constant = value
            .constant
            .checked_add(other.constant.checked_mul(sign)?)?;
        for (a, b) in value.terms.iter_mut().zip(other.terms) {
            *a = a.checked_add(b.checked_mul(sign)?)?;
        }
        let (lo, hi) = value.range();
        (lo >= i64::from(i32::MIN) && hi <= i64::from(i32::MAX)).then_some(value)
    }
    fn negate(self) -> Option<Self> {
        Self::constant(0).combine(self, true)
    }
    fn absolute(mut self) -> Self {
        if self
            .terms
            .into_iter()
            .find(|&v| v != 0)
            .unwrap_or(self.constant)
            < 0
        {
            self.terms = self.terms.map(|v| -v);
            self.constant = -self.constant;
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Linear(Linear),
    Absolute(Linear),
    Select(Box<Compare>, Box<Value>, Box<Value>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Compare {
    left: Value,
    right: Value,
}

impl Value {
    fn linear(&self) -> Option<Linear> {
        if let Self::Linear(v) = self {
            Some(*v)
        } else {
            None
        }
    }
    fn range(&self) -> (i64, i64) {
        match self {
            Self::Linear(v) => v.range(),
            Self::Absolute(v) => {
                let (lo, hi) = v.range();
                (0, lo.abs().max(hi.abs()))
            }
            Self::Select(_, a, b) => {
                let (al, ah) = a.range();
                let (bl, bh) = b.range();
                (al.min(bl), ah.max(bh))
            }
        }
    }
    fn select(condition: Compare, yes: Self, no: Self) -> Self {
        if yes == no {
            return yes;
        }
        if let (Some(a), Some(b), Some(left), Some(right)) = (
            yes.linear(),
            no.linear(),
            condition.left.linear(),
            condition.right.linear(),
        ) && let Some(difference) = left.combine(right, true)
            && let Some(negative) = difference.negate()
            && a == negative
            && b == difference
        {
            return Self::Absolute(difference.absolute());
        }
        Self::Select(Box::new(condition), Box::new(yes), Box::new(no))
    }
}

struct Normalizer {
    variables: [Expression; 3],
    remaining: usize,
    exhausted: bool,
}

impl Normalizer {
    fn step(&mut self) -> Option<()> {
        if self.remaining == 0 {
            self.exhausted = true;
            None
        } else {
            self.remaining -= 1;
            Some(())
        }
    }
    fn value(&mut self, expression: &Expression) -> Option<Value> {
        self.step()?;
        if let Some(index) = self.variables.iter().position(|v| v == expression) {
            return Some(Value::Linear(Linear::input(index)));
        }
        match expression.kind() {
            Kind::Constant(v) => Some(Value::Linear(Linear::constant(match expression.width() {
                Width::Dword => i64::from(*v as u32 as i32),
                Width::Byte => *v as i64,
                _ => return None,
            }))),
            Kind::Binary(op @ (Binary::Add | Binary::Subtract), a, b)
                if expression.width() == Width::Dword =>
            {
                let a = self.value(a)?.linear()?;
                let b = self.value(b)?.linear()?;
                Some(Value::Linear(a.combine(b, *op == Binary::Subtract)?))
            }
            Kind::Extend(inner, signed) => {
                let value = self.value(inner)?;
                let (lo, hi) = value.range();
                let limit = if *signed {
                    (1i64 << (inner.width().bits() - 1)) - 1
                } else {
                    inner.width().mask() as i64
                };
                (lo >= 0 && hi <= limit).then_some(value)
            }
            Kind::Extract(inner, 0) => {
                let value = self.value(inner)?;
                let (lo, hi) = value.range();
                (lo >= 0 && hi <= expression.width().mask() as i64).then_some(value)
            }
            Kind::Select(condition, yes, no) => {
                let (condition, inverse) = self.condition(condition)?;
                let (yes, no) = (self.value(yes)?, self.value(no)?);
                Some(if inverse {
                    Value::select(condition, no, yes)
                } else {
                    Value::select(condition, yes, no)
                })
            }
            _ => None,
        }
    }
    fn condition(&mut self, expression: &Expression) -> Option<(Compare, bool)> {
        self.step()?;
        let Kind::Condition(cc, flags) = expression.kind() else {
            return None;
        };
        let Kind::IntegerFlags(kind, _, left, right) = flags.kind() else {
            return None;
        };
        if left.width() != Width::Dword {
            return None;
        }
        let a = self.value(left)?;
        let b = self.value(right)?;
        if matches!(kind, M::Cmp | M::Sub) {
            match cc {
                Cc::b | Cc::ae | Cc::a | Cc::be if a.range().0 >= 0 && b.range().0 >= 0 => {
                    let reverse = matches!(cc, Cc::a | Cc::be);
                    return Some((
                        if reverse {
                            Compare { left: b, right: a }
                        } else {
                            Compare { left: a, right: b }
                        },
                        matches!(cc, Cc::ae | Cc::be),
                    ));
                }
                Cc::l | Cc::ge => return Some((Compare { left: a, right: b }, *cc == Cc::ge)),
                Cc::g | Cc::le => return Some((Compare { left: b, right: a }, *cc == Cc::le)),
                _ => {}
            }
        }
        if matches!(cc, Cc::s | Cc::ns) {
            let a = a.linear()?;
            let b = b.linear()?;
            let result = match kind {
                M::Cmp | M::Sub => a.combine(b, true)?,
                M::Add => a.combine(b, false)?,
                M::Neg => a.negate()?,
                _ => return None,
            };
            return Some((
                Compare {
                    left: Value::Linear(result),
                    right: Value::Linear(Linear::constant(0)),
                },
                *cc == Cc::ns,
            ));
        }
        None
    }
}

fn reference() -> Value {
    let (a, b, c) = (Linear::input(0), Linear::input(1), Linear::input(2));
    let pa = Value::Absolute(b.combine(c, true).unwrap().absolute());
    let pb = Value::Absolute(a.combine(c, true).unwrap().absolute());
    let pc = Value::Absolute(
        a.combine(b, false)
            .unwrap()
            .combine(c, true)
            .unwrap()
            .combine(c, true)
            .unwrap()
            .absolute(),
    );
    let first = Compare {
        left: pb.clone(),
        right: pa.clone(),
    };
    let best = Value::select(first.clone(), Value::Linear(b), Value::Linear(a));
    let distance = Value::select(first, pb, pa);
    Value::select(
        Compare {
            left: pc,
            right: distance,
        },
        Value::Linear(c),
        best,
    )
}

/// Successful admission uses exact normalized identities, never sample agreement.
pub(super) fn prove(value: &Expression, view: &StateView, at: u32) -> Result<(), Rejection> {
    let previous = view.input(Input::PreviousRow).unwrap();
    let left = view.input(Input::ReconstructedPrefix).unwrap();
    let distance = view.input(Input::BytesPerPixel).unwrap();
    let upper = Expression::binary(Binary::Subtract, previous.clone(), distance.clone()).unwrap();
    let byte = |address| Expression::load(MemoryVersion(0), address, Width::Byte).unwrap();
    let filtered = byte(view.output.address.clone());
    let predictor = match value.kind() {
        Kind::Binary(Binary::Add, a, b) if value.width() == Width::Byte => {
            if *a == filtered {
                Some(b)
            } else if *b == filtered {
                Some(a)
            } else {
                None
            }
        }
        _ => None,
    };
    let mut normalizer = Normalizer {
        variables: [byte(left.clone()), byte(previous.clone()), byte(upper)],
        remaining: 4096,
        exhausted: false,
    };
    if predictor.and_then(|p| normalizer.value(p)).as_ref() == Some(&reference()) {
        return Ok(());
    }
    if normalizer.exhausted {
        Err(Rejection::Timeout)
    } else {
        Err(Rejection::Unsupported {
            offset: at,
            reason: "Paeth predictor outside the bounded integer rule",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Value {
        fn evaluate(&self, inputs: [i64; 3]) -> i64 {
            let linear = |v: Linear| {
                v.constant
                    + v.terms
                        .into_iter()
                        .zip(inputs)
                        .map(|(a, b)| a * b)
                        .sum::<i64>()
            };
            match self {
                Self::Linear(v) => linear(*v),
                Self::Absolute(v) => linear(*v).abs(),
                Self::Select(c, a, b) => if c.left.evaluate(inputs) < c.right.evaluate(inputs) {
                    a
                } else {
                    b
                }
                .evaluate(inputs),
            }
        }
    }

    fn variables() -> [Expression; 3] {
        std::array::from_fn(|n| {
            Expression::load(
                MemoryVersion(0),
                Expression::constant(Width::Dword, 0x1000 + n as u64),
                Width::Byte,
            )
            .unwrap()
        })
    }
    fn normalizer() -> Normalizer {
        Normalizer {
            variables: variables(),
            remaining: 4096,
            exhausted: false,
        }
    }
    fn wide(v: Expression) -> Expression {
        Expression::extend(v, Width::Dword, false).unwrap()
    }
    fn subtract(a: Expression, b: Expression) -> Expression {
        Expression::binary(Binary::Subtract, a, b).unwrap()
    }
    fn abs_from_sign(value: Expression) -> Expression {
        let zero = Expression::constant(Width::Dword, 0);
        let flags =
            Expression::integer_flags(M::Neg, Expression::flags(), value.clone(), zero.clone())
                .unwrap();
        Expression::select(
            Expression::condition(Cc::s, flags).unwrap(),
            value.clone(),
            subtract(zero, value),
        )
        .unwrap()
    }

    // spec: EXEC-NATIVE-5
    #[test]
    fn abs_rewrites_match_shared_flag_semantics_for_every_byte_pair() {
        let [a, b, _] = variables().map(wide);
        let flags =
            Expression::integer_flags(M::Sub, Expression::flags(), a.clone(), b.clone()).unwrap();
        let select = Expression::select(
            Expression::condition(Cc::ae, flags).unwrap(),
            subtract(a.clone(), b.clone()),
            subtract(b.clone(), a.clone()),
        )
        .unwrap();
        let signed = abs_from_sign(subtract(a, b));
        for expression in [select, signed] {
            let normalized = normalizer().value(&expression).unwrap();
            for a in 0..=255u64 {
                for b in 0..=255u64 {
                    let actual =
                        expression.evaluate(&[0; 8], 0x2ad7, &|version, address, width| {
                            assert_eq!(version, MemoryVersion(0));
                            assert_eq!(width, Width::Byte);
                            [a, b, 0][(address - 0x1000) as usize]
                        });
                    assert_eq!(actual as i64, normalized.evaluate([a as i64, b as i64, 0]));
                    assert_eq!(actual as i64, (a as i64 - b as i64).abs());
                }
            }
        }
    }

    // spec: EXEC-NATIVE-5
    #[test]
    fn predictor_width_and_strict_final_ties_remain_part_of_the_proof() {
        let [a, b, c] = variables().map(wide);
        let sum = Expression::binary(Binary::Add, a, b).unwrap();
        let pc = abs_from_sign(subtract(subtract(sum, c.clone()), c));
        let byte = Expression::extract(pc.clone(), 0, Width::Byte).unwrap();
        assert!(normalizer().value(&pc).is_some());
        assert!(normalizer().value(&byte).is_none(), "pc may exceed 255");
        let min = Expression::binary(
            Binary::Add,
            wide(variables()[0].clone()),
            Expression::constant(Width::Dword, 0x80000000),
        )
        .unwrap();
        assert!(
            normalizer().value(&abs_from_sign(min)).is_none(),
            "NEG overflow cannot become mathematical abs"
        );
        let exact = reference();
        assert_eq!(exact.evaluate([120, 90, 100]), 120);
        assert_eq!(exact.evaluate([90, 120, 100]), 120);
        assert_eq!(exact.evaluate([120, 80, 100]), 100);
        let Value::Select(c, yes, no) = &exact else {
            unreachable!()
        };
        let wrong = Value::select(
            Compare {
                left: c.right.clone(),
                right: c.left.clone(),
            },
            (**no).clone(),
            (**yes).clone(),
        );
        assert_ne!(wrong, exact);
        assert_eq!(wrong.evaluate([120, 90, 100]), 100);
        let mut exhausted = normalizer();
        exhausted.remaining = 1;
        assert!(exhausted.value(&pc).is_none());
        assert!(exhausted.exhausted);
    }
}
