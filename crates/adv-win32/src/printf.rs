//! printf-family formatting over guest arguments (wsprintf, and PAL's debug printf).
//!
//! Strings are byte strings in the guest code page; `%S`/`%ls` read UTF-16 and are
//! converted with the codec. Floating point (`%f %e %g`) reads a double (two stack slots),
//! which wsprintf itself never does but C printf does.

use adv_cpu::backend::{Guest, MemFault};

use crate::util::{cstr, wstr};
use adv_os::Codec;

/// Supplies successive 32-bit argument slots.
pub trait Args {
    fn next(&mut self) -> u32;
}

/// Arguments on the guest stack starting at `addr`.
pub struct StackArgs<'a> {
    pub cpu: &'a dyn Guest,
    pub addr: u32,
}

impl Args for StackArgs<'_> {
    fn next(&mut self) -> u32 {
        let mut b = [0u8; 4];
        let _ = self.cpu.read(self.addr, &mut b);
        self.addr += 4;
        u32::from_le_bytes(b)
    }
}

pub fn format(cpu: &dyn Guest, codec: &Codec, fmt: &[u8], args: &mut dyn Args) -> Vec<u8> {
    format_units(cpu, codec, fmt, args, false).unwrap_or_default()
}

pub const WINDOWS_LIMIT: usize = 1024;
pub fn windows_a(
    cpu: &dyn Guest,
    codec: &Codec,
    fmt: &[u8],
    args: &mut dyn Args,
) -> Result<Vec<u8>, MemFault> {
    format_units(cpu, codec, fmt, args, true)
}
pub fn windows_w(
    cpu: &dyn Guest,
    codec: &Codec,
    fmt: &[u16],
    args: &mut dyn Args,
) -> Result<Vec<u16>, MemFault> {
    format_units(cpu, codec, fmt, args, true)
}
trait Unit: Copy + Eq + From<u8> {
    const WIDE: bool;
    fn byte(self) -> u8;
    fn lead(self, codec: &Codec) -> bool;
    fn narrow(codec: &Codec, bytes: &[u8]) -> Vec<Self>;
    fn wide(codec: &Codec, units: &[u16]) -> Vec<Self>;
}
impl Unit for u8 {
    const WIDE: bool = false;
    fn byte(self) -> u8 {
        self
    }
    fn lead(self, codec: &Codec) -> bool {
        codec.is_lead_byte(0, self)
    }
    fn narrow(_codec: &Codec, bytes: &[u8]) -> Vec<Self> {
        bytes.to_vec()
    }
    fn wide(codec: &Codec, units: &[u16]) -> Vec<Self> {
        codec.from_utf16(0, units).0
    }
}
impl Unit for u16 {
    const WIDE: bool = true;
    fn byte(self) -> u8 {
        u8::try_from(self).unwrap_or(0xff)
    }
    fn lead(self, _codec: &Codec) -> bool {
        false
    }
    fn narrow(codec: &Codec, bytes: &[u8]) -> Vec<Self> {
        codec.to_utf16(0, bytes)
    }
    fn wide(_codec: &Codec, units: &[u16]) -> Vec<Self> {
        units.to_vec()
    }
}
trait ReadUnit: Default + PartialEq {
    const SIZE: u32;
    fn read(cpu: &dyn Guest, pointer: u32) -> Result<Self, MemFault>
    where
        Self: Sized;
}
impl ReadUnit for u8 {
    const SIZE: u32 = 1;
    fn read(cpu: &dyn Guest, pointer: u32) -> Result<Self, MemFault> {
        adv_abi::GuestPtr::<Self>::new(pointer).read(cpu)
    }
}
impl ReadUnit for u16 {
    const SIZE: u32 = 2;
    fn read(cpu: &dyn Guest, pointer: u32) -> Result<Self, MemFault> {
        adv_abi::GuestPtr::<Self>::new(pointer).read(cpu)
    }
}
fn checked_string<T: ReadUnit>(
    cpu: &dyn Guest,
    pointer: u32,
    count: usize,
) -> Result<Vec<T>, MemFault> {
    let mut result = Vec::new();
    for index in 0..count {
        let address = pointer
            .checked_add(index as u32 * T::SIZE)
            .ok_or(MemFault {
                addr: pointer,
                len: T::SIZE,
            })?;
        let value = T::read(cpu, address)?;
        if value == T::default() {
            break;
        }
        result.push(value);
    }
    Ok(result)
}
fn units<T: Unit>(bytes: &[u8]) -> Vec<T> {
    bytes.iter().copied().map(T::from).collect()
}
fn pad_units<T: Unit>(text: &[T], width: usize, left: bool) -> Vec<T> {
    let padding = vec![T::from(b' '); width.saturating_sub(text.len())];
    if left {
        [text, &padding].concat()
    } else {
        [&padding, text].concat()
    }
}
fn format_units<T: Unit>(
    cpu: &dyn Guest,
    codec: &Codec,
    fmt: &[T],
    args: &mut dyn Args,
    windows: bool,
) -> Result<Vec<T>, MemFault> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() && (!windows || out.len() < WINDOWS_LIMIT) {
        let c = fmt[i];
        if c != T::from(b'%') {
            // Copy DBCS characters whole so a trail byte of 0x25 isn't taken for '%'.
            let n = if c.lead(codec) && i + 1 < fmt.len() {
                2
            } else {
                1
            };
            out.extend_from_slice(&fmt[i..i + n]);
            i += n;
            continue;
        }
        i += 1;
        let (mut left, mut zero, mut plus, mut space, mut alt) =
            (false, false, false, false, false);
        while let Some(&f) = fmt.get(i) {
            match f.byte() {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' if !windows => plus = true,
                b' ' if !windows => space = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let num = |i: &mut usize, args: &mut dyn Args| -> Option<usize> {
            if !windows && fmt.get(*i) == Some(&T::from(b'*')) {
                *i += 1;
                return Some(args.next() as i32 as usize);
            }
            let s = *i;
            let mut value = 0usize;
            while fmt.get(*i).is_some_and(|unit| unit.byte().is_ascii_digit()) {
                value = value
                    .saturating_mul(10)
                    .saturating_add(usize::from(fmt[*i].byte() - b'0'));
                if windows {
                    value = value.min(WINDOWS_LIMIT + 1);
                }
                *i += 1;
            }
            (*i > s).then_some(value)
        };
        let width = num(&mut i, args).unwrap_or(0);
        let prec = if fmt.get(i) == Some(&T::from(b'.')) {
            i += 1;
            Some(num(&mut i, args).unwrap_or(0))
        } else {
            None
        };
        let (mut long, mut wide, mut ll, mut short) = (false, None, false, false);
        loop {
            match fmt.get(i).map(|unit| unit.byte()) {
                Some(b'l') => {
                    if long {
                        ll = true;
                    }
                    long = true;
                    wide = Some(true);
                }
                Some(b'h') => {
                    wide = Some(false);
                    short = true;
                }
                Some(b'I')
                    if fmt.get(i + 1).is_some_and(|unit| unit.byte() == b'6')
                        && fmt.get(i + 2).is_some_and(|unit| unit.byte() == b'4') =>
                {
                    ll = true;
                    i += 2;
                }
                Some(b'I') if windows => {} // Pointer-sized modifier is 32-bit on x86.
                Some(b'w') => wide = Some(true),
                _ => break,
            }
            i += 1;
        }
        let Some(&raw_conversion) = fmt.get(i) else {
            break;
        };
        let conv = raw_conversion.byte();
        i += 1;
        let remaining = WINDOWS_LIMIT.saturating_sub(out.len());
        let numeric_left = left || (windows && width > remaining);
        let body: Vec<T> = match conv {
            b'%' => vec![T::from(b'%')],
            b'c' | b'C' => {
                let value = args.next();
                let use_wide = if windows {
                    wide.unwrap_or(T::WIDE == (conv == b'c'))
                } else {
                    conv == b'C'
                };
                let character = if use_wide {
                    T::wide(codec, &[value as u16])
                } else {
                    T::narrow(codec, &[value as u8])
                };
                if windows {
                    pad_units(&character, width, left)
                } else {
                    character
                }
            }
            b'd' | b'i' => {
                let v = if ll {
                    let lo = args.next();
                    (u64::from(args.next()) << 32 | u64::from(lo)) as i64
                } else {
                    let value = args.next();
                    if windows && short {
                        i64::from(value as i16)
                    } else {
                        i64::from(value as i32)
                    }
                };
                let mut s = v.unsigned_abs().to_string();
                if let Some(p) = prec {
                    while s.len() < p {
                        if windows && p > remaining {
                            s.push('0');
                        } else {
                            s.insert(0, '0');
                        }
                    }
                }
                let sign = if v < 0 {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                units(&pad_num(
                    sign,
                    &s,
                    width,
                    numeric_left,
                    zero && prec.is_none(),
                ))
            }
            b'u' | b'x' | b'X' | b'o' if !windows || conv != b'o' => {
                let v = if ll {
                    let lo = args.next();
                    u64::from(args.next()) << 32 | u64::from(lo)
                } else {
                    u64::from(args.next())
                };
                let mut s = match conv {
                    b'u' => v.to_string(),
                    b'x' => format!("{v:x}"),
                    b'X' => format!("{v:X}"),
                    _ => format!("{v:o}"),
                };
                if let Some(p) = prec {
                    while s.len() < p {
                        if windows && p > remaining {
                            s.push('0');
                        } else {
                            s.insert(0, '0');
                        }
                    }
                }
                let prefix = match (alt && (windows || v != 0), conv) {
                    (true, b'x') => "0x",
                    (true, b'X') => "0X",
                    (true, b'o') => "0",
                    _ => "",
                };
                units(&pad_num(
                    prefix,
                    &s,
                    width,
                    numeric_left,
                    zero && prec.is_none(),
                ))
            }
            b'p' => units(&pad_num(
                "",
                &format!("{:08X}", args.next()),
                width,
                numeric_left,
                false,
            )),
            b's' | b'S' => {
                let p = args.next();
                // In wsprintfA, %s is ANSI and %S/%ls wide.
                let use_wide = if windows {
                    wide.unwrap_or(T::WIDE == (conv == b's'))
                } else {
                    conv == b'S' || wide == Some(true)
                };
                let mut s = if p == 0 {
                    if windows {
                        Vec::new()
                    } else {
                        units(b"(null)")
                    }
                } else if use_wide {
                    let text = if windows {
                        checked_string::<u16>(cpu, p, prec.unwrap_or(remaining).min(remaining))?
                    } else {
                        wstr(cpu, p)
                    };
                    T::wide(codec, &text)
                } else {
                    let text = if windows {
                        let limit = prec.unwrap_or(remaining).min(remaining);
                        checked_string::<u8>(cpu, p, if T::WIDE { limit * 4 } else { limit })?
                    } else {
                        cstr(cpu, p)
                    };
                    T::narrow(codec, &text)
                };
                if let Some(pr) = prec {
                    s.truncate(pr);
                }
                pad_units(&s, width, left)
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' if !windows => {
                let lo = args.next();
                let v = f64::from_bits(u64::from(args.next()) << 32 | u64::from(lo));
                let p = prec.unwrap_or(6);
                let s = match conv {
                    b'f' | b'F' => format!("{v:.p$}"),
                    b'e' => format!("{v:.p$e}"),
                    b'E' => format!("{v:.p$E}"),
                    _ => format!("{v}"),
                };
                let (sign, digits) = s.strip_prefix('-').map_or(("", s.as_str()), |d| ("-", d));
                units(&pad_num(sign, digits, width, left, zero))
            }
            _ => {
                // Unknown conversion: emit it verbatim.
                if windows {
                    vec![raw_conversion]
                } else {
                    vec![T::from(b'%'), raw_conversion]
                }
            }
        };
        out.extend_from_slice(&body);
        if windows {
            out.truncate(WINDOWS_LIMIT);
        }
    }
    Ok(out)
}

fn pad(s: &[u8], width: usize, left: bool, fill: u8) -> Vec<u8> {
    if s.len() >= width {
        return s.to_vec();
    }
    let padding = vec![fill; width - s.len()];
    if left {
        [s, &padding].concat()
    } else {
        [&padding, s].concat()
    }
}

fn pad_num(prefix: &str, digits: &str, width: usize, left: bool, zero: bool) -> Vec<u8> {
    let len = prefix.len() + digits.len();
    if zero && !left && len < width {
        return [
            prefix.as_bytes(),
            &vec![b'0'; width - len],
            digits.as_bytes(),
        ]
        .concat();
    }
    pad(format!("{prefix}{digits}").as_bytes(), width, left, b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    struct V(Vec<u32>);
    impl Args for V {
        fn next(&mut self) -> u32 {
            if self.0.is_empty() {
                0
            } else {
                self.0.remove(0)
            }
        }
    }

    struct NoMem;
    impl Guest for NoMem {
        fn push_f32(&mut self, _: f32) {
            unreachable!()
        }
        fn reg(&self, _: adv_cpu::backend::Reg) -> u32 {
            0
        }
        fn set_reg(&mut self, _: adv_cpu::backend::Reg, _: u32) {}
        fn read(&self, addr: u32, _: &mut [u8]) -> Result<(), adv_cpu::backend::MemFault> {
            Err(adv_cpu::backend::MemFault { addr, len: 0 })
        }
        fn write(&mut self, addr: u32, _: &[u8]) -> Result<(), adv_cpu::backend::MemFault> {
            Err(adv_cpu::backend::MemFault { addr, len: 0 })
        }
        fn map(
            &mut self,
            _: u32,
            _: u32,
            _: adv_cpu::backend::Prot,
        ) -> Result<(), adv_cpu::backend::MemFault> {
            Ok(())
        }
        fn protect(
            &mut self,
            _: u32,
            _: u32,
            _: adv_cpu::backend::Prot,
        ) -> Result<(), adv_cpu::backend::MemFault> {
            Ok(())
        }
        fn unmap(&mut self, _: u32, _: u32) -> Result<(), adv_cpu::backend::MemFault> {
            Ok(())
        }
    }

    fn f(fmt: &str, args: &[u32]) -> String {
        String::from_utf8(format(
            &NoMem,
            &Codec::new(932),
            fmt.as_bytes(),
            &mut V(args.to_vec()),
        ))
        .unwrap()
    }

    #[test]
    fn integers() {
        assert_eq!(
            f("%d|%5d|%-5d|%05d", &[-3i32, 42, 42, -42].map(|v| v as u32)),
            "-3|   42|42   |-0042"
        );
        assert_eq!(
            f("%x %X %08x %#x", &[255, 255, 0xbeef, 16]),
            "ff FF 0000beef 0x10"
        );
        assert_eq!(f("%u%%%c", &[u32::MAX, b'A' as u32]), "4294967295%A");
        assert_eq!(f("%.3d %I64d", &[7, 1, 1]), "007 4294967297");
    }

    #[test]
    fn floats() {
        let v = 1.5f64.to_bits();
        assert_eq!(f("%.2f", &[v as u32, (v >> 32) as u32]), "1.50");
    }
}
