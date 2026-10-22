//! Locale-aware Automation number syntax and exact decimal rounding without dependencies.
use super::*;
#[derive(Clone)]
pub(super) struct Number {
    negative: bool,
    digits: String,
    exponent: i64,
    fraction: i64,
}
impl Number {
    pub fn parse(text: &str, locale: Locale) -> Result<Self, u32> {
        let mut text = text.trim();
        let mut negative = false;
        let mut signed = false;
        let mut currency = false;
        let symbol = crate::kernel32::locale_info(locale, 0x14).ok_or(INVALID)?;
        // Monetary signs can surround a signed or parenthesized numeric value.
        for _ in 0..4 {
            let before = text;
            if !currency {
                if let Some(rest) = text.strip_prefix(&symbol) {
                    text = rest.trim();
                    currency = true;
                } else if let Some(rest) = text.strip_suffix(&symbol) {
                    text = rest.trim();
                    currency = true;
                } else if locale.id & 0x3ff == 4 || locale.id & 0x3ff == 0x11 {
                    if let Some(rest) = text.strip_prefix('￥') {
                        text = rest.trim();
                        currency = true;
                    } else if let Some(rest) = text.strip_suffix('￥') {
                        text = rest.trim();
                        currency = true;
                    }
                }
            }
            if !signed {
                if text.starts_with('(') && text.ends_with(')') {
                    text = text[1..text.len() - 1].trim();
                    negative = true;
                    signed = true;
                } else if let Some(rest) = text.strip_prefix('-') {
                    text = rest.trim();
                    negative = true;
                    signed = true;
                } else if let Some(rest) = text.strip_prefix('+') {
                    text = rest.trim();
                    signed = true;
                } else if let Some(rest) = text.strip_suffix('-') {
                    text = rest.trim();
                    negative = true;
                    signed = true;
                } else if let Some(rest) = text.strip_suffix('+') {
                    text = rest.trim();
                    signed = true;
                }
            }
            if text == before {
                break;
            }
        }
        if text.is_empty() {
            return Err(MISMATCH);
        }
        if text.starts_with("&H")
            || text.starts_with("&h")
            || text.starts_with("&O")
            || text.starts_with("&o")
        {
            let radix = if text.as_bytes()[1].eq_ignore_ascii_case(&b'h') {
                16
            } else {
                8
            };
            let value = u128::from_str_radix(&text[2..], radix).map_err(|_| MISMATCH)?;
            return Ok(Self {
                negative,
                digits: value.to_string(),
                exponent: 0,
                fraction: 0,
            });
        }
        let mut digits = String::new();
        let mut fraction = 0;
        let mut dot = false;
        let mut last_digit = false;
        let mut at = 0;
        let bytes = text.as_bytes();
        while at < bytes.len() {
            match bytes[at] {
                digit @ b'0'..=b'9' => {
                    digits.push(digit as char);
                    fraction += i64::from(dot);
                    last_digit = true;
                }
                b'.' if !dot => {
                    dot = true;
                    last_digit = false;
                }
                b',' if !dot && last_digit && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) => {
                    last_digit = false;
                }
                b'e' | b'E' => break,
                _ => return Err(MISMATCH),
            }
            at += 1;
        }
        if digits.is_empty() {
            return Err(MISMATCH);
        }
        let exponent = if at < bytes.len() {
            let value = &text[at + 1..];
            let raw = value.strip_prefix(['+', '-']).unwrap_or(value);
            if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err(MISMATCH);
            }
            value.parse::<i64>().unwrap_or(if value.starts_with('-') {
                i64::MIN
            } else {
                i64::MAX
            })
        } else {
            0
        };
        let digits = digits.trim_start_matches('0');
        Ok(Self {
            negative,
            digits: if digits.is_empty() {
                "0".into()
            } else {
                digits.into()
            },
            exponent,
            fraction,
        })
    }
    pub fn nonzero(&self) -> bool {
        self.digits != "0"
    }
    pub fn from_integer(value: i128) -> Self {
        Self {
            negative: value < 0,
            digits: value.unsigned_abs().to_string(),
            exponent: 0,
            fraction: 0,
        }
    }
    pub fn from_currency(value: i64) -> Self {
        let mut value = Self::from_integer(i128::from(value));
        value.fraction = 4;
        value
    }
    pub fn floating(&self) -> Result<f64, u32> {
        let exponent = self.exponent.saturating_sub(self.fraction);
        let number = format!(
            "{}{}e{exponent}",
            if self.negative { "-" } else { "" },
            self.digits
        )
        .parse::<f64>()
        .map_err(|_| OVERFLOW)?;
        if number.is_finite() {
            Ok(number)
        } else {
            Err(OVERFLOW)
        }
    }
    pub fn fixed(&self, scale: i64) -> Result<i128, u32> {
        if self.digits == "0" {
            return Ok(0);
        }
        let shift = self
            .exponent
            .saturating_sub(self.fraction)
            .saturating_add(scale);
        let magnitude = if shift >= 0 {
            if shift > 38 || self.digits.len() as i64 + shift > 38 {
                return Err(OVERFLOW);
            }
            let number = self.digits.parse::<u128>().map_err(|_| OVERFLOW)?;
            number
                .checked_mul(10u128.pow(shift as u32))
                .ok_or(OVERFLOW)?
        } else {
            let discard = shift.unsigned_abs();
            let length = self.digits.len() as u64;
            let keep = length.saturating_sub(discard) as usize;
            if keep > 38 {
                return Err(OVERFLOW);
            }
            let mut integer = if keep == 0 {
                0
            } else {
                self.digits[..keep].parse::<u128>().map_err(|_| OVERFLOW)?
            };
            if discard <= length {
                let rest = &self.digits[keep..];
                let first = rest.as_bytes()[0];
                if first > b'5'
                    || first == b'5'
                        && (rest.as_bytes()[1..].iter().any(|&b| b != b'0') || integer & 1 != 0)
                {
                    integer = integer.checked_add(1).ok_or(OVERFLOW)?;
                }
            }
            integer
        };
        let magnitude = i128::try_from(magnitude).map_err(|_| OVERFLOW)?;
        Ok(if self.negative { -magnitude } else { magnitude })
    }
    pub fn text(&self) -> String {
        let sign = if self.negative && self.digits != "0" {
            "-"
        } else {
            ""
        };
        let shift = self.exponent.saturating_sub(self.fraction);
        if (0..32).contains(&shift) {
            return format!("{sign}{}{}", self.digits, "0".repeat(shift as usize));
        }
        if (-32..0).contains(&shift) {
            let places = (-shift) as usize;
            let mut value = if places >= self.digits.len() {
                format!(
                    "0.{}{}",
                    "0".repeat(places - self.digits.len()),
                    self.digits
                )
            } else {
                let split = self.digits.len() - places;
                format!("{}.{}", &self.digits[..split], &self.digits[split..])
            };
            while value.ends_with('0') {
                value.pop();
            }
            if value.ends_with('.') {
                value.pop();
            }
            return format!("{sign}{value}");
        }
        format!("{sign}{}E{shift}", self.digits)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_numeric_syntax_and_exact_half_even_currency_rounding() {
        let locale = Locale {
            id: 0x409,
            name: "en-US",
            cp: 1252,
        };
        for (text, float, currency) in [
            ("1,234.5", 1234.5, 12345000),
            ("(12.5)", -12.5, -125000),
            ("1e3", 1000.0, 10000000),
            ("&H10", 16.0, 160000),
            ("$1.5", 1.5, 15000),
            ("0.00005", 0.00005, 0),
            ("0.00015", 0.00015, 2),
        ] {
            let number = Number::parse(text, locale).unwrap();
            assert_eq!(number.floating().unwrap(), float);
            assert_eq!(number.fixed(4).unwrap(), currency);
        }
        assert_eq!(
            Number::parse("922337203685477.5807", locale)
                .unwrap()
                .fixed(4)
                .unwrap(),
            i128::from(i64::MAX)
        );
        assert_eq!(
            Number::parse("-922337203685477.5808", locale)
                .unwrap()
                .fixed(4)
                .unwrap(),
            i128::from(i64::MIN)
        );
        for (text, value) in [
            ("0.5", 0),
            ("1.5", 2),
            ("2.5", 2),
            ("-0.5", 0),
            ("-1.5", -2),
            ("1.49999999999999999", 1),
        ] {
            assert_eq!(
                Number::parse(text, locale).unwrap().fixed(0).unwrap(),
                value
            );
        }
    }
}
