//! printf-family formatting over guest arguments (wsprintf, and PAL's debug printf).
//!
//! Strings are byte strings in the guest code page; `%S`/`%ls` read UTF-16 and are
//! converted with the codec. Floating point (`%f %e %g`) reads a double (two stack slots),
//! which wsprintf itself never does but C printf does.

use adv_cpu::backend::Guest;

use crate::Codec;
use crate::util::{cstr, wstr};

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
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        if c != b'%' {
            // Copy DBCS characters whole so a trail byte of 0x25 isn't taken for '%'.
            let n = if codec.is_lead_byte(0, c) && i + 1 < fmt.len() {
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
            match f {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let num = |i: &mut usize, args: &mut dyn Args| -> Option<usize> {
            if fmt.get(*i) == Some(&b'*') {
                *i += 1;
                return Some(args.next() as i32 as usize);
            }
            let s = *i;
            while fmt.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            (*i > s).then(|| {
                std::str::from_utf8(&fmt[s..*i])
                    .unwrap()
                    .parse()
                    .unwrap_or(0)
            })
        };
        let width = num(&mut i, args).unwrap_or(0);
        let prec = if fmt.get(i) == Some(&b'.') {
            i += 1;
            Some(num(&mut i, args).unwrap_or(0))
        } else {
            None
        };
        let (mut long, mut wide, mut ll) = (false, false, false);
        loop {
            match fmt.get(i) {
                Some(b'l') => {
                    if long {
                        ll = true;
                    }
                    long = true;
                    wide = true;
                }
                Some(b'h') => wide = false,
                Some(b'I') if fmt.get(i + 1..i + 3) == Some(b"64") => {
                    ll = true;
                    i += 2;
                }
                Some(b'w') => wide = true,
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        let body: Vec<u8> = match conv {
            b'%' => b"%".to_vec(),
            b'c' => vec![args.next() as u8],
            b'C' => codec.from_utf16(0, &[args.next() as u16]).0,
            b'd' | b'i' => {
                let v = if ll {
                    let lo = args.next();
                    (u64::from(args.next()) << 32 | u64::from(lo)) as i64
                } else {
                    i64::from(args.next() as i32)
                };
                let mut s = v.unsigned_abs().to_string();
                if let Some(p) = prec {
                    while s.len() < p {
                        s.insert(0, '0');
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
                pad_num(sign, &s, width, left, zero && prec.is_none())
            }
            b'u' | b'x' | b'X' | b'o' => {
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
                        s.insert(0, '0');
                    }
                }
                let prefix = match (alt && v != 0, conv) {
                    (true, b'x') => "0x",
                    (true, b'X') => "0X",
                    (true, b'o') => "0",
                    _ => "",
                };
                pad_num(prefix, &s, width, left, zero && prec.is_none())
            }
            b'p' => pad_num("", &format!("{:08X}", args.next()), width, left, false),
            b's' | b'S' => {
                let p = args.next();
                // In wsprintfA, %s is ANSI and %S/%ls wide.
                let use_wide = conv == b'S' || wide;
                let mut s = if p == 0 {
                    b"(null)".to_vec()
                } else if use_wide {
                    codec.from_utf16(0, &wstr(cpu, p)).0
                } else {
                    cstr(cpu, p)
                };
                if let Some(pr) = prec {
                    s.truncate(pr);
                }
                pad(&s, width, left, b' ')
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
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
                pad_num(sign, digits, width, left, zero)
            }
            other => {
                // Unknown conversion: emit it verbatim.
                vec![b'%', other]
            }
        };
        out.extend_from_slice(&body);
    }
    out
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
