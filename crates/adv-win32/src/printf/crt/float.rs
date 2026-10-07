//! Finite binary64 output for the default VC80/90 dot/nearest profile.
//! The authored VC80 oracle records legacy half-away rounding, 17 generated
//! significant digits followed by zero padding, and three-digit exponents.
//! See target/engine-loop/20261007-resume/crt-float-native/native.jsonl and
//! https://learn.microsoft.com/cpp/c-runtime-library/reference/printf-printf-l-wprintf-wprintf-l
use super::{Error, LIMIT};

// Larger explicit precisions are a reported profile boundary, not truncated.
const PRECISION_LIMIT: usize = 64;

struct Decimal {
    digits: Vec<u8>,
    /// Decimal point relative to the first significant digit.
    point: i32,
}

impl Decimal {
    fn zero(&self) -> bool {
        self.digits == b"0"
    }

    fn from_double(value: f64) -> Self {
        // Every finite binary64 fraction terminates within1074 decimal places.
        // This constant exact expansion avoids host rounding mode and log10
        // boundary guesses; its allocation is at most309+1+1074 characters.
        let exact = format!("{:.1074}", value.abs());
        let point = exact.find('.').expect("fixed decimal point") as i32;
        let mut digits: Vec<u8> = exact.bytes().filter(|byte| *byte != b'.').collect();
        let Some(first) = digits.iter().position(|digit| *digit != b'0') else {
            // The classic formatter uses point0 for zero, observable in %#g.
            return Self {
                digits: vec![b'0'],
                point: 0,
            };
        };
        let point = point - first as i32;
        digits.drain(..first);
        while digits.last() == Some(&b'0') {
            digits.pop();
        }
        let mut decimal = Self { digits, point };
        decimal.round(17);
        decimal
    }

    /// Round the magnitude half away from zero at a significant-digit boundary.
    fn round(&mut self, keep: i32) {
        if self.zero() {
            return;
        }
        if keep <= 0 {
            if keep == 0 && self.digits[0] >= b'5' {
                self.digits = vec![b'1'];
                self.point += 1;
            } else {
                self.digits = vec![b'0'];
                self.point = 0;
            }
            return;
        }
        let keep = keep as usize;
        if self.digits.len() <= keep {
            return;
        }
        let up = self.digits[keep] >= b'5';
        self.digits.truncate(keep);
        if up {
            for digit in self.digits.iter_mut().rev() {
                if *digit != b'9' {
                    *digit += 1;
                    return;
                }
                *digit = b'0';
            }
            self.digits[0] = b'1';
            self.point += 1;
        }
    }

    fn digit(&self, index: i32) -> u8 {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.digits.get(i))
            .copied()
            .unwrap_or(b'0')
    }

    fn fixed(&self, precision: usize, alternate: bool) -> String {
        let mut result = String::new();
        if self.point <= 0 {
            result.push('0');
        } else {
            for index in 0..self.point {
                result.push(char::from(self.digit(index)));
            }
        }
        if precision != 0 || alternate {
            result.push('.');
            for index in 0..precision as i32 {
                result.push(char::from(self.digit(self.point + index)));
            }
        }
        result
    }

    fn scientific(&self, precision: usize, alternate: bool, upper: bool) -> String {
        let mut result = String::from(char::from(self.digit(0)));
        if precision != 0 || alternate {
            result.push('.');
            for index in 1..=precision as i32 {
                result.push(char::from(self.digit(index)));
            }
        }
        let exponent = if self.zero() { 0 } else { self.point - 1 };
        result.push(if upper { 'E' } else { 'e' });
        result.push(if exponent < 0 { '-' } else { '+' });
        result.push_str(&format!("{:03}", exponent.unsigned_abs()));
        result
    }
}

fn trim_fraction(result: &mut String) {
    let exponent = result.find(['e', 'E']).unwrap_or(result.len());
    if result[..exponent].contains('.') {
        let mut end = exponent;
        while result.as_bytes()[end - 1] == b'0' {
            end -= 1;
        }
        if result.as_bytes()[end - 1] == b'.' {
            end -= 1;
        }
        result.replace_range(end..exponent, "");
    }
}

pub(super) fn format(
    value: f64,
    conversion: u8,
    precision: Option<usize>,
    alternate: bool,
) -> Result<String, Error> {
    if !value.is_finite() {
        return Err(Error::Unsupported(
            "non-finite CRT floating output is outside the VC80 profile",
        ));
    }
    let precision = precision.unwrap_or(6);
    if precision > PRECISION_LIMIT {
        return Err(Error::Unsupported(
            "CRT floating precision exceeds the bounded64-digit profile",
        ));
    }
    let mut decimal = Decimal::from_double(value);
    let result = match conversion {
        b'f' => {
            decimal.round(decimal.point + precision as i32);
            decimal.fixed(precision, alternate)
        }
        b'e' | b'E' => {
            decimal.round(precision as i32 + 1);
            decimal.scientific(precision, alternate, conversion == b'E')
        }
        b'g' | b'G' => {
            let precision = precision.max(1);
            decimal.round(precision as i32);
            let exponent = decimal.point - 1;
            let mut result = if !decimal.zero() && (exponent < -4 || exponent >= precision as i32) {
                decimal.scientific(precision - 1, alternate, conversion == b'G')
            } else {
                decimal.fixed(
                    (precision as i32 - decimal.point).max(0) as usize,
                    alternate,
                )
            };
            if !alternate {
                trim_fraction(&mut result);
            }
            result
        }
        _ => return Err(Error::Invalid("invalid floating conversion")),
    };
    if result.len() > LIMIT {
        return Err(Error::Unsupported(
            "floating output exceeds the bounded formatter profile",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
